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

### Deviation: Hybrid X25519 + ML-KEM-768 / ML-DSA-44 vs. P-256
*   **Aura**: `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`: X-Wing hybrid KEM (X25519 + ML-KEM-768) and ML-DSA-44 signatures, with each member's MLS key certified by their hybrid Ed25519 + ML-DSA-65 identity.
*   **Standard DAVE**: `DHKEMP256` + `P-256`.

**Rationale:**
- **P-256** (NIST) contains unexplained "seed" values that have long been a source of skepticism in the security community (potential "backdoors").
- **Ed25519/X25519** (Bernstein) is easier to implement safely without side-channels (like branch-on-secret) and has much higher performance. Since we are not bound by legacy browser compatibility, we chose the superior modern standard.
- **ML-KEM-768** is combined with X25519 so that recorded MLS handshakes stay confidential against a future quantum computer ("harvest now, decrypt later"), while the classical half preserves today's security if ML-KEM is ever broken.
- **ML-DSA** signatures resist forgery by a quantum computer. MLS has no hybrid signature scheme, so MLS messages use ML-DSA-44 alone, and the long-term identity that vouches for each member's MLS key is hybrid Ed25519 + ML-DSA-65 (both must verify). See [PQ Migration](PQ_MIGRATION.md).

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

### Deviation: Hybrid identity keys vs. P-256, version 1
*   **Aura**: the member's 1,984-byte hybrid identity public key (ML-DSA-65 key, then Ed25519 key) as the `Pub` input, taken from their verified MLS credential, with fingerprint version `00 01`.
*   **Standard DAVE**: P-256 keys, version `00 00`.

**Rationale:**
Follows from §3: the identity key is what each member's MLS credential
certifies, and it is the same on every connection, so verification and trust
survive reconnects. The construction treats the key as opaque bytes. The
version change keeps these values from ever matching ones computed over the
older 32-byte Ed25519 keys, so every user's safety words changed once.

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

## 6. Membership Changes

### Deviation: Founder-committed removals vs. external-sender proposals
*   **Aura**: There is no external sender. The server tells the group's
    founder which account ids should remain (`MlsSyncMembers`, 0x58), and the
    founder commits a Remove for every other leaf, returned on 0x57.
*   **Standard DAVE**: The voice gateway is an MLS external sender and
    broadcasts external Remove proposals that members commit.

**Rationale:**
Aura's server is a pure Delivery Service and holds no MLS credential, so it
cannot propose anything. Sending the *remaining* ids rather than the leaver's
makes the request idempotent (a lost or repeated request converges on the same
tree) and one-directional: it can only shrink the group. Removals are matched
against each leaf's verified credential identity, which is the account id, so
two leaves for one account (two devices) are kept or removed together.

### Deviation: Single committer with founder hand-off vs. any member committing
*   **Aura**: Only the founder commits. If the founder leaves, the next-longest
    member takes over; if the tree empties, the first waiting session starts a
    fresh group. The server drops commits from anyone else.
*   **Standard DAVE**: Any member may commit pending proposals; the gateway
    picks the winning commit.

**Rationale:**
With one committer the epoch cannot fork, so the server never has to choose
between competing commits. The cost is that commits stall briefly while a
departing founder is replaced, which the hand-off keeps to one round trip.
