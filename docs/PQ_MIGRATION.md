---
title: Post-Quantum Migration
description: Hybrid post-quantum key exchange and signatures across Aura's transport, MLS and identity layers.
---
# Post-Quantum Migration

Aura protects recorded traffic against a future quantum computer ("harvest
now, decrypt later") with **hybrid** key exchange: classical X25519 combined
with ML-KEM-768 (FIPS 203). It protects identities against forgery with
**hybrid** signatures: Ed25519 combined with ML-DSA-65 (FIPS 204). In both
cases an attacker has to break the classical *and* the post-quantum half, so
these changes cannot make Aura weaker than it was against a classical attacker.

| Layer | Status | Algorithm |
|---|---|---|
| Media/text payloads (DAVE) | Unchanged, already PQ-adequate | XChaCha20-Poly1305, 256-bit keys |
| QUIC / TLS 1.3 key exchange | **Phase 1, done** | `X25519MLKEM768`, classical fallback |
| MLS group key exchange (E2EE) | **Phase 2, done** | X-Wing (X25519 + ML-KEM-768) |
| Long-term identity: login, profiles, MLS credentials | **Phase 3, done** | Ed25519 + ML-DSA-65, both required |
| MLS message signatures | **Phase 3, done** | ML-DSA-44, with a key certified by the identity |

Symmetric primitives with 256-bit keys keep ~128-bit security against
Grover's algorithm, so the DAVE wire format did not change.

The MLS ciphersuite is
`MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` (Phases 2 and 3).

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

## Phase 2: Hybrid MLS key exchange (E2EE voice and text)

**Where:** `crates/aura-core/src/mls.rs` (`CIPHERSUITE`, `leaf_capabilities`).

### Ciphersuite

`MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` from
draft-ietf-mls-pq-ciphersuites, via OpenMLS 0.9 and its RustCrypto provider:

- **KEM:** MLKEM768-X25519, which is X-Wing (draft-connolly-cfrg-xwing-kem-06).
  It protects every group secret MLS distributes: Welcome group secrets and
  UpdatePath secrets.
- **AEAD:** ChaCha20-Poly1305 (256-bit key). **Hash/KDF:** SHA-384.
- **Signatures:** ML-DSA-44. See Phase 3 for how each member's signing key is
  tied to their hybrid identity.
- The "128" in the name is the draft's security category, not the AEAD key
  size. The code point (0x0052) is **provisional** until IANA assigns one.

It is the only OpenMLS suite that pairs ML-DSA with the hybrid KEM; the
ML-DSA-65 suite (0x0051) uses pure ML-KEM-768. DAVE sender keys are still
32-byte MLS exporter outputs, so their derivation, the epoch-retention logic
and the data plane are unchanged.

**Why the RustCrypto provider.** OpenMLS's libcrux provider pins
`libcrux-kem` 0.0.9, whose hybrid public-key decoder panics on short input
(RUSTSEC-2026-0331). In MLS the public keys come from peers, so a crafted
KeyPackage would crash the founder's client. The RustCrypto provider decodes
X-Wing through RustCrypto's `x-wing`/`ml-kem` crates, which reject malformed
keys with an error.

### Downgrade protection

`MlsClient` enforces the suite itself instead of relying on what peers offer:

- **`add_member`** rejects a KeyPackage of any other suite with
  `MlsError::CiphersuiteMismatch`, before the group changes.
- **`process_welcome`** refuses to join a group of any other suite. OpenMLS on
  its own *would* join any suite the provider supports; the check runs before
  the group is stored.
- KeyPackages and leaves advertise **only** this suite in their capabilities.

### Compatibility: all clients must upgrade together

There is no suite negotiation. A client from before a suite change cannot join
a group created by a new one, and vice versa; the founder logs `Ciphersuite
mismatch … (peer client may need updating)`. MLS state is in memory only, so
nothing persisted needs migrating, and the server relays MLS messages as
opaque bytes.

---

## Phase 3: Hybrid signatures

Signatures aren't exposed to harvest-now-decrypt-later (a recorded signature
gives an attacker nothing to decrypt), but once a quantum computer exists it
could forge any classical signature, including every long-term identity pinned
by TOFU. Phase 3 replaces those identities before that point.

### Long-term identity: Ed25519 + ML-DSA-65

**Where:** `crates/aura-identity`.

- **Keys.** A 64-byte secret (Ed25519 secret key, then ML-DSA-65 seed), a
  1,984-byte public key (ML-DSA-65, then Ed25519) and 3,373-byte signatures.
- **Both halves required.** A signature is the ML-DSA-65 signature followed
  by the Ed25519 signature over the same message, and it verifies only if both
  do (an AND combiner, in the style of draft-ietf-lamps-pq-composite-sigs,
  without claiming wire compatibility).
- **Purpose binding.** The signed message is
  `PREFIX || len(label) || label || message`, with a label per use (`auth`,
  `profile`, `mls-credential`), so a signature for one purpose never verifies
  as another. The ML-DSA half also carries the prefix as its FIPS 204 context
  string.
- **Upgrading.** An Ed25519-only identity upgrades by keeping its Ed25519
  secret and adding a fresh ML-DSA seed, so its Ed25519 public key, and the
  account pinned to it, stay the same.

