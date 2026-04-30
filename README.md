# Aura

End-to-end encrypted VoIP with text chat. Rust core, native macOS and cross-platform desktop clients, QUIC transport, MLS group keying.

> **Pre-alpha / experimental.** APIs, wire format, and on-disk schemas can change without notice. Do **not** use this for sensitive communication. See [`docs/08_security_review.md`](docs/08_security_review.md) for the in-tree security review, including open findings.

---

## Why Aura?

Most always-on group voice tools force a tradeoff:

- **Mumble** has the right shape — persistent channels, push-to-talk, low-latency, self-hosted — but predates modern end-to-end encryption and offers none of it. The server sees plaintext audio.
- **Discord** is closed-source and centralized; its DAVE rollout brought E2EE to calls but the model still trusts Discord's services for identity, signaling, and metadata.
- **Signal / WhatsApp** have strong E2EE but are built around address-book messaging, not always-on community voice channels.
- **Matrix / Element Call** are general-purpose and powerful, but heavy and not optimized for low-latency hobbyist-server voice.

Aura is the gap in the middle: a small, self-hostable, Mumble-shaped voice + text app where the server is a **zero-trust relay** that never sees plaintext, and identity is cryptographic rather than account-based.

---

## Status: what works today

| Capability | macOS client | Desktop (.NET) client |
|---|---|---|
| Voice (E2EE via DAVE / MLS group keys) | ✅ | ✅ |
| Text chat (E2EE) | ✅ | ✅ |
| Push-to-talk | ✅ global hotkey | 🟡 in-window button only |
| Voice activity detection | ✅ | ✅ |
| Per-user local volume + mute | ✅ persisted across reconnects | 🟡 not persisted |
| Audio preprocessing toggles (RNNoise / AEC / NS / AGC) | ❌ RNNoise hardcoded on | ✅ user-toggleable |
| Settings persistence | ✅ | ❌ lost on app restart |
| Auto-reconnect with backoff | ✅ | 🟡 |
| Master / output volume slider | ❌ | ❌ |
| Recording | ❌ | ❌ |

**Server:** QUIC + TLS 1.3 transport, Ed25519 TOFU authentication, SQLite persistence, channel hierarchy, admin verification with three modes, opaque audio relay.

For the long-form roadmap and milestones, see [`docs/ROADMAP.md`](docs/ROADMAP.md).

---

## How it works

### Transport
- All client ↔ server communication runs over a single **QUIC** connection with TLS 1.3.
- Audio frames travel as **unreliable QUIC datagrams** for low jitter; control, text, and MLS handshake messages travel on **reliable streams**.
- The server is stateless w.r.t. payloads — it routes opaque ciphertext between session IDs based on channel membership.

### Identity
- Every client generates an **Ed25519 keypair** locally on first run. The public key is the user's stable identity.
- Display names are claimed **TOFU**-style (Trust On First Use): the first key to claim a name owns it permanently. Subsequent connections must sign a server-issued challenge with that key.
- On macOS, the private key is stored in the **Secure Enclave** when available. On the desktop client it lives in a file under the user's profile directory.
- The server stores no user passwords. Optional `password = ...` in `server.toml` adds a coarse access-control gate independent of identity.

