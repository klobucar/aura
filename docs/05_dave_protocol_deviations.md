---
title: DAVE Protocol Deviations
description: Rationale for Aura's deviations from the standard Discord DAVE protocol.
---
# Aura DAVE Protocol Deviations & Rationale

While Aura shares the same high-level architecture as Discord's DAVE (MLS-based group key exchange with per-packet E2EE), it makes several strategic deviations optimized for its **QUIC-native transport** and **high-assurance security model**.

## 1. Ciphersuite Selection

### Deviation: XChaCha20-Poly1305 vs. AES-128-GCM
*   **Aura**: `XChaCha20-Poly1305` + Zero-Padding Commitment.
*   **Standard DAVE**: `AES-128-GCM`.

**Rationale (Security Findings - Trail of Bits):**
AES-GCM is famously **not key-committing**. In a multi-party context, an attacker could potentially find two keys that decrypt a single ciphertext into two different valid-looking plaintexts, enabling "partitioning attacks." 
- **TOB-DISCE2EC-5**: Trail of Bits identified this as a gap in Discord's implementation. 
- **Solution**: We chose `XChaCha20-Poly1305` because it has better security bounds for random nonces and naturally higher resistance to certain classes of AEAD malleability. To formally address the commitment gap, we implement a **16-byte Zero-Padding Commitment** inside the ciphertext. This ensures that only the intended key can successfully unpad the message, creating a cryptographical "lock" that AES-GCM lacks.

---

## 2. Transport Architecture

### Deviation: Header-First vs. Footer-Supplement
*   **Aura**: Custom Binary Header (`FastAudioPacket`).
*   **Standard DAVE**: Frame Footer (Magic `0xFAFA`).

**Rationale (QUIC vs. WebRTC):**
Standard DAVE is constrained by **WebRTC**. WebRTC-based SFUs (Relays) often inspect or modify RTP headers. Putting E2EE metadata in a "supplemental footer" allows the packets to look like standard Opus packets to middle-boxes while keeping the E2EE data at the end.
- **Aura Advantage**: Aura uses **QUIC Datagrams**. QUIC treats the payload as an opaque byte stream. We don't have to "hide" our metadata from the transport layer.
- **Security Impact**: By using a **Header-First** approach, we ensure that the session context (Epoch Hint, Sequence, Session ID) is parsed *before* the decryption attempt. This is more robust against truncation attacks (where an attacker chops off the end of a packet) which are a known risk with footer-based schemes.

---

## 3. Cryptographic Primitives

### Deviation: Ed25519/X25519 vs. P-256
*   **Aura**: `DHKEMX25519` + `Ed25519`.
*   **Standard DAVE**: `DHKEMP256` + `P-256`.

**Rationale:**
- **P-256** (NIST) contains unexplained "seed" values that have long been a source of skepticism in the security community (potential "backdoors").
- **Ed25519/X25519** (Bernstein) is easier to implement safely without side-channels (like branch-on-secret) and has much higher performance. Since we are not bound by legacy browser compatibility, we chose the superior modern standard.

---

## 4. Key Derivation Logic

### Deviation: Context-Based Per-Sender Keys
*   **Aura**: Per-session `sender_id` as MLS Export context.
*   **Standard DAVE**: Identity-frame based verification.

**Rationale:**
Aura implements **cryptographical isolation** at the export level. By including the `sender_id` in the MLS `export_secret` context, we ensure that even within the same epoch, **Alice and Bob never share the same symmetric key**.
- In standard DAVE, users share a base secret and use "Identity Keys" to sign transition frames. 
- Aura's approach is simpler and stronger: if a user's session key is somehow leaked, it can *only* be used to impersonate (or decrypt) that specific user's audio, not anyone else in the group.

---

## 5. Identity Verification Display

Aura follows the DAVE pairwise fingerprint construction in `protocol.md`
(§ Verification Fingerprint) unchanged in its essentials — the same
`version || key || id` buffers, the same unsigned lexicographic sort, the same
scrypt parameters (N=16384, r=8, p=2, dkLen=64) and the same fixed salt. The
sort is what makes the result symmetric, so both devices derive identical
output without negotiating who is "first". Three inputs and the presentation
differ. Implementation: `crates/aura-core/src/verification.rs`.

### Deviation: Ed25519 public keys vs. P-256
*   **Aura**: 32-byte Ed25519 signature keys as the `Pub` input.
*   **Standard DAVE**: P-256 keys.

**Rationale:**
Forced by §3 above — the MLS ciphersuite's signature algorithm *is* the
identity key, so choosing Ed25519 for MLS chooses it here too. No security
argument beyond §3's; the fingerprint construction treats the key as opaque
bytes and is indifferent to the curve.

### Deviation: UUID identifiers vs. u64 snowflakes
*   **Aura**: The user's UUID, as UTF-8 bytes.
*   **Standard DAVE**: Big-endian u64 snowflake.

**Rationale:**
Aura has no snowflake type; users are identified by UUID end to end. Binding
the identifier still matters and is preserved — without it, two users who
somehow shared a key would produce the same fingerprint, and a rename could
inherit someone else's verification. The identity comes from the MLS
credential, not from a server-supplied user list: `UserInfo` on the wire
deliberately carries no key material, so a malicious server cannot substitute
one. Keys are read from the authenticated ratchet tree via
`MlsWrapper.group_members`.

### Deviation: BIP-39 safety words vs. the 45-digit displayable code
*   **Aura**: 6 BIP-39 English words, plus both identity fingerprints as
    grouped hex.
*   **Standard DAVE**: 45-digit displayable code, 9 groups of 5 digits.

**Rationale:**
The value only works if people actually compare it, and words survive being
read aloud far better than 45 digits. The BIP-39 English list is chosen for
its unique 4-character prefixes, so a misheard word is still recoverable, and
for its large existing review surface. It is vendored at
`crates/aura-core/src/bip39_english.txt` and pinned by a test against the
canonical SHA-256 `2f5eed…24dbda`.

**Security note.** Six words carry `6 × 11 = 66` bits, short of the digit
code's ~149. Two things carry the difference. First, scrypt's memory-hardness
(~32 MiB per guess) prices up grinding a colliding key far beyond what the bit
count alone suggests. Second, the words are a *convenience layer* over the
full fingerprints, which are displayed alongside them and remain the
authoritative comparison — the trust sheet shows both, and a user who wants
the stronger check has it on screen.

**The wordlist is a protocol constant.** Changing the list, the word count, or
the bit-extraction order silently invalidates every verification users have
already stored, without any visible error. Treat it as frozen.
