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
   - Ciphersuite: `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` (hybrid post-quantum key exchange, ML-DSA-44 signatures; see [PQ Migration](PQ_MIGRATION.md))
   - Each member signs with a persistent ML-DSA-44 key derived from their hybrid Ed25519 + ML-DSA-65 identity, and carries a credential in which that identity certifies the key. Credentials are checked against each leaf's key on every Welcome, Add and Commit (`MlsError::InvalidCredential`)
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
- The founder is the **only committer**: adds, ratchets and removals all come
  from it, so a group never has two commits racing for the same epoch. The
  server drops a commit (and its Welcome) from anyone else.
- The server keeps an ordered roster of who is actually in each MLS tree,
  separate from who is in the channel (which includes sessions still waiting
  for a Welcome). The founder is the roster's first entry.

### 1a. Leaving a channel

Leaving a channel — switching away, disconnecting, or having the session
removed — takes the member out of the MLS group, not just out of the server's
routing:

```
Leaver            Server                         Founder            Others
  |-- JOIN other -->|                               |                  |
  |   (or drops)    |-- MLS_SYNC_MEMBERS (0x58) --->|                  |
  |                 |   [ids that should remain]    |                  |
  |                 |<-- MLS_RATCHET_COMMIT (0x57) -|                  |
  |                 |    (Remove commit)            |                  |
  |                 |-- MLS_COMMIT ------------------------------------>|
```

- The founder commits a Remove for every leaf whose **verified** credential
  identity is not in the list, then everyone remaining moves to an epoch the
  leaver cannot derive. Clients also delete their own copy of a channel's
  groups when they leave it.
- `MLS_SYNC_MEMBERS` can only shrink a group. A server that lies in it can drop
  members — which it could already do by not relaying — but cannot add one.
- **Founder hand-off:** if the founder is the one leaving, the next-longest
  member becomes founder (and receives any key packages still waiting) before
  the removal is requested. If nobody is left in the tree, the first session
  still waiting for a Welcome starts a fresh group and adds the rest; a group
  is never split between an old tree and a new one.
- **Limitation:** membership is per account id, the identity in the MLS
  credential. One account in a channel from two devices keeps both leaves
  until both have left.

Before this, leaving only stopped the server routing to the member. They
stayed in the tree, so anyone able to show them later commits (a malicious
server) could let them keep deriving the group's keys; and a founder who moved
to another channel kept receiving every newcomer's key package.

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
    ttl: Duration,
    // channel_id -> (message_id -> expires_at)
    messages: DashMap<String, HashMap<String, Instant>>,
}

impl SeenMessages {
    pub fn check_and_mark(&self, channel_id: String, message_id: &str) -> bool {
        // Returns false if message_id was seen within the TTL
    }
}
```

**Attack mitigation:**
- Duplicate `message_id` within the TTL → rejected by server
- TTL: 300 seconds (5 minutes); after that the ID is accepted again
- A background task sweeps expired entries every 60 seconds
  (`SEEN_MESSAGE_SWEEP_INTERVAL`), so memory holds at most one TTL's worth
  of message IDs

---

## Security Guarantees

### What the Server CANNOT Do

❌ **Decrypt audio or text** - No access to MLS group secrets  
❌ **Impersonate users** - hybrid Ed25519 + ML-DSA-65 identity signatures required  
❌ **Inject messages** - AEAD tags prevent forgery  
❌ **Replay messages** - Message ID deduplication  
❌ **Downgrade encryption** - Clients enforce MLS

### What the Server CAN Do

✅ **Route messages** - Opaque relay of ciphertext  
✅ **Track presence** - Who is in which channel  
✅ **Enforce rate limits** - Prevent DoS  
✅ **Manage membership** - Route only to the channel's sessions, and ask the founder to remove members who left (it can request removals, never additions)  
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
// Ciphersuite (draft-ietf-mls-pq-ciphersuites, provisional code point 0x0052)
MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44

// Key schedule
- HPKE KEM: MLKEM768-X25519 (X-Wing): hybrid X25519 + ML-KEM-768
- AEAD: ChaCha20-Poly1305
- Hash/KDF: SHA-384
- Signatures: ML-DSA-44, with each member's key certified by their hybrid
  Ed25519 + ML-DSA-65 identity in their credential

// Extensions
- Ratchet tree extension in Welcomes
- Leaf capabilities advertise only the ciphersuite above
```

The hybrid KEM protects every group secret MLS distributes (Welcome group
secrets, UpdatePath secrets) against harvest-now-decrypt-later. DAVE sender
keys remain 32-byte MLS exporter outputs, so the data plane is unchanged.

**Payload sizes.** Post-quantum keys, signatures and credentials make MLS
handshake messages large: a KeyPackage is ~13.7 KB, and a Welcome grows by
~10.2 KB per group member. The server accepts MLS Commit/Welcome uploads up to
1 MiB, enough for channels of 86 members. See
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
| MLS Core (incl. PQ suite, downgrade guards, credential checks) | 24 | ✅ All pass |
| DAVE Crypto (incl. known-answer vector) | 18 | ✅ All pass |
| Text Crypto | 4 | ✅ All pass |
| Replay Protection | 5 | ✅ All pass |
| Server State | 33 | ✅ All pass |

---

## Deployment Considerations

### Key Storage

**Client-side:**
- One 64-byte identity secret per user: Keychain on macOS (Secure Enclave-wrapped
  for biometric profiles), `identity.json` on desktop
- The MLS signing key is derived from it, so it is the same on every connection
- MLS group state stays in memory

**Server-side:**
- Only stores public identity keys (Ed25519, plus the hybrid key once pinned)
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
- [x] **Post-quantum signatures** - Hybrid Ed25519 + ML-DSA-65 identities; ML-DSA-44 in MLS ([details](PQ_MIGRATION.md#phase-3-hybrid-signatures))
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

**Last Updated:** 2026-11-22  
**Implementation Status:** Phase 4 Complete (MLS + Replay Protection)
