---
title: MLS Security Architecture
description: Security model for Aura's Messaging Layer Security implementation.
---
# Aura MLS E2EE Security Model

## Overview

Aura implements **MLS (Messaging Layer Security)** per RFC 9420 for end-to-end encrypted voice and text communication. The server operates as a **zero-trust relay** that never has access to plaintext content.

---

## Architecture

### Key Components

1. **MLS Client** (`aura-core/src/mls.rs`)
   - OpenMLS 0.9 with the RustCrypto provider
   - Ciphersuite: `MLS_128_MLKEM768X25519_AES256GCM_SHA384_Ed25519` (hybrid post-quantum; see [PQ Migration](PQ_MIGRATION.md))
   - Manages group state, key packages, commits, and welcomes
   - Accepts only that ciphersuite: other-suite KeyPackages and Welcomes are rejected (`MlsError::CiphersuiteMismatch`)

2. **Server Delivery Service** (`aura-server/src/state.rs`)
   - Routes MLS signaling messages (key packages, commits, welcomes)
   - Tracks group membership and epochs
   - **Never decrypts** - operates on opaque ciphertext

3. **DAVE Encryption** (`aura-core/src/crypto.rs`)
   - XChaCha20-Poly1305 AEAD
   - Per-sender keys derived from MLS group secrets
   - Used for both audio frames and text messages

---

## MLS Group Lifecycle

### 1. First-Joiner Protocol

When a user joins a channel:

```
Client                    Server                    Founder
  |                          |                          |
  |--- MLS_JOIN (key pkg) -->|                          |
  |                          |                          |
  |                          |-- MLS_CREATE_GROUP ----->| (if first)
  |                          |                          |
  |                          |<-- (creates group) ------|
  |                          |                          |
  |                          |-- MLS_ADD_MEMBER_REQ --->| (if not first)
  |                          |                          |
  |                          |<-- MLS_COMMIT_WELCOME ---|
  |                          |                          |
  |<-- MLS_WELCOME ----------|                          |
  |                          |-- MLS_COMMIT ----------->| (to others)
```

**Key properties:**
- First client to join becomes the **founder**
- Founder manages group additions via `addMember()`
- All subsequent joins go through founder
- Server queues key packages in `pending_joins`

### 2. Key Derivation

Each client derives **per-sender encryption keys** from the MLS group secret:

```rust
// Export base secret from MLS group
let base_secret = mls_group.export_secret("aura-dave", 32);

// Derive per-sender key using HKDF
let sender_key = hkdf_expand(
    base_secret,
    format!("sender-{}", sender_session_id).as_bytes(),
    32
);
```

**Why per-sender keys?**
- Prevents key reuse across senders
- Enables sender authentication
- Allows independent key rotation per participant

### 3. Epoch Advancement

MLS epochs advance when:
- A new member is added (via `addMember()`)
- A member is removed
- A member updates their key package

**On epoch change:**
1. Founder generates `Commit` message
2. Server broadcasts `Commit` to existing members
3. Server sends `Welcome` to new member
4. All clients call `processCommit()` or `joinGroup()`
5. All clients re-derive audio/text keys from new epoch

---

## Audio Encryption

### Packet Format

```
[Session ID: 4 bytes][Sequence: 2 bytes][Encrypted Payload]
```

**Encrypted Payload:**
```
[Opus Frame][AEAD Tag (16 bytes)][Nonce (24 bytes)]
```

### Encryption Flow

```rust
// Sender
let key = mls.export_audio_key(channel_id, my_session_id);
let crypto = DaveCrypto::new(&key);
let ciphertext = crypto.encrypt(&opus_frame, &nonce);

// Receiver
let key = mls.export_audio_key(channel_id, sender_session_id);
let crypto = DaveCrypto::new(&key);
let plaintext = crypto.decrypt(&ciphertext, &nonce);
```

**Security properties:**
- **Confidentiality**: Server cannot decrypt audio
- **Integrity**: AEAD tag prevents tampering
- **Authenticity**: Per-sender key proves sender identity
- **Forward secrecy**: Keys rotate with MLS epochs

---

## Text Encryption

### Message Format

```
EncryptedTextPacket {
    sender_session_id: u32,
    channel_id: u32,
    epoch: u64,
    message_id: String,        // UUID for replay protection
    ciphertext: Vec<u8>,       // Encrypted TextMessage protobuf
    nonce: Vec<u8>,            // 24 bytes
    tag: Vec<u8>,              // 16 bytes (AEAD tag)
    reply_to_id: String,       // Plaintext (for threading)
}
```

### Replay Protection

The server tracks seen message IDs to prevent replay attacks:

```rust
pub struct SeenMessages {
    messages: DashMap<u32, Vec<SeenMessageEntry>>,
}

impl SeenMessages {
    pub fn check_and_mark(&self, channel_id: u32, message_id: &str) -> bool {
        // Returns false if message_id already seen
        // Entries expire after 5 minutes
    }
}
```

**Attack mitigation:**
- Duplicate `message_id` → rejected by server
- Expired entries cleaned up automatically
- TTL: 300 seconds (5 minutes)

---

## Security Guarantees

### What the Server CANNOT Do

