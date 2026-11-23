---
title: Security Review
description: What has been reviewed in Aura, what is still open, and what was fixed, with links to the fixes.
---
# Aura Security Review

**Status as of 22 November 2026.** This is a living document. It is updated
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

- **End-to-end encryption of voice and text holds on `main`,** with one open
  gap: a member who leaves a channel stays in its MLS group until the fix below
  lands ([A1](#a1-high-leaving-a-channel-does-not-remove-you-from-its-mls-group)).
- **The desktop client sent voice unencrypted from April to November 2026**
  ([F1](#f1-critical-desktop-voice-was-sent-unencrypted)). Fixed in #59 and #62.
  If you used the desktop client in that window, assume the server operator,
  and anyone able to read the server's memory, could have heard your voice.
- **Never use a Debug build of either client** for real conversations: Debug
  builds accept any server certificate ([A3](#a3-medium-debug-builds-accept-any-server-certificate)).
- **No moderation over the wire yet.** An admin cannot ban or kick an abusive
  user from a running server (A4).

---

## Open findings

| ID | Severity | Component | Finding | Status |
|---|---|---|---|---|
| A1 | High | server, core, clients | [Leaving a channel does not remove you from its MLS group](#a1-high-leaving-a-channel-does-not-remove-you-from-its-mls-group) | Fix in review (#64) |
| A2 | Medium | server | [Any session can inject a commit into any group](#a2-medium-any-session-can-inject-a-commit-into-any-group) | Fix in review (#64) |
| A3 | Medium | macOS, desktop | [Debug builds accept any server certificate](#a3-medium-debug-builds-accept-any-server-certificate) | Open; release builds unaffected |
| A4 | Medium | server | No wire messages reach the existing verify / ban / grant-admin logic, and there is no kick, so abuse can't be handled on a live server | Fix in review (#65) |
| A5 | Low | server | A join to a channel id that doesn't exist is accepted and creates a phantom group (e.g. rejoining a deleted channel after a reconnect) | Fix in review (#65) |
| A6 | Low | core, clients | Profiles are signed but nothing verifies the signatures (`verify_profile` exists in core; no caller) | Open |
| A7 | Low | macOS | Profile exports made before #61 hold the identity secret as **plaintext JSON**. The app now writes passphrase-encrypted `.aura-identity` backups, but old files on disk are still plaintext | Delete old exports; re-export |

### A1. [HIGH] Leaving a channel does not remove you from its MLS group

**Where:** `crates/aura-server/src/state.rs` (`remove_from_voice_group`,
`remove_from_text_group`, the founder logic in the MLS join handlers).

**What happens.** Leaving a channel, by switching away or disconnecting, only
removes the session from the server's routing set. Nobody commits an MLS
Remove, so the leaver stays a leaf of the channel's voice and text groups. Two
consequences:

1. The [threat model](THREAT_MODEL.md)'s first goal, *only current members can
   decrypt*, then rests on the server routing correctly. A malicious server can
   keep forwarding the group's commits to a departed member, who can keep
   deriving every future epoch's keys.
2. If the group's founder is the one who left, the server keeps treating them
   as founder: every newcomer's KeyPackage goes to them, and their client keeps
   up with the group they left. If the founder disconnects instead, the next
   joiner creates a *new* group while the people already in the room stay in
   the old one, and they can no longer hear each other.

**Mitigation on `main`.** Since #61 and #62, both clients refuse to let another
channel's group set their sending key. That prevents audio from being encrypted
for the wrong room, but does not remove anyone from a group.

**Fix (in review, #64).**
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

**What happens.** The server distributes a commit, and advances its epoch
counter, without checking who sent it. Clients reject a commit signed by a
non-member, so a stranger can't change a group's keys. But a *member* who isn't
the founder can commit alongside the founder and fork the group's epoch, and
anyone can make every member process junk and desynchronise the server's epoch
counter.

**Fix (in review, #64).** Only the group's founder may commit; the server
drops anyone else's commit, and its Welcome, before sending anything.

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

---

## Fixed findings

Newest first. "Since" is when the problem entered `main`, where known.

| ID | Severity | Component | Finding | Fixed in |
|---|---|---|---|---|
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
- **Replay protection.** The server keeps a bounded set of seen text message
  ids.

## Not yet reviewed

- The `aura-identity` crate's hybrid signature construction in depth (beyond
  its tests), and the draft post-quantum MLS ciphersuite itself.
- Windows-specific code paths: the global push-to-talk hook and the
  `aura://` scheme registration were written but have never run on Windows.
- Fuzzing coverage of the client-side protobuf and MLS parsing (see
  [`FUZZING.md`](FUZZING.md) for what is fuzzed today).
