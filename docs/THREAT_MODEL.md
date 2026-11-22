---
title: Threat Model
description: What Aura protects, what it doesn't, and the trust assumptions behind each component.
---
# Aura Threat Model

This document summarizes what Aura protects, what it does **not** protect, and the trust assumptions of each component. For implementation details, see [`protocol.md`](protocol.md), [`MLS_SECURITY.md`](MLS_SECURITY.md), [`PQ_MIGRATION.md`](PQ_MIGRATION.md), [`05_dave_protocol_deviations.md`](05_dave_protocol_deviations.md), and [`08_security_review.md`](08_security_review.md).

---

## Security Goals

1. **End-to-End Confidentiality & Integrity for Media and Text:** Only current members of a channel's MLS group can decrypt voice frames or text messages. The server acts as an untrusted Delivery Service and cannot read payloads.
2. **Forward Secrecy (FS) & Post-Compromise Security (PCS):** Compromising a member's key state does not expose past epochs, and future epochs heal after a key update (`Commit`).
   - Text groups ratchet every 50 messages or 5 minutes.
   - Voice groups ratchet on join/leave and on a 15-minute server-prompted commit timer.
3. **Zero-Trust Identity Verification:** Users are identified by hybrid Ed25519 + ML-DSA-65 identity keys (on macOS, optionally wrapped by a Secure Enclave key), pinned Trust-On-First-Use (TOFU), plus BIP39 safety-word verification derived from the authenticated MLS ratchet tree.

---

## Adversary Model

| Adversary | Capabilities | What Is Protected | What Is NOT Protected |
|---|---|---|---|
| **Passive / Active Network Attacker** | Sniff, drop, replay, or inject packets on the wire | All control & media traffic is wrapped in QUIC / TLS 1.3; payloads are additionally E2EE with MLS + XChaCha20-Poly1305 (DAVE) | Packet timing, packet sizes, IP addresses, and traffic volume |
| **Malicious / Compromised Server Operator** | Full access to server memory, SQLite DB, and relayed packets | Voice audio and text message contents; MLS group secrets; private identity keys | **Metadata:** who connects from which IP, channel membership, who talks to whom and when, display names, and message timestamps. Server can also drop traffic (DoS) or attempt to inject/equivocate MLS Welcome/Commit messages if clients skip TOFU/safety-word verification |
| **Malicious Channel Member** | Valid member of a channel's MLS group | Past messages/audio from before they joined; other channels they are not in | Content sent while they are a member of that channel (they hold the group epoch secrets) |
| **Quantum Adversary ("Harvest Now, Decrypt Later")** | Records ciphertext today for future decryption | MLS group secrets use hybrid X25519 + ML-KEM-768 (X-Wing) key exchange, `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`; payloads use 256-bit XChaCha20-Poly1305; QUIC/TLS offers hybrid `X25519MLKEM768` | **Transport metadata from clients without ML-KEM:** TLS uses the hybrid group only when the client's TLS stack offers it (e.g. macOS 26+), otherwise classical X25519. That exposes control-plane metadata, not E2EE content |
| **Quantum Adversary (live)** | Runs a cryptographically relevant quantum computer during a session | Key exchange as above. Identities: login, MLS credentials and profiles are signed with hybrid Ed25519 + ML-DSA-65, and both halves must verify; MLS leaves sign with ML-DSA-44. See [`PQ_MIGRATION.md`](PQ_MIGRATION.md#phase-3-hybrid-signatures) | **The server's TLS certificate is classical** (WebPKI), so the attacker could impersonate the server at the transport layer and see control-plane metadata. E2EE content and user identities don't depend on that certificate |

---

## Trust Assumptions & Known Limitations

- **Pre-alpha status:** Aura has not undergone an external third-party cryptographic audit.
- **TOFU & Key Pinning:** First-time connections trust the peer's advertised identity key until verified out-of-band (safety words). If an active server operator MITMs a user's *very first* key exchange before verification, TOFU alone cannot detect it.
- **Founder-driven MLS Commits:** Because the server is zero-trust, it relies on the channel founder to process `PendingMlsJoin`s and `MlsRatchetRequest`s. A malicious or unresponsive founder can delay epoch advancement until another member takes over or the founder disconnects.
- **Display names:** New accounts must use printable ASCII names, and reserved names (`admin`, `system`, …) are refused even with punctuation added, so a name can't impersonate a role or another user with look-alike characters. Safety words, not names, are what prove identity.
- **Profile signatures:** Profiles are signed with the hybrid identity, but nothing checks those signatures yet. `aura-core` has `verify_profile`; neither the server nor the apps call it.
- **Endpoint Security:** Aura assumes the client OS, audio stack, and local key storage are not compromised.
- **Ciphersuite pinning:** Clients accept exactly one MLS ciphersuite and refuse KeyPackages and Welcomes for any other, so a malicious server or peer cannot downgrade a group to classical key exchange. The flip side is that every client must run a version with the same suite (see [`PQ_MIGRATION.md`](PQ_MIGRATION.md#compatibility-all-clients-must-upgrade-together)).
- **Draft algorithms:** The MLS suite's code point (draft-ietf-mls-pq-ciphersuites) and the X-Wing KEM are pre-standard. Adopting final IANA values will be another all-clients-at-once upgrade.
