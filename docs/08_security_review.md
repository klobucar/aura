# Aura Server Security Review 
*Conducted in the spirit of Trail of Bits*

This document outlines the findings of a vulnerability and architecture review of the Aura Server, specifically focusing on its Rust implementation (`aura-core`, `aura-server`, `aura-protocol`).

## Executive Summary
The Aura server demonstrates a strong foundation in modern cryptographic practices. The use of QUIC, the DAVE protocol (XChaCha20-Poly1305 with zero-padding commitments), and OpenMLS ensure that payload confidentiality and integrity are robust by design. Furthermore, the server operates on a genuine **zero-trust** basis regarding payload data—acting purely as an encrypted relay without accessing plaintext content.

However, several **Availability (Denial of Service)** and **Authentication** vulnerabilities were identified, primarily stemming from unchecked resource allocation and the optimistic nature of the TOFU (Trust On First Use) implementation.

## Vulnerability Findings

### 1. [CRITICAL] Memory Exhaustion via Pre-allocated QUIC Streams (Slowloris Vector)
**Status:** ✅ Fixed in `6772991b1` — control-frame reader now grows incrementally with a hard cap instead of pre-allocating from the length prefix.

**Location:** `crates/aura-server/src/connection.rs`

**Description:**
The server accepts messages over reliable QUIC control streams (e.g., `MSG_AUDIO_STREAM` and `MSG_TEXT_PACKET`). The protocol specifies a `u32` length prefix, which the server uses to pre-allocate a memory buffer up to `MAX_PACKET_SIZE` (currently 2MB).
```rust
let packet_len = u32::from_le_bytes(len_buf) as usize;
if packet_len > MAX_PACKET_SIZE { return; }
let mut packet_buf = vec![0u8; packet_len]; // DANGER: Unbounded eager allocation
self.recv.read_exact(&mut packet_buf).await?;
```
Because the server pre-allocates the vector before the data has arrived, an attacker can open multiple connections and send a message header claiming a 2MB payload, but then stop transmitting data (or drip-feed 1 byte every 29 seconds to bypass the idle timeout). 

**Impact:**
With the default configuration of `max_connections = 1000`, an attacker can trivially force the server to allocate **2GB of RAM**, immediately crashing the 512MB Fly.io VM with an Out-of-Memory (OOM) panic.

**Remediation:**
- Reduce `MAX_PACKET_SIZE` to a sensible limit (`65536` bytes for text, audio shouldn't be on the reliable stream anyway, but capped to `2048` bytes if so).
- Avoid eager allocation. Read from the stream into an incrementally growing `BytesMut` with a capacity limit.

### 2. [MEDIUM] Unbounded CPU Exhaustion via Ed25519 Handshake Spam
**Status:** ✅ Fixed in `960517e50` — per-source-IP token-bucket rate limit on incoming QUIC handshakes.

**Location:** `crates/aura-server/src/connection.rs`

**Description:**
The server requires clients to sign a challenge during the `authenticate_client` handshake. While the challenge generation mitigates replay attacks perfectly, signature verification is computationally expensive. There is no IP-based rate limiting on new QUIC connection attempts.

**Impact:**
A distributed or single high-throughput attacker continuously initiating TLS handshakes and submitting invalid AuthRequests will force the server to perform thousands of Ed25519 signature validations per second, starving the CPU and inducing significant jitter or dropped packets for active voice calls.

**Remediation:**
- Implement a leaky bucket or token bucket rate limiter per IP address for incoming QUIC handshake attempts.

### 3. [LOW] Cosmetic Spoofing in TOFU Identity Registration
**Status:** ⚠️ Partially fixed — reserved-name list (`admin`, `system`, `server`, `aura`, `root`, `moderator`) is now enforced in `auth.rs`. Homoglyph / non-ASCII normalization is still open.

**Location:** `crates/aura-server/src/auth.rs`

**Description:**
The Trust On First Use (TOFU) system allows any new key to claim any available `display_name`. While usernames must be unique (enforced case-insensitively in the database), there is no mechanism to protect reserved names or prevent homoglyph attacks.

**Impact:**
An attacker could register a display name like `System`, `admin`, or use Cyrillic characters to impersonate authority figures.

**Remediation:**
- Implement a restricted word list (e.g., blocking `admin`, `system`, `server`, `aura`).
- Strip non-ASCII/special characters to prevent spoofing.

### 4. [INFORMATIONAL] Forward Secrecy in Low-Churn Voice Groups
**Status:** ❌ Open — no MLS commit timer is in place; long sessions with stable membership reuse the same epoch.

**Location:** `crates/aura-core/src/crypto.rs` / `state.rs`

**Description:**
OpenMLS enforces Post-Compromise Security (PCS) and Forward Secrecy by rotating key material whenever the group membership updates (a `Commit`). However, "Voice Groups" are classified as Low Churn. If a call lasts for 3 hours with the exact same members, the `current_epoch` never increments, and the same DAVE key is used for the entire duration.

**Remediation:**
The system would benefit from forcing an MLS `Commit` on a timer (e.g., every 15 minutes) even if membership has not changed.

## Positive Observations
1. **Challenge-Response Authenticity:** `auth.rs` dynamically generates challenges per connection state rather than globally. This securely prevents malicious clients from recording and reusing authentication packages.
2. **Key Commitment:** The implementation of a 16-byte zero-padding commitment in `dave_crypto.encrypt()` correctly adheres to the DAVE protocol specification, effectively neutralizing partitioning oracle attacks.
3. **Database Parameterization:** All SQLite queries rely strictly on `rusqlite::params![]`. No string interpolation is used for SQL generation, neutralizing injection risks.