Signing happens only in Rust. The clients call the aura-core FFI
(`generate_identity_secret`, `upgrade_ed25519_identity`,
`build_auth_request`, `MlsWrapper(user_id, identity_secret)`, …) with the
stored secret, so the signature formats are defined in one place.

### Login (TOFU)

**Where:** `crates/aura-server/src/{auth,connection,db}.rs`,
`crates/aura-core/src/identity_ffi.rs`.

- The server opens with `[0x02][32-byte challenge]`. The client replies
  `[0x03][u32 LE length][AuthRequest]` with its identity key and a hybrid
  signature over `auth_transcript`: the challenge, the display name it claims
  and its whole identity key. The server answers
  `[0x04][u32 LE length][AuthResponse]`, including the account's user id
  (`U_…`) and `is_admin`.
- Accounts are found by the Ed25519 half of the key. Schema v2 adds a nullable
  `users.identity_public_key`. An account created before hybrid identities, or
  a bootstrap admin, gets its key pinned at its next login: its pinned Ed25519
  key signed a transcript containing the whole identity key, which vouches for
  the new ML-DSA half. Once pinned, a different key is refused.
- `AURA_BOOTSTRAP_ADMIN_KEY` is still the admin's 64-hex Ed25519 public key.

### MLS: a persistent ML-DSA-44 key, certified by the identity

The suite fixes MLS signatures to ML-DSA-44, and MLS has no hybrid signature
scheme. So each member signs MLS messages with an ML-DSA-44 key and proves
which identity it belongs to with a **credential**
(`aura_identity::MemberCredential`, carried in the MLS `BasicCredential`):
their user id, their identity public key, and the identity's hybrid signature
over (signature scheme, user id, MLS public key).

- **Persistent.** The MLS key is derived from the identity secret
  (`IdentitySecret::mls_signing_key`, a one-way hash KDF), so a member presents
  the same key on every connection and keeps one stored secret. Trust pinned
  on a member survives reconnects.
- **Checked on entry.** `MlsClient` verifies the credential against the leaf's
  own signature key for every leaf in a Welcome, every KeyPackage it adds, and
  every leaf a commit adds or updates, before acting on it
  (`MlsError::InvalidCredential`). OpenMLS checks that leaves sign with their
  key, but not whose key it is.
- **Replaceable.** A future suite with a stronger or hybrid signature changes
  only the MLS key and its certificate; the identity stays the same.

What ML-DSA-44 doesn't give: MLS messages are signed by ML-DSA-44 alone, so
anyone able to break ML-DSA-44 could forge messages from an existing member's
leaf. The credential protects who a key belongs to, not each message.

### Verification and profiles

- **Safety words and fingerprints** are computed over the 1,984-byte identity
  key from each member's verified credential, with fingerprint version 1
  (`00 01`), so they can't be confused with the old values over 32-byte
  Ed25519 keys. Everyone's safety words changed once with this release.
- **Profiles** are signed with the hybrid identity (`Purpose::Profile`). The
  signed payload and content hash are unchanged. Server-side verification of
  profile signatures is still to do.

### Payload sizes and frame caps

Measured with real credentials, the founder adding members one at a time and
ratcheting every eight adds:

| Group size | KeyPackage | Welcome | Ratchet commit | Commit + Welcome upload |
|---:|---:|---:|---:|---:|
| 2 | 13.7 KB | 25 KB | — | 53 KB |
| 32 | 13.7 KB | 335 KB | 55 KB | 402 KB |
| 64 | 13.7 KB | 660 KB | 93 KB | 766 KB |
| 86 | 13.7 KB | 885 KB | 113 KB | 1,017 KB |

Each member adds about 10.2 KB to every Welcome, because the ratchet tree
carries each member's ML-DSA-44 key and leaf signature plus their identity key
and certificate. With X-Wing and Ed25519 alone it was 1.4 KB.

The server caps `MSG_MLS_COMMIT_WELCOME` and `MSG_MLS_RATCHET_COMMIT` at 1 MiB
(other control messages at 256 KB), which limits channels to **86 members**:
the founder's Commit + Welcome is the binding size. Clients accept 2 MB inbound
frames, enough for a `JoinResponse` carrying two Welcomes at that size.
Doubling the server cap to 2 MiB and the clients' to 4 MB would allow about
170 members.

### Platform key storage

| Platform | Identity secret (64 bytes) |
|---|---|
| macOS | Keychain. Biometric profiles wrap it with a Secure Enclave P-256 key (ECIES). |
| Desktop | `identity.json` (v2) under the user's profile directory, mode 600. |

The Secure Enclave holds P-256 keys, and on macOS 26+ also ML-DSA-65 and -87
(`SecureEnclave.MLDSA65`). A non-exportable, Enclave-resident ML-DSA-65
identity would break profile export, and the MLS key derivation needs the
secret, so it would have to become an opt-in with a separately stored MLS key.

### Tests

- `aura-identity`: round trips, purpose binding, both halves required
  (mixed-signature rejection), upgrade keeps the Ed25519 key, credential
  tampering, known answers.
- `mls::tests`: the suite signs with ML-DSA-44; one identity gives the same
  MLS key on every client; `group_members` reports verified identity keys;
  forged credentials are rejected in a KeyPackage, a Welcome (nothing stored)
  and a commit (epoch unchanged).
- Server: legacy-account pinning, pinned-key swap refused, signature for
  another name refused, schema v1 → v2 migration, and login over real QUIC
  for both success and failure.
