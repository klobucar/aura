---
title: Security Review
description: What has been reviewed in Aura, what is still open, and what was fixed, with links to the fixes.
---
# Aura Security Review

**Status as of 23 November 2026.** This is a living document. It is updated
when a finding is opened or closed, so it reflects the code on `main`, not a
point-in-time snapshot.

**What this is, and isn't.** These are the results of the maintainer's own
reviews of the server, core library and both clients, done with AI-assisted
code review and adversarial testing. **Aura has not had an external
security audit.** Treat it as pre-alpha and keep sensitive conversations off
it. To report a vulnerability, see [`SECURITY.md`](../SECURITY.md).

For what Aura is designed to protect against, and its known structural limits,
read the [threat model](THREAT_MODEL.md) first. This document tracks where the
implementation falls short of it.

---

## At a glance

- **End-to-end encryption of voice and text holds on `main`.** Leaving a
  channel now removes you from its MLS groups
  ([A1](#a1-high-leaving-a-channel-does-not-remove-you-from-its-mls-group),
  fixed in #64).
- **Audio integrity has open gaps.**
  - Until #71 lands, the server relays audio that names another session as its
    sender, so a channel member can speak as someone else
    ([A10](#a10-medium-the-server-relays-audio-on-behalf-of-any-session)).
  - The audio header isn't authenticated, so a malicious server can replay
    earlier frames ([A14](#a14-medium-the-audio-packet-header-is-not-authenticated)).
- **The desktop client sent voice unencrypted from April to November 2026**
  ([F1](#f1-critical-desktop-voice-was-sent-unencrypted)). Fixed in #59 and #62.
  If you used the desktop client in that window, assume the server operator,
  and anyone able to read the server's memory, could have heard your voice.
- **Never use a Debug build of either client** for real conversations: Debug
  builds accept any server certificate ([A3](#a3-medium-debug-builds-accept-any-server-certificate)).
- **Moderation works over the wire** (#65): ban, kick, verify and admin grants.
  The client consoles are in review (#69).

---

## Open findings

| ID | Severity | Component | Finding | Status |
|---|---|---|---|---|
| A3 | Medium | macOS, desktop | [Debug builds accept any server certificate](#a3-medium-debug-builds-accept-any-server-certificate) | Open; release builds unaffected |
| A6 | Low | core, clients | Profiles are signed but nothing verifies the signatures (`verify_profile` exists in core; no caller) | Open |
| A7 | Low | macOS | Profile exports made before #61 hold the identity secret as **plaintext JSON**. The app now writes passphrase-encrypted `.aura-identity` backups, but old files on disk are still plaintext | Delete old exports; re-export |
| A10 | Medium | server | [The server relays audio on behalf of any session](#a10-medium-the-server-relays-audio-on-behalf-of-any-session) | Fix in review (#71) |
| A11 | Medium | core | [One forged audio frame can silence a speaker](#a11-medium-one-forged-audio-frame-can-silence-a-speaker) | Fix in review (#71) |
| A12 | Medium | server | [Profile updates can rename you live and carry unbounded data](#a12-medium-profile-updates-can-rename-you-live-and-carry-unbounded-data) | Fix in review (#70) |
| A13 | Low | core | A speaker goes silent after 65,536 frames (about 22 minutes of continuous speech), because the 16-bit audio sequence number wraps and the jitter buffer treats every later frame as already played | Fix in review (#72) |
| A14 | Medium | core, server | [The audio packet header is not authenticated](#a14-medium-the-audio-packet-header-is-not-authenticated) | In progress |

### A3. [MEDIUM] Debug builds accept any server certificate

**Where:** `clients/desktop/Services/AuraNetworkClient.cs` (`#if DEBUG … return true`)
and `clients/macos/Aura/Services/QuicNetworkClient.swift` (`#if DEBUG … completeHandler(true)`).

**What happens.** In Debug builds, server certificates are not checked at all:
no WebPKI validation, no trust-on-first-use pin. A network attacker can then
impersonate the server at the transport layer and see control-plane metadata
(who is online, channel membership, display names). Voice and text stay
end-to-end encrypted under MLS, but TOFU's protection of first contact is gone.

**Status.** Release builds keep full validation. An invite link's certificate
fingerprint is enforced in every build configuration (#61, #62). Don't
distribute Debug builds; the release runbook builds Release only.

### A10. [MEDIUM] The server relays audio on behalf of any session

**Where:** `crates/aura-server/src/connection.rs`, the audio datagram handler
and the reliable-stream audio path.

**What happens.** Fan-out follows the session id inside an audio packet. The
server never compared it with the connection the packet arrived on. Any
logged-in client could send frames naming another session, into that session's
channel and regardless of that user's mute state. Receivers decrypt with the
named sender's key, so a non-member's frames fail, but every member of the
channel's group can derive every sender's key. A member can therefore speak as
another member.

**Fix (in review, #71).** The server drops audio whose session id is not the
sending connection's own, on both paths. An end-to-end test covers it.

### A11. [MEDIUM] One forged audio frame can silence a speaker

**Where:** `crates/aura-core/src/audio_pipeline.rs`, `AudioReceiver::on_packet`.

**What happens.**
- The receiver moved its replay window before decrypting a frame. One forged
  frame with a sequence number inside the window made every genuine frame after
  it look like a replay.
- The window also started at sequence 0, so a listener whose first frame from a
  sender was past 32,768 (a sender talking for more than about 11 minutes)
  refused the stream.
- It kept the numerically largest sequence, which sticks at 65,535 once the
  counter wraps.

**Fix (in review, #71).** The window moves only after a frame authenticates,
starts empty, and advances with wrap-aware comparison. Tests cover a forged
frame and a listener joining a stream in progress.

### A12. [MEDIUM] Profile updates can rename you live and carry unbounded data

**Where:** `crates/aura-server/src/state.rs`, `update_profile_persistent`.

**What happens.** The server stored and rebroadcast the client's `UserProfile`
as sent, including `display_name`, and clients apply the broadcast name to the
roster. Anyone could therefore rename themselves live, bypassing the display
name rules (length, trimming, look-alikes and uniqueness; F8). Avatars and bios
had no limit beyond the 256 KiB control-frame cap, and clients decode avatars
with the platform's image parsers.

**Fix (in review, #70).**
- The server ignores the client's `display_name` and broadcasts the account's
  own.
- Avatars must be empty, PNG or JPEG, and at most 128 KiB.
- Bios are capped at 16 KiB.

### A14. [MEDIUM] The audio packet header is not authenticated

**Where:** `crates/aura-protocol/src/fast_header.rs` and the DAVE audio
encryption in `crates/aura-core/src/crypto.rs`.

**What happens.**
- An audio frame's session id, epoch hint and sequence number sit outside the
  ciphertext and are not associated data.
- A malicious server can replay any genuine frame from the current epoch under
  a fresh sequence number, and the receiver plays it again.
- It can also move the receiver's replay window ahead with a replayed frame.

A server can't forge new audio, or change who a frame appears to come from,
because each sender's key is distinct.

**Status (in progress).** The fix binds the header into the AEAD as associated
data. That is a wire-incompatible change, so both clients must update together.

---

## Fixed findings

Newest first. "Since" is when the problem entered `main`, where known.

| ID | Severity | Component | Finding | Fixed in |
|---|---|---|---|---|
| A1 | High | server, core, clients | [Leaving a channel did not remove you from its MLS group](#a1-high-leaving-a-channel-does-not-remove-you-from-its-mls-group) | #64 |
| A2 | Medium | server | [Any session could inject a commit into any group](#a2-medium-any-session-can-inject-a-commit-into-any-group) | #64 |
| A4 | Medium | server | No wire messages reached the existing verify / ban / grant-admin logic, and there was no kick, so abuse couldn't be handled on a live server | #65 |
| A8 | Medium | server | Any admin could delete any account, including the bootstrap admin's, with no permission or target checks | #65 |
| A5 | Low | server | A join to a channel id that doesn't exist was accepted and created a phantom group (e.g. rejoining a deleted channel after a reconnect) | #65 |
| A9 | Low | server | For an unknown control-frame type the server skipped only the type byte, so the payload was parsed as further commands on the sender's own stream | #65 |
| F1 | Critical | desktop | [Voice was sent unencrypted](#f1-critical-desktop-voice-was-sent-unencrypted) (since 2fbf9c85, Apr 2026) | #59 (stop), #62 (encrypt) |
| F2 | Medium | desktop | Unhandled control frames desynchronised the stream; anyone editing their profile (0x46) broke every desktop client's connection | #62 |
| F3 | Medium | desktop | Inline markdown parsing was quadratic; one long hostile message could freeze every desktop client in the channel | #62 |
| F4 | Medium | macOS, desktop | Commits from a channel you'd left could set your *sending* key, so your audio was encrypted for the wrong room | #61, #62 |
| F5 | Low | desktop | A reply quote could be taken from the server-visible `reply_to_id` instead of the encrypted message, so a server could forge "reply to" | #62 |
| F6 | Low | macOS | Profile export wrote the identity secret as plaintext JSON (see A7 for old files) | #60 (format), #61 |
| F7 | Low | server | Deleting an account always failed (a foreign key from `tofu_identities`), so users couldn't remove their data or release their name | #60 |
| F8 | Low | server | Display names allowed look-alike and padded names, and reserved names could be dodged with punctuation | #58 |
| F9 | Info | server | Voice groups with stable membership never ratcheted, so one media key could last for hours | #58 (15-minute ratchet) |
| F10 | High | server | Control-frame reader pre-allocated from an attacker-chosen length prefix; idle connections could exhaust memory | `704cfd73` |
| F11 | Medium | server | No rate limit on handshakes; signature-verification spam could starve live calls of CPU | `0f7c6f03` |

### A1. [HIGH] Leaving a channel does not remove you from its MLS group

**Where:** `crates/aura-server/src/state.rs` (`remove_from_voice_group`,
`remove_from_text_group`, the founder logic in the MLS join handlers).

**What happened.** Leaving a channel, by switching away or disconnecting, only
removed the session from the server's routing set. Nobody committed an MLS
Remove, so the leaver stayed a leaf of the channel's voice and text groups. Two
consequences:

1. The [threat model](THREAT_MODEL.md)'s first goal, *only current members can
   decrypt*, then rested on the server routing correctly. A malicious server
   could keep forwarding the group's commits to a departed member, who could
   keep deriving every future epoch's keys.
2. If the group's founder was the one who left, the server kept treating them
   as founder: every newcomer's KeyPackage went to them, and their client kept
   up with the group they left. If the founder disconnected instead, the next
   joiner created a *new* group while the people already in the room stayed in
   the old one, and they could no longer hear each other.

**Fix (#64).**
- The server tracks who is actually in each MLS tree, in join order.
- After any departure it sends the founder `MlsSyncMembers` (0x58), the account
  ids that should remain. The founder commits Removes for every other leaf.
  This message can only shrink a group, never grow it.
- The founder role passes to the next-longest member when the founder leaves.
- A group with nobody left restarts with the first waiting joiner instead of
  splitting.
- Clients delete a channel's group state when they leave it.

Design: [`MLS_SECURITY.md`](MLS_SECURITY.md) and
[`05_dave_protocol_deviations.md`](05_dave_protocol_deviations.md) §6.

### A2. [MEDIUM] Any session can inject a commit into any group

**Where:** `crates/aura-server/src/state.rs`, `handle_mls_ratchet_commit`
(0x57) and `handle_mls_commit_welcome` (0x51).

**What happened.** The server distributed a commit, and advanced its epoch
counter, without checking who sent it. Clients reject a commit signed by a
non-member, so a stranger couldn't change a group's keys. But a *member* who
wasn't the founder could commit alongside the founder and fork the group's
epoch, and anyone could make every member process junk and desynchronise the
server's epoch counter.

**Fix (#64).** Only the group's founder may commit; the server drops anyone
else's commit, and its Welcome, before sending anything.

### F1. [CRITICAL] Desktop voice was sent unencrypted

**Where:** `clients/desktop/Services/AuraNetworkClient.cs`, `SendAudioFrameAsync`.

**What happened.** 2fbf9c85 (10 April 2026) removed the only call that set up
the desktop client's audio sender. With no sender, encoding returned nothing,
and a fallback then sent the **raw PCM** on the control stream. The server
relayed it to the channel. Desktop voice was not end-to-end encrypted, or even
compressed, until November 2026. macOS was not affected.

**Fix.**
- #59 removed the fallback, so desktop voice went silent instead of plaintext.
- #62 sets the sender up after login and blocks sending until the channel's MLS
  voice key arrives. The block resets on every channel switch and disconnect.

**Lesson.** A "send it anyway" fallback on a crypto path turns any setup bug
into a confidentiality failure. Fail closed.

---

## Checked and found sound

- **Challenge-response login.** Challenges are per connection, so a recorded
  login can't be replayed. Login, MLS credentials and profiles use the hybrid
  Ed25519 + ML-DSA-65 signature, and both halves must verify.
- **Key commitment.** DAVE frames carry the 16-byte zero-padding commitment,
  which defeats partitioning-oracle attacks on XChaCha20-Poly1305.
- **Audio nonces.** Senders draw a random 192-bit XChaCha20 nonce for every
  frame, so the 16-bit sequence number wrapping never repeats a nonce.
- **Ciphersuite pinning.** Clients refuse KeyPackages and Welcomes for any MLS
  suite but the hybrid post-quantum one, so a server can't downgrade a group.
- **Credential checks.** Every leaf in a Welcome, every added KeyPackage and
  every leaf a commit touches has its credential verified before it is used.
- **Fail-closed audio on both clients** (since #62 on desktop). Nothing is sent
  before a group key exists, and there is no plaintext path.
- **Invite pinning.** An `aura://` link's certificate fingerprint is checked in
  every build configuration, a mismatch is refused outright, and the pin is
  saved only after a successful connection (#61, #62).
- **Identity backups.** The format uses scrypt (N = 2^17) and
  XChaCha20-Poly1305 with the header as associated data, so the KDF parameters
  can't be lowered undetected, and crafted files can't demand unbounded memory
  (#60).
- **SQL.** Every query uses bound parameters.
- **Text replay protection.** The server keeps a bounded set of seen text
  message ids.
- **Moderation authorisation** (#65). Permissions are read from the database on
  every request, nobody can act on their own account, and acting on another
  admin needs `grant_admin`, so the server can't lock itself out.

## Not yet reviewed

- The `aura-identity` crate's hybrid signature construction in depth (beyond
  its tests), and the draft post-quantum MLS ciphersuite itself.
- Which `EncryptedTextPacket` fields outside the ciphertext a server can alter,
  and with what effect, beyond `reply_to_id` (F5).
- Encrypted file sharing (#73, draft): the chunk construction, the relay's
  limits, and sender attribution of the manifest.
- Windows-specific code paths: the global push-to-talk hook and the
  `aura://` scheme registration were written but have never run on Windows.
- Fuzzing coverage of protobuf, MLS and audio parsing. There are no fuzz
  targets on `main` yet; [`FUZZING.md`](FUZZING.md) describes the plan.
