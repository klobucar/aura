# Control Stream Protocol

This document describes the binary protocol used over the QUIC control stream for signaling, authentication, presence and moderation.

## Overview

The control stream is a bidirectional QUIC stream opened by the server immediately after connection. It's used for:
- Authentication handshake
- Channel join/leave
- User presence updates
- Channel, profile and account administration, and moderation
- Keepalive pings and round-trip-time probes
- Reliable audio fallback (when datagrams fail)

The control stream is the only stream the server opens as bidirectional. Other streams on the connection are used for [encrypted file transfers](file_transfer.md): client-opened bidirectional streams for uploads and server-opened unidirectional streams for deliveries.

## Framing

Every message except the bare `0x00` keepalive and the `0x02` ServerHello is framed as:

```
[type: u8] [length: u32 LE] [payload: length bytes]
```

Payloads are the protobuf messages in `crates/aura-protocol/src/aura.proto` unless the table says otherwise. Requests that get a reply (`0x40`-`0x44`, `0x48`-`0x4C`) are answered on the request's own message type.

## Message Types

| Type | Name | Direction | Payload | Description |
|------|------|-----------|---------|-------------|
| `0x00` | Keepalive | C→S | none (1 byte, unframed) | Ping to prevent timeout |
| `0x02` | ServerHello | S→C | 32-byte challenge (unframed) | Challenge for authentication |
| `0x03` | AuthRequest | C→S | `AuthRequest` | Client authentication |
| `0x04` | AuthResponse | S→C | `AuthResponse` | Auth result |
| `0x05` | Ping | C→S | 8-byte nonce | Round-trip-time probe (see [Control-Stream Ping](#control-stream-ping-0x05-0x06)) |
| `0x06` | Pong | S→C | the ping's nonce | Reply to a Ping |
| `0x10` | JoinChannel | C→S | `JoinChannelRequest` | Join (move to) a channel |
| `0x11` | UserJoined | S→C | `UserJoined` | A user joined a channel |
| `0x12` | UserLeft | S→C | `UserLeft` | A user left a channel |
| `0x13` | ChannelState | S→C | `ServerState` | Full snapshot of channels and users |
| `0x20` | AudioPacket | Both | audio packet bytes | Encapsulated audio (fallback) |
| `0x30` | TextPacket | Both | `EncryptedTextPacket` | End-to-end encrypted text message |
| `0x40` | CreateChannel | C→S, reply S→C | `CreateChannelRequest` → `CreateChannelResponse` | Create a channel (`manage_channels`) |
| `0x41` | UpdateChannel | C→S, reply S→C | `UpdateChannelRequest` → `MetadataUpdateResponse` | Edit a channel (`manage_channels`) |
| `0x42` | UpdateProfile | C→S, reply S→C | `UpdateProfile` → `MetadataUpdateResponse` | Update your own profile |
| `0x43` | DeleteChannel | C→S, reply S→C | `DeleteChannelRequest` → `AdminResponse` | Delete a channel (`manage_channels`) |
| `0x44` | DeleteUser | C→S, reply S→C | `DeleteUserRequest` → `AdminResponse` | Delete your account, or someone else's (`ban_users`) |
| `0x45` | UserStatus | Both | `UserStatusUpdate` | Mute/deafen state |
| `0x46` | ProfileUpdated | S→C | `UserProfile` | A user's profile changed |
| `0x47` | ChannelDeleted | S→C | `ChannelDeleted` | You are no longer in a channel; move to the fallback (see [Leaving a Channel Involuntarily](#leaving-a-channel-involuntarily)) |
| `0x48` | ListUsers | C→S, reply S→C | `ListUsersRequest` → `ListUsersResponse` | List accounts (any admin) |
| `0x49` | VerifyUser | C→S, reply S→C | `VerifyUserRequest` → `AdminResponse` | Verify a user (`verify_users`) |
| `0x4A` | BanUser | C→S, reply S→C | `BanUserRequest` → `AdminResponse` | Ban or unban a user (`ban_users`) |
| `0x4B` | GrantAdmin | C→S, reply S→C | `GrantAdminRequest` → `AdminResponse` | Set a user's admin permissions (`grant_admin`) |
| `0x4C` | KickUser | C→S, reply S→C | `KickUserRequest` → `AdminResponse` | Move a user out of their channel (`ban_users`) |
| `0x50` | MlsJoin | C→S | `MlsEnvelope` (key package) | Join a channel's MLS group |
| `0x51` | MlsCommitWelcome | C→S | `MlsEnvelope` (commit + welcome) | Founder adds a member |
| `0x52` | MlsCreateGroup | S→C | `MlsEnvelope` | You are the founder; create the group |
| `0x53` | MlsAddMember | S→C | `MlsEnvelope` (key package) | Founder: add this joiner |
| `0x54` | MlsCommit | S→C | `MlsEnvelope` (commit) | Apply this commit |
| `0x55` | MlsWelcome | S→C | `MlsEnvelope` (welcome) | Join with this welcome |
| `0x56` | MlsRatchetRequest | S→C | `MlsEnvelope` | Founder: advance the epoch |
| `0x57` | MlsRatchetCommit | C→S | `MlsEnvelope` (commit) | Founder's empty commit |

`0x50`-`0x5F` are reserved for MLS signalling.

### Unknown Message Types

The server skips a frame whose type it doesn't know, reading its length prefix and discarding its payload (up to 1 MiB, the largest frame it accepts), and carries on. Older servers ignored only the type byte and then read the length prefix and payload as further messages, so a client must not send a message type to a server that predates it.

## Control-Stream Ping (0x05 / 0x06)

Clients measure round-trip time with a QUIC datagram, `[0x00][8-byte nonce]`, which the server echoes. Clients that can't send datagrams (the .NET client: `System.Net.Quic` has no datagram API) probe on the control stream instead:

```
C→S  [0x05] [length = 8: u32 LE] [nonce: 8 bytes]
S→C  [0x06] [length = 8: u32 LE] [nonce: the same 8 bytes]
```

- The nonce is opaque to the server, which echoes it unchanged.
- The length must be exactly 8. Any other length is a protocol error and the server closes the connection.
- Pings are rate limited per connection: a burst of 5, then 2 per second. A ping over the limit is read and dropped without a reply, which the client sees as a lost probe; the connection stays up.
- A ping counts as activity for the 30-second idle timeout, like any other message.
- Servers that predate this message don't know `0x05`. See [Unknown Message Types](#unknown-message-types).

The reply comes back on the same ordered stream as everything else, so a Pong also tells the client that the server has handled every message sent before the Ping.

## Leaving a Channel Involuntarily

`ChannelDeleted` (`0x47`) tells a client it is no longer in `channel_id` and should move to `fallback_channel_id` (the lobby, or empty if the server has none, in which case the client picks). `reason` says why:

| `reason` | When | `channel_id` |
|----------|------|--------------|
| `CHANNEL_REMOVAL_REASON_DELETED` (0) | The channel was deleted. Sent to its occupants. | The deleted channel |
| `CHANNEL_REMOVAL_REASON_NOT_FOUND` (1) | The client sent JoinChannel for a channel that doesn't exist, typically after reconnecting into one deleted while it was away. | The channel it asked for |
| `CHANNEL_REMOVAL_REASON_KICKED` (2) | An admin kicked the client out of the channel. `detail` carries the admin's reason. The channel still exists and the client may come back. | The channel it was kicked from |

Clients that predate `reason` read every notice as a deletion, which still moves them to the fallback.

### Joining a channel that doesn't exist

A JoinChannel is a move: the server first takes the session out of its current channel (broadcasting UserLeft), then joins the new one. If the requested channel doesn't exist, the server stops there and replies with a `NOT_FOUND` notice naming the lobby as the fallback; the session is in no channel until it joins one.

The server only accepts an MLS join (`0x50`) for the channel the session is in, and ignores any other. Clients send their MLS joins straight after JoinChannel, so without this a refused join, or one that raced the channel's deletion, would bring the dead channel's MLS groups back.

## Moderation (0x48-0x4C)

The server authorises each moderation request with the account the connection logged in as, and reads that account's admin permissions from the database on every request, so a grant or revocation applies at once, even to admins who are already connected. A banned admin's requests are refused. The `session_token` fields in these messages are left over from an earlier design and are ignored.

| Type | Request | Needs |
|------|---------|-------|
| `0x48` | `ListUsersRequest { offset, limit }` | Any admin |
| `0x49` | `VerifyUserRequest { target_user_uuid }` | `verify_users` |
| `0x4A` | `BanUserRequest { target_user_uuid, banned, reason }` | `ban_users` |
| `0x4B` | `GrantAdminRequest { target_user_uuid, verify_users, ban_users, grant_admin, manage_channels }` | `grant_admin` |
| `0x4C` | `KickUserRequest { target_user_uuid, reason }` | `ban_users` |

ListUsers is answered with a `ListUsersResponse`; the others with an `AdminResponse`. A refused request has `success = false` and an `error_message` such as `Permission denied`, `You can't do this to your own account`, `Acting on another admin requires the grant_admin permission`, `User not found`, `User is not online` or `User is not in a channel they can be kicked from`. Internal failures read `Internal server error`; the details go to the server log.

### Rules for acting on another account

Ban, unban, kick, granting or revoking admin, and deleting someone else's account (`0x44`) all follow the same rules:

1. Nobody acts on their own account.
2. The target must exist.
3. Acting on an account that is itself an admin needs `grant_admin`: someone who couldn't revoke an admin can't ban, kick or delete them either.

These keep the server from locking itself out. Whoever bans, deletes or demotes an admin holds `grant_admin` and is never their own target, so at least one admin who can manage admins always remains, starting with the bootstrap admin, who is the only one at first. Deleting your own account is not moderation and needs no permission, so the one way to lose the last such admin is for them to delete their own account.

### Operations

- **ListUsers** returns accounts oldest first. A `limit` of 0 means 100; larger limits are capped at 500. `total_count` counts every account, and each `UserInfo` says whether that account is an admin.
- **VerifyUser** marks the target verified.
- **BanUser** with `banned = true` bans the target: the server closes every connection the target has open, with QUIC application error code `0x01` and reason `banned`, and refuses their logins with an AuthResponse whose `error_message` is `User is banned`. `banned = false` lifts the ban. The `reason` goes to the server log.
- **GrantAdmin** replaces the target's permissions with the ones in the request. Granting none revokes admin status. `verify_users` allows VerifyUser; `ban_users` allows BanUser, KickUser and deleting other accounts; `grant_admin` allows GrantAdmin and acting on other admins; `manage_channels` allows CreateChannel, UpdateChannel and DeleteChannel.
- **KickUser** takes each of the target's sessions that is in a channel other than the lobby out of that channel (everyone gets a UserLeft, as for any leave) and sends it a `ChannelDeleted` notice with reason `KICKED`, the lobby as the fallback and the request's `reason` as `detail`. The target can rejoin; banning is what keeps someone out. The request fails if the target isn't online, or is online only in the lobby or in no channel.

Channel management (`0x40`, `0x41`, `0x43`) needs `manage_channels`. A non-admin is told `Admin required`, which the macOS client takes as its cue to hide its admin controls; an admin without the permission is told `Channel management permission required`.

## Connection Close Codes

The server closes a connection with a QUIC application error code:

| Code | Reason | Meaning |
|------|--------|---------|
| `0x00` | (empty) | Protocol error, refused login, or normal shutdown |
| `0x01` | `banned` | The account was banned while connected. Logging in again is refused until it is unbanned. |

## Message Formats

> **Note:** several layouts below (`0x03`, `0x04`, `0x10`-`0x13`, `0x21`, `0x30`) describe an earlier binary encoding. The current payloads are the protobuf messages listed in [Message Types](#message-types), and the server no longer handles `0x21`.

### 0x00 - Keepalive
```
[0x00]
```
Single byte. Sent every 10s by client. Server responds to datagrams, not stream pings.

### 0x02 - ServerHello (Challenge)
```
[0x02] [challenge: 32 bytes]
```
Server sends 32 random bytes for client to sign.

### 0x03 - AuthRequest
```
[0x03]
[public_key_len: u8] [public_key: bytes]
[display_name_len: u8] [display_name: UTF-8]
[signature_len: u8] [signature: bytes]
[challenge_len: u8] [challenge: bytes]
[password_len: u8] [password: UTF-8] (optional)
```

### 0x04 - AuthResponse
```
[0x04]
[success: u8] (0 = fail, 1 = success)
[user_id: u32 LE]
[token_len: u8] [token: UTF-8]
[verified: u8]
[error_len: u8] [error: UTF-8]
```

### 0x10 - JoinChannel
```
[0x10] 
[channel_id: u32 LE]
[key_package_len: u16 LE] [key_package: bytes]
```
Client sends their initial MLS KeyPackage to join the group.

### 0x11 - UserJoined
```
[0x11]
[channel_id: u32 LE]
[session_id: u32 LE]
[display_name_len: u8] [display_name: UTF-8]
[key_package_len: u16 LE] [key_package: bytes]
```
Broadcast to channel members. Includes KeyPackage for group inclusion.
Sent to **ALL connected users** when a user joins any channel.

### 0x12 - UserLeft
```
[0x12]
[channel_id: u32 LE]
[session_id: u32 LE]
```
Sent to **ALL connected users** when a user leaves any channel or disconnects.

### 0x13 - ChannelState
```
[0x13]
[channel_id: u32 LE]
[user_count: u8]
[users: repeated {
    [session_id: u32 LE]
    [display_name_len: u8] [display_name: UTF-8]
    [key_package_len: u16 LE] [key_package: bytes]
}]
```
Sent to new joiners with the full list of users and their MLS KeyPackages.

### 0x42 - UpdateProfile
```
[0x42]
[length: u32 LE] [UpdateProfile protobuf]
```
Replies with a `MetadataUpdateResponse` on `0x42`. `profile.user_id` must be the sender's own user id (otherwise `Cannot update other profiles`). Only `bio` and `avatar_data` are taken from the request:

- `display_name` is ignored, not rejected. The server stores and broadcasts the account's own name (the one validated at login), so a profile update cannot rename anyone.
- `avatar_data` must be empty (no avatar), a PNG (`89 50 4E 47 0D 0A 1A 0A`) or a JPEG (`FF D8 FF`), and at most 128 KiB. Otherwise the reply is `Avatar is larger than 128 KB` or `Avatar must be a PNG or JPEG image`.
- `bio` is at most 16 KiB (16384 bytes of UTF-8). Otherwise the reply is `Bio is longer than 16 KB`.

On success every session receives `ProfileUpdated` (`0x46`) with the stored profile. `signature` and `signing_key` are stored as sent and are not verified by the server.

### 0x21 - MlsSignal
```
[0x21]
[channel_id: u32 LE]
[signal_type: u8] (0=Welcome, 1=Commit, 2=Proposal)
[payload_len: u32 LE]
[payload: bytes]
```
Transports opaque MLS messages. `Welcome` is sent by an existing member to a joiner; `Commit` and `Proposal` are broadcast.

### 0x30 - TextPacket
```
[0x30]
[packet_len: u32 LE]
[sender_session_id: u32 LE]
[channel_id: u32 LE]
[epoch: u64 LE]
[message_id_len: u8] [message_id: UTF-8]
[content_len: u32 LE] [content: encrypted bytes]
[nonce: 24 bytes]
[tag: 16 bytes]
[reply_to_id_len: u8] [reply_to_id: UTF-8] (optional)
```
End-to-end encrypted text message. Payload is XChaCha20-Poly1305.

A text message can announce an end-to-end encrypted file (`TextMessage.attachment`). The file's bytes never use the control stream: they go on a dedicated QUIC stream per transfer. See [file_transfer.md](file_transfer.md).

## Connection Flow

```
Client                              Server
  |                                    |
  |  -------- QUIC Connect --------->  |
  |                                    |
  |  <------- ServerHello (0x02) ----  |
  |           [challenge: 32 bytes]    |
  |                                    |
  |  -------- AuthRequest (0x03) --->  |
  |           [pubkey, name, sig...]   |
  |                                    |
  |  <------- AuthResponse (0x04) ---  |
  |           [success, user_id...]    |
  |                                    |
  |  -------- JoinChannel (0x10) --->  |
  |           [channel_id, KeyPackage] |
  |                                    |
  |  <------- ChannelState (0x13) ---  |
  |           [existing users + KPs]   |
  |                                    |
  |  <------- MlsSignal (Welcome) ---  |
  |           (from existing member)   |
  |                                    |
  |  <======= E2EE Audio/Text =======> |
  |                                    |
  |  -------- Disconnect ----------->  |
  |                                    |
  |  <------- UserLeft (0x12) -------  |
  |           (to remaining users)     |
```

## Presence System

### Join Flow
1. Client sends `JoinChannel (0x10)` with channel ID
2. Server adds client to voice group
3. Server sends `ChannelState (0x13)` to new joiner with existing users
4. Server broadcasts `UserJoined (0x11)` to all existing members

### Leave Flow
1. Client disconnects or times out
2. Server broadcasts `UserLeft (0x12)` to remaining members
3. Server removes client from voice group

### Channel Switch
When a user sends `JoinChannel` for a different channel than their current one:
1. Server broadcasts `UserLeft (0x12)` to the **old channel** members
2. Server removes user from old voice group
3. Server adds user to new voice group  
4. Server sends `ChannelState (0x13)` to the switching user with new channel's users
5. Server broadcasts `UserJoined (0x11)` to the **new channel** members

## Keepalive

- Client sends `0x00` every 10 seconds on control stream
- Server timeout is 30 seconds of inactivity
- Either audio packets OR keepalive pings count as activity