❌ **Decrypt audio or text** - No access to MLS group secrets  
❌ **Impersonate users** - Ed25519 signatures required  
❌ **Inject messages** - AEAD tags prevent forgery  
❌ **Replay messages** - Message ID deduplication  
❌ **Downgrade encryption** - Clients enforce MLS

### What the Server CAN Do

✅ **Route messages** - Opaque relay of ciphertext  
✅ **Track presence** - Who is in which channel  
✅ **Enforce rate limits** - Prevent DoS  
✅ **Manage membership** - Add/remove from groups  
✅ **Detect replays** - Message ID tracking

### Threat Model

**Protected against:**
- Passive network eavesdropping
- Active MITM attacks (TLS 1.3 + TOFU)
- Malicious server operator (zero-trust)
- Replay attacks (message ID deduplication)
- Message tampering (AEAD authentication)

**NOT protected against:**
- Compromised client endpoint
- Malicious group member (insider threat)
- Traffic analysis (metadata leakage)

---

## Implementation Details

### MLS Configuration

```rust
// Ciphersuite (draft-ietf-mls-pq-ciphersuites, provisional code point 0x004E)
MLS_128_MLKEM768X25519_AES256GCM_SHA384_Ed25519

// Key schedule
- HPKE KEM: MLKEM768-X25519 (X-Wing): hybrid X25519 + ML-KEM-768
- AEAD: AES-256-GCM
- Hash/KDF: SHA-384
- Signatures: Ed25519 (classical; hybrid ML-DSA-65 planned)

// Extensions
- Ratchet tree extension in Welcomes
- Leaf capabilities advertise only the ciphersuite above
```

The hybrid KEM protects every group secret MLS distributes (Welcome group
secrets, UpdatePath secrets) against harvest-now-decrypt-later. DAVE sender
keys remain 32-byte MLS exporter outputs, so the data plane is unchanged.

**Payload sizes.** X-Wing keys make MLS handshake messages ~9× larger than
with X25519: a KeyPackage is ~2.6 KB, and a Welcome grows by ~1.45 KB per
group member. The server accepts MLS Commit/Welcome uploads up to 1 MiB,
enough for channels of about 390 members. See
[PQ Migration](PQ_MIGRATION.md#payload-sizes-and-frame-caps) for measurements.

### DAVE Encryption

```rust
// Algorithm: XChaCha20-Poly1305
- Key size: 32 bytes (256 bits)
- Nonce size: 24 bytes (192 bits)
- Tag size: 16 bytes (128 bits)

// Nonce generation
- Random via OsRng (cryptographically secure)
- Never reused for same key
```

### Test Coverage

| Component | Tests | Status |
|-----------|-------|--------|
| MLS Core (incl. PQ suite, downgrade guards) | 19 | ✅ All pass |
| DAVE Crypto (incl. known-answer vector) | 18 | ✅ All pass |
| Text Crypto | 4 | ✅ All pass |
| Replay Protection | 3 | ✅ All pass |
| Server State | 33 | ✅ All pass |

---

## Deployment Considerations

### Key Storage

**Client-side:**
- MLS identity stored in memory (ephemeral)
- No persistent key storage yet
- TODO: Keychain integration (macOS/Windows)

**Server-side:**
- Only stores public keys (Ed25519)
- No MLS group secrets
- SQLite for user/channel metadata

### Performance

**Audio latency:**
- Encryption overhead: ~0.1ms per frame
- MLS key derivation: ~1ms (cached)
- Total E2EE impact: <1% latency increase

**Text throughput:**
- Replay check: O(1) hash lookup
- Encryption: <1ms per message
- Cleanup: Periodic (every 60s)

### Monitoring

**Metrics to track:**
- MLS epoch mismatches (indicates sync issues)
- Replay attack attempts (duplicate message IDs)
- Decryption failures (wrong keys)
- Group membership divergence

---

## Future Enhancements

### Planned (Phase 5+)

- [ ] **Safety numbers UI** - Display MLS epoch authenticators
- [ ] **Member removal** - Graceful eviction from groups
- [ ] **Key rotation** - Periodic re-keying (every 24h)
- [ ] **External senders** - Allow server to inject silence packets
- [ ] **Persistent identity** - Keychain storage for MLS credentials

### Under Consideration

- [x] **Post-quantum MLS key exchange** - Hybrid X25519 + ML-KEM-768 (X-Wing)
- [ ] **Post-quantum signatures** - Hybrid Ed25519 + ML-DSA-65 ([plan](PQ_MIGRATION.md#phase-3-hybrid-signatures-design))
- [ ] **Deniability** - Off-the-record messaging properties
- [ ] **Metadata protection** - Onion routing for signaling
- [ ] **Audit logging** - Cryptographic proofs of server behavior

---

## References

- [RFC 9420: Messaging Layer Security](https://www.rfc-editor.org/rfc/rfc9420.html)
- [OpenMLS Documentation](https://openmls.tech/)
- [DAVE Protocol (Discord)](https://github.com/discord/dave-protocol)
- [XChaCha20-Poly1305](https://datatracker.ietf.org/doc/html/draft-irtf-cfrg-xchacha)

---

**Last Updated:** 2026-11-21  
**Implementation Status:** Phase 4 Complete (MLS + Replay Protection)
