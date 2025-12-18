# Control Stream Protocol

This document describes the binary protocol used over the QUIC control stream for signaling, authentication, and presence.

## Overview

The control stream is a bidirectional QUIC stream opened by the server immediately after connection. It's used for:
- Authentication handshake
- Channel join/leave
- User presence updates
- Keepalive pings
- Reliable audio fallback (when datagrams fail)

## Message Types

| Type | Name | Direction | Description |
|------|------|-----------|-------------|
| `0x00` | Keepalive | Both | Ping to prevent timeout |
| `0x02` | ServerHello | S→C | Challenge for authentication |
| `0x03` | AuthRequest | C→S | Client authentication |
| `0x04` | AuthResponse | S→C | Auth result |
| `0x10` | JoinChannel | C→S | Join channel (includes KeyPackage) |
| `0x11` | UserJoined | S→C | User joined (includes KeyPackage) |
| `0x12` | UserLeft | S→C | User left channel |
| `0x13` | ChannelState | S→C | Full channel user list |
| `0x20` | AudioPacket | Both | Encapsulated audio (fallback) |
| `0x21` | MlsSignal | Both | MLS Signaling (Commit, Welcome, Proposal) |
| `0x30` | TextPacket | Both | End-to-end encrypted text message |

## Message Formats

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
