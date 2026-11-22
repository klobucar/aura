---
title: Post-Quantum Migration
description: Hybrid post-quantum key exchange in Aura's transport and MLS layers, and the plan for hybrid signatures.
---
# Post-Quantum Migration

Aura protects recorded traffic against a future quantum computer ("harvest
now, decrypt later") by using **hybrid** key exchange: classical X25519
combined with ML-KEM-768 (FIPS 203). A session stays confidential unless
*both* are broken, so the change cannot make Aura weaker than it was against a
classical attacker.

| Layer | Status | Algorithm |
|---|---|---|
| Media/text payloads (DAVE) | Unchanged, already PQ-adequate | XChaCha20-Poly1305, 256-bit keys |
| QUIC / TLS 1.3 key exchange | **Phase 1, done** | `X25519MLKEM768`, classical fallback |
| MLS group key exchange (E2EE) | **Phase 2, done** | `MLS_128_MLKEM768X25519_AES256GCM_SHA384_Ed25519` |
| Identity & MLS signatures | **Phase 3, planned** | Ed25519 today; Ed25519 + ML-DSA-65 planned |

Symmetric primitives with 256-bit keys keep ~128-bit security against
Grover's algorithm, so the DAVE wire format did not change.

---

## Phase 1: Hybrid transport (QUIC / TLS 1.3)

**Where:** `crates/aura-server/src/tls.rs`, used by every TLS source in
`connection.rs` (manual certificates, ACME, self-signed fallback).

- The server uses **aws-lc-rs** (`ring` is no longer in the server's
  dependency graph) and offers, in order: `X25519MLKEM768`, `X25519`,
  `secp256r1`, `secp384r1`. TLS 1.3 only, as QUIC requires.
- The order is set explicitly rather than inherited from rustls's defaults,
  which only prefer the hybrid when the `prefer-post-quantum` cargo feature
  happens to be unified into the build.

**Who decides.** rustls servers follow the *client's* group preference, so
the server makes PQ available but cannot force it. Whether a given
connection is hybrid depends on the client's TLS stack:

| Client | Hybrid TLS? |
|---|---|
| macOS app on macOS 26+ | Yes. Apple's TLS stack offers `X25519MLKEM768` by default from macOS 26. |
| macOS app on macOS 14–15 | No, classical X25519. There is no API to add the group. |
| Desktop app (.NET / MsQuic) | Depends on the TLS library MsQuic uses on that platform. OpenSSL 3.5+ (Linux) offers it by default; Schannel support depends on the Windows version. |
| `aura-ping` | Yes, it uses the server's provider. |

**Tests:** `tls::tests` runs in-memory handshakes against the production
config (PQ preferred → `X25519MLKEM768`; classical-only client → `X25519`;
PQ-only client accepted). `connection::tests` runs real **quinn** handshakes
with a PQ-only client (its ~1.2 KB key share spans several QUIC Initial
packets) and a classical-only client.

---

## Phase 2: Hybrid MLS (E2EE voice and text)

**Where:** `crates/aura-core/src/mls.rs` (`CIPHERSUITE`, `leaf_capabilities`).

### Ciphersuite

`MLS_128_MLKEM768X25519_AES256GCM_SHA384_Ed25519` from
draft-ietf-mls-pq-ciphersuites, via OpenMLS 0.9 and its RustCrypto provider:

- **KEM:** MLKEM768-X25519, which is X-Wing (draft-connolly-cfrg-xwing-kem-06).
  It protects every group secret MLS distributes: Welcome group secrets and
  UpdatePath secrets.
- **AEAD:** AES-256-GCM. **Hash/KDF:** SHA-384.
- **Signatures:** Ed25519 (unchanged; see Phase 3).
- The "128" in the name is the draft's security category for ML-KEM-768, not
  the AEAD key size. The code point (0x004E) is **provisional** until IANA
  assigns one.

DAVE sender keys are still 32-byte MLS exporter outputs, so their derivation,
the epoch-retention logic and the data plane are unchanged.

**Why this suite and provider.** OpenMLS also has the individual-draft suite
`MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (0x004D), but only its libcrux
provider implements it, and `openmls_libcrux_crypto` 0.4.0 pins `libcrux-kem`
0.0.9. That version's X-Wing public-key decoder panics on short input
(RUSTSEC-2026-0331), and in MLS the public keys come from peers: a crafted
KeyPackage would crash the founder's client. The RustCrypto provider decodes
the same KEM through RustCrypto's `x-wing`/`ml-kem` crates, which reject
malformed keys with an error. Revisit once a fixed libcrux provider ships.

### Downgrade protection

`MlsClient` enforces the suite itself instead of relying on what peers offer:

- **`add_member`** rejects a KeyPackage of any other suite with
  `MlsError::CiphersuiteMismatch`, before the group changes.
- **`process_welcome`** refuses to join a group of any other suite. OpenMLS on
  its own *would* join any suite the provider supports; the check runs before
  the group is stored.
- KeyPackages and leaves advertise **only** this suite in their capabilities.

### Compatibility: all clients must upgrade together

There is no suite negotiation. A client from before this change cannot join a
group created by a new one, and vice versa. The founder logs `Ciphersuite
mismatch … (peer client may need updating)`. MLS state is in memory only, so
nothing persisted needs migrating; the server needs no MLS changes (it relays
opaque bytes).

### Payload sizes and frame caps

X-Wing public keys are 1,216 bytes and ciphertexts 1,120 bytes (X25519: 32
bytes each). Measured with the founder adding members one at a time and
ratcheting every eight adds:

| Group size | KeyPackage | Welcome | Ratchet commit | Commit + Welcome upload |
|---:|---:|---:|---:|---:|
| 2 | 2.6 KB | 8 KB | 4 KB | 16 KB |
| 64 | 2.6 KB | 101 KB | 84 KB | 188 KB |
| 100 | 2.6 KB | 151 KB | 128 KB | 281 KB |
| 256 | 2.6 KB | 370 KB | 314 KB | 688 KB |
| 380 | 2.6 KB | 542 KB | 463 KB | 1,007 KB |

The classical suite was ~9× smaller (77 KB for the 256-member upload).

All MLS framing is `[type u8][len u32][protobuf]`, so no field-level caps
apply. The binding limit was the server's 256 KB inbound control-frame cap,
which the founder's combined Commit + Welcome (`MSG_MLS_COMMIT_WELCOME`)
crosses at about 90 members. The server now allows **1 MiB for
`MSG_MLS_COMMIT_WELCOME` and `MSG_MLS_RATCHET_COMMIT` only**; other control
messages keep 256 KB. That supports channels of roughly **390 members**. Both
clients already accept 2 MB inbound frames, which covers a Welcome for any
group the server cap admits, including `JoinResponse` with two Welcomes.

**Tests** (`mls::tests`): suite parameters; KeyPackages carry real 1,216-byte
X-Wing keys and advertise only this suite; join → add → non-founder ratchet
with key agreement at every epoch; rejection of a legacy KeyPackage; rejection
of a classical-suite Welcome with nothing persisted.

---

## Phase 3: Hybrid signatures (design)

Signatures are not exposed to harvest-now-decrypt-later: a recorded signature
gives an attacker nothing to decrypt. The risk is **forgery once a
cryptographically relevant quantum computer exists**. Every long-term Ed25519
identity pinned by TOFU would then be forgeable, so this migration must finish
*before* that point, but it is not urgent the way key exchange was.

### Target: Ed25519 + ML-DSA-65 composite identity

Each identity becomes a pair `(Ed25519, ML-DSA-65)`. A signature is valid only
if **both** halves verify (AND-combiner), with domain separation in the style
of draft-ietf-lamps-pq-composite-sigs (`id-MLDSA65-Ed25519`): both halves sign
`Prefix || Domain || len(ctx) || ctx || M`. Sizes: ML-DSA-65 public key
1,952 B, signature 3,309 B.

Implement it once in **aura-core** (RustCrypto `ml-dsa`, already in the
dependency graph through OpenMLS) and expose it over UniFFI, so both clients
share one implementation regardless of platform crypto support.

### TOFU authentication (`auth.rs`, `connection.rs`, `db.rs`)

1. **Wire:** the auth request uses `u8` length prefixes for key and signature
   (255-byte max), which ML-DSA cannot fit. Add an `AuthRequestV2` message
   with `u16` lengths carrying both public keys and both signatures over the
   challenge. Keep v1 for accounts that haven't upgraded.
2. **Storage:** add a nullable `mldsa65_public_key BLOB` column next to
   `ed25519_public_key`.
3. **Upgrading an existing account:** on its first v2 login, accept the
   ML-DSA key only together with a **binding statement signed by the
   already-pinned Ed25519 key**
   (`Ed25519.sign("aura-pq-bind-v1" || mldsa65_pk)`). Without that, a
   quantum-capable attacker who can forge Ed25519 could attach their own
   ML-DSA key.
4. **Downgrade lock:** once an account has an ML-DSA key, refuse v1
   (Ed25519-only) authentication for it.
5. **New accounts** register both keys from the start.

### Identity verification and profiles (`verification.rs`, `profile.rs`)

- Bump `FINGERPRINT_VERSION` and hash both public keys into key and pairwise
  fingerprints. Upgraded users' **safety words will change once**; the UI
  should explain the re-verification and must keep v1 words for contacts who
  haven't upgraded.
- Profiles carry both signatures. Verifiers require both once the signer's
  account is ML-DSA-bound.
- The known-answer tests added with this work (`fingerprints_match_known_answers`,
  `signature_and_hash_match_known_answers`) pin the v1 outputs; v2 needs its own.

### MLS leaf credentials (`mls.rs`)

OpenMLS's `SignatureScheme` is a closed enum and offers no composite
Ed25519 + ML-DSA scheme. The draft-ietf-mls-pq-ciphersuites suites with
ML-DSA (`…_MLDSA44`, `…_MLDSA65`) are **pure** ML-DSA, and the ML-DSA-65 one
also drops the X25519 half of the KEM. Options, in order of preference:

1. **Wait for a standardized hybrid-signature MLS suite** and adopt it as
   another all-clients-at-once suite change.
2. **Application-layer binding now:** keep Ed25519 as the MLS signature and
   put the member's ML-DSA-65 public key plus an ML-DSA signature over the
   leaf's Ed25519 key in a leaf-node extension, verified by aura-core on every
   KeyPackage, Welcome and Commit (both must pass). Cost: about +5.3 KB per
   member in the ratchet tree, which cuts the 1 MiB-cap group size from about
   390 to roughly 150 unless Welcomes stop embedding the tree.
3. **Switch to a pure ML-DSA suite.** This loses the classical half of the
   signature and, for ML-DSA-65, the classical half of the KEM.

### Platform key storage

| Platform | Ed25519 (today) | ML-DSA-65 (planned) |
|---|---|---|
| macOS 26+ | Software key, wrapped by a Secure Enclave **P-256** key | Either software (wrapped like Ed25519), or Enclave-resident via CryptoKit `SecureEnclave.MLDSA65` |
| macOS 14–15 | Software key, wrapped by a Secure Enclave P-256 key | Software, wrapped the same way. The Enclave holds only P-256 keys on these versions. |
| Desktop | File under the user's profile directory | Same file, via aura-core |

The Secure Enclave never holds Ed25519 keys on any macOS version. An
Enclave-resident ML-DSA key **cannot be exported**, which breaks profile
export/import, so the default should be a software ML-DSA key wrapped like
the Ed25519 key, with Enclave residency as an opt-in for non-portable
identities. The wrapping key (P-256 ECIES) is classical. That protects keys at
rest on the device, not traffic in transit, so it is outside the
harvest-now model.

### Suggested order

1. Composite sign/verify in aura-core, with known-answer tests.
2. `AuthRequestV2`, DB column, binding statement, downgrade lock.
3. v2 fingerprints and safety words, and profile dual signatures.
4. MLS leaf binding (option 2 above), or adopt a hybrid MLS suite if one has
   been standardized by then.