### Group keying (MLS)
- Each channel is backed by an **MLS group** ([RFC 9420](https://www.rfc-editor.org/rfc/rfc9420.html)) using ciphersuite `MLS_128_DHKEMP256_AES128GCM_SHA256_P256`.
- The first client in a channel becomes the **founder** and owns adds/removes; the server is the MLS Delivery Service and never decrypts.
- On membership changes, an MLS commit moves the group to a new epoch; clients export new per-sender secrets.

### Media encryption (DAVE)
- Aura adopts the **DAVE** AEAD construction (XChaCha20-Poly1305 with zero-padding commitments), originally specified by Discord, on top of MLS-derived keys.
- A per-sender symmetric key is derived from the MLS group secret via HKDF, scoped by the sender's session ID, so keys are never shared between senders.
- Both Opus audio frames and text messages use the same DAVE construction with distinct labels.
- Notes on Aura's deviations from the upstream DAVE spec live in [`docs/05_dave_protocol_deviations.md`](docs/05_dave_protocol_deviations.md). Full MLS notes in [`docs/MLS_SECURITY.md`](docs/MLS_SECURITY.md). Wire format in [`docs/protocol.md`](docs/protocol.md).

### Audio pipeline
- **Capture** → **VAD** (optional, used in voice-activation mode) → **RNNoise** noise suppression → **Opus** encode → **DAVE encrypt** → QUIC datagram.
- **Receive** → DAVE decrypt → **jitter buffer** → Opus decode → mix → playback. The jitter buffer also drives the per-user "talking" indicator.

---

## Threat model (summary)

What Aura tries to protect, and what it explicitly does not. The full review is in [`docs/08_security_review.md`](docs/08_security_review.md).

| | |
|---|---|
| ✅ **Confidentiality of voice + text payloads** against the server operator and any network observer. | The server only sees ciphertext. Even with a full disk image of the server, past calls cannot be decrypted (forward secrecy via MLS commits, modulo open finding #4). |
| ✅ **Integrity** of media frames and text messages. | DAVE's AEAD prevents undetected tampering. |
| ✅ **Authenticity** of speakers within a channel. | Each frame is bound to a sender session whose Ed25519 identity was authenticated at connection time. |
| ✅ **Forward secrecy across membership changes.** | MLS commits rotate the group secret; departed members cannot decrypt future traffic. |
| ❌ **Metadata.** | The server (and any observer of it) can see *who is connected*, *which channels they're in*, *when they speak*, and packet sizes/timing. This is metadata Aura *needs* to route traffic. Use Tor or a similar layer if metadata resistance matters to you. |
| ❌ **Endpoint compromise.** | If a participant's machine is compromised, no protocol can save the conversation. |
| ❌ **Identity verification beyond TOFU.** | First-claim-wins for display names. Out-of-band fingerprint verification between users is not yet exposed in the UI. |
| ❌ **Denial of service.** | Aura mitigates several DoS vectors (per-IP handshake rate limiting, capped buffer growth) but cannot guarantee availability against a determined attacker. |
| ❌ **Audited cryptographic implementation.** | All primitives are widely-used libraries (`ring`, `openmls`, `quinn`, `opus`), but Aura itself has had no third-party audit. |

---

## Repository layout

```
crates/
  aura-protocol/   # shared binary wire format
  aura-core/       # client engine (audio, MLS, codec) — exposed via UniFFI
  aura-server/     # QUIC relay, auth, channel state, SQLite persistence
clients/
  macos/           # SwiftUI client (Xcode project)
  desktop/         # Avalonia (.NET 10) cross-platform client
docs/              # protocol notes, MLS notes, security review, roadmap
scripts/           # build helpers (macOS dylib build, etc.)
```

---

## Quickstart: run a server and connect two clients locally

This walks through standing up a local server and connecting two clients on the same machine.

### Prerequisites
- Rust stable (`rustup install stable`)
- `protoc` on `PATH` (`brew install protobuf` on macOS, `apt install protobuf-compiler` on Debian/Ubuntu)
- For macOS client: Xcode 15+
- For desktop client: .NET 10 SDK

### 1. Start a server

```bash
# Generate a placeholder admin key (replace with your real client's pubkey later).
ADMIN_KEY=$(openssl rand -hex 32)

cat > server.toml <<EOF
[server]
bind_address = "0.0.0.0:8443"
max_connections = 100
log_level = "info"

[database]
path = "aura.db"

[verification]
mode = "none"
EOF

AURA_BOOTSTRAP_ADMIN_KEY=$ADMIN_KEY \
AURA_BOOTSTRAP_ADMIN_NAME="Admin" \
cargo run -p aura-server
```

The server listens on `0.0.0.0:8443` and writes `aura.db` to the working directory.

### 2. Launch two clients

In separate terminals (or one in Xcode and one via the desktop client), connect to `localhost:8443` with two distinct display names. Each client generates its own keypair on first run; whichever one connects first claims its display name.

### 3. Talk

Join the same channel from both clients, hit your push-to-talk hotkey (macOS) or click the in-window PTT button (desktop), and you're encrypted end-to-end through your own server.

To wipe state and start over: stop the server, delete `aura.db`, and clear each client's local keystore (`~/Library/Application Support/Aura/` on macOS, `~/.config/Aura/` on Linux, `%APPDATA%\Aura\` on Windows).

---

## Server configuration

`server.toml`, read from the working directory:

```toml
[server]
bind_address = "0.0.0.0:8443"
max_connections = 1000
log_level = "info"
# password = "optional-server-password"

[database]
path = "aura.db"

[verification]
# "none" | "optional" | "required"
mode = "optional"
```

### Verification modes

| Mode | Meaning |
|---|---|
| `none` | Anyone can connect and join voice channels. |
| `optional` | Admins may verify users; verification grants a cosmetic badge. |
| `required` | Only admin-verified users may join voice channels (text and presence still work). |

### First-time admin

On first start with an empty database, the bootstrap admin is set via env vars:

```bash
export AURA_BOOTSTRAP_ADMIN_KEY=<64-char-hex-ed25519-public-key>
export AURA_BOOTSTRAP_ADMIN_NAME="AdminUser"
cargo run -p aura-server
```

The bootstrap is consumed once — subsequent admin promotions go through the running server's admin API.

---

## Building the clients

### macOS (SwiftUI)

Open `clients/macos/Aura.xcodeproj` and hit Run. A pre-compile build phase invokes `scripts/build_macos.sh`, which builds `aura-core` as a universal dylib (arm64 + x86_64), regenerates the Swift bindings, and writes them into `clients/macos/Aura/Generated/`.

To build outside Xcode:

```bash
./scripts/build_macos.sh release
```

See [`clients/macos/Aura/RUNNING.md`](clients/macos/Aura/RUNNING.md) for the Xcode wiring details (search paths, embed-and-sign, bridging header).

### Desktop (Avalonia / .NET 10)

```bash
cd clients/desktop

# macOS / Linux: build core for the host triple, then run.
cargo build --release -p aura-core
cp ../../target/release/libaura_core.dylib Generated/   # macOS
# cp ../../target/release/libaura_core.so Generated/    # Linux
./run.sh

# Windows (MSVC):
cargo build --release -p aura-core --target x86_64-pc-windows-msvc
copy ..\..\target\x86_64-pc-windows-msvc\release\aura_core.dll Generated\
dotnet run -c Release
```

`Generated/aura_core.cs` is checked in and pinned to the core's UniFFI version. Regenerate with `uniffi-bindgen-cs` only if `aura.udl` changes.

---

## Development

```bash
# Workspace tests
cargo test --workspace

# Lints (clippy) — currently advisory in CI; will go strict once the warning backlog is cleared
cargo clippy --workspace --all-targets

# Formatting
cargo fmt --all
```

Fuzzing harnesses for the protocol parser live under `crates/aura-protocol/fuzz/`. See [`docs/FUZZING.md`](docs/FUZZING.md) for how to run them with `cargo fuzz`.

CI runs on every push and PR via `.github/workflows/ci.yml`: format check (strict), workspace tests (strict), clippy (advisory). The macOS Xcode build and the desktop .NET build aren't gated by the workflow yet.

---

## FAQ

**Why QUIC instead of WebRTC?**
WebRTC is the right answer for browsers and for general-purpose A/V apps that need NAT traversal, simulcast, and SFUs. Aura is a native-app, single-server-per-community tool, and that lets us drop a lot of complexity. QUIC gives us TLS 1.3, multiplexed reliable streams *plus* unreliable datagrams, and 0-RTT resumption in one transport — without a SDP/ICE/DTLS-SRTP stack.

**Why MLS instead of double-ratchet (Signal-style)?**
MLS is designed for groups: O(log N) rekeying on membership changes, a single shared epoch secret, and a delivery service that never sees plaintext. Pairwise double-ratchet sessions would scale poorly for an always-on N-party voice channel.

**Why DAVE for media specifically?**
DAVE is a small, well-specified AEAD construction layered on top of MLS-exported keys, designed for streaming audio frames where you can't afford a full handshake per packet. The construction commits to the zero-padding so SFU-side framing tweaks are detectable. Aura uses DAVE's frame format (with [some deviations](docs/05_dave_protocol_deviations.md)) on top of MLS group keys.

**Why TOFU instead of a real PKI / verified identities?**
Aura targets self-hosted servers run by hobbyists for friend groups and small communities, where a "bring your own CA" story is friction nobody wants. TOFU + visible Ed25519 fingerprints is the same trust model SSH has used successfully for decades. A future version may add out-of-band fingerprint verification UI.

**Why Rust core + native UI shells instead of Electron / a single cross-platform UI?**
The audio path needs to be tight — we want sub-frame jitter, low CPU at 48 kHz, and direct access to platform audio APIs (`AVAudioEngine` on macOS, WASAPI on Windows). A Rust `aura-core` exposed via UniFFI gives us one tested implementation of the codec / crypto / jitter buffer, with idiomatic Swift and C# bindings on top.

**Is this production-ready?**
No. It is pre-alpha. Treat it as a research project until it has a third-party audit, a tagged release, and a track record.

---

## Contributing

Issue templates, contribution guide, and security disclosure policy land alongside the public release. In the meantime: bug reports as GitHub issues, security issues *privately* per the disclosure address that will live in `SECURITY.md`.

---

## AI-assisted development

Parts of this codebase were written with AI assistance (Claude Code). By project convention, individual commits do **not** carry `Co-Authored-By: Claude` trailers — this single notice covers the repository.

---

## Acknowledgments

Aura stands on a lot of other people's careful work:

- [**OpenMLS**](https://github.com/openmls/openmls) — the MLS implementation Aura uses.
- [**quinn**](https://github.com/quinn-rs/quinn) — Rust QUIC.
- [**Opus**](https://opus-codec.org/) and [**RNNoise**](https://gitlab.xiph.org/xiph/rnnoise) — audio codec and noise suppression from Xiph.
- [**ring**](https://github.com/briansmith/ring) — primitives for X25519 / Ed25519 / AEAD.
- The [**DAVE protocol**](https://github.com/discord/dave-protocol) (Discord, CC BY-NC-SA 4.0) — adapted for Aura's media frames.
- [**UniFFI**](https://github.com/mozilla/uniffi-rs) — Mozilla's binding generator that lets one Rust core back both clients.
- [**Avalonia**](https://avaloniaui.net/) — the cross-platform .NET UI framework powering the desktop client.

---

## License

Copyright 2026 Jonathon Klobucar.

Licensed under the Apache License, Version 2.0. You may obtain a copy of the License at <http://www.apache.org/licenses/LICENSE-2.0>.

Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the License for the specific language governing permissions and limitations under the License.
