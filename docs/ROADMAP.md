# Aura Product Roadmap

**Goal**: Achieve Mumble feature parity with enhanced privacy (MLS E2EE) and modern UX.

---

## Current State ✅

### Server
- ✅ QUIC transport with TLS 1.3 (hybrid X25519MLKEM768 where the client offers it), ACME certificates
- ✅ Hybrid Ed25519 + ML-DSA-65 identities, challenge-response login, TOFU pinning
- ✅ SQLite persistence (users, admins, profiles)
- ✅ Channel hierarchy
- ✅ Admin verification system
- ✅ UUID-based user identification
- ✅ Moderation over the wire: list, verify, ban, kick, grant admin, delete account (`0x44`, `0x48`-`0x4C`)
- ✅ Account deletion (self-service)
- ✅ Handshake and ping rate limiting; display-name hardening (look-alikes, reserved names)
- ✅ Prometheus `/metrics` endpoint; opt-in OpenTelemetry tracing
- ✅ Docker image; Fly.io deployment

### Core Library
- ✅ Opus audio codec
- ✅ DAVE encryption (XChaCha20-Poly1305)
- ✅ MLS client (group lifecycle complete), hybrid post-quantum ciphersuite pinned
- ✅ MLS member removal when someone leaves a channel (`MlsSyncMembers`, `0x58`)
- ✅ Text message encryption (MLS-derived per-sender keys)
- ✅ Jitter buffer
- ✅ Voice activity detection, RNNoise noise suppression, optional WebRTC AEC/NS/AGC (`webrtc-audio` feature)
- ✅ Safety words (BIP39) from the authenticated ratchet tree
- ✅ Passphrase-encrypted identity backups (scrypt + XChaCha20-Poly1305)
- ✅ UniFFI bindings (Swift/C#)

### Clients
- ✅ macOS SwiftUI client (voice, text, channels, settings, profiles, TOFU pinning, Secure Enclave, Swift 6)
- ✅ Avalonia (.NET 11) desktop client with shared UniFFI core (voice + text parity for Windows/Linux/macOS)
- ✅ Both clients: `aura://` invite links, mentions and notifications, identity backup/restore, account deletion, key-change trust sheet with safety words, layout modes and themes
- ❌ Neither client has a moderation UI yet (see Phase 4.2)

---

## Phase 1: Core Voice Chat 🎯 **[MOSTLY COMPLETE]**

**Goal**: Functional voice communication with E2EE

### 1.1 Audio Pipeline Integration
- [x] Wire audio encryption (Phase 1: Session Key)
- [x] Implement audio capture (AVAudioEngine)
- [x] Implement audio playback with jitter buffer
- [x] Add VAD (Voice Activity Detection) — Rust core (`vad.rs`), used by both clients in voice-activation mode
- [x] Test: Two clients can talk with E2EE

### 1.2 Server Audio Relay
- [x] Fast path audio routing (opaque datagrams)
- [x] Session heartbeat/keepalive
- [x] Handle client disconnects gracefully
- [ ] Bandwidth tracking per session — counted per session and logged at disconnect, plus a global `aura_audio_bytes_total` metric; no per-session query or limit

### 1.3 Client UI - Voice
- [x] Push-to-talk keybind (recordable in Settings → Audio) — macOS global; desktop global on Windows only (low-level hook, never run on Windows), window-focused elsewhere
- [x] Voice activity indicator
- [x] Audio settings (input/output device selection)
- [x] Per-user local volume sliders + local mute (persisted across reconnects)
- [x] Master output volume — both clients
- [ ] Input gain (microphone level; only WebRTC AGC exists)

**Milestone**: Two users can join a channel and talk with E2EE ✅

---

## Phase 2: Text Chat 💬 ✅ **[COMPLETE]**

**Goal**: IRC-style encrypted text messaging

### 2.1 Server Text Relay
- ✅ `broadcast_text_message()` in state.rs
- ✅ Route `EncryptedTextPacket` to online channel members
- ✅ Handle text group membership
- ✅ Batched ratcheting (50 messages or 5 minutes)
- ✅ Bounded replay protection for message ids

### 2.2 Client UI - Text
- ✅ Chat message list view
- ✅ Text input field
- ✅ Message bubbles (sender/timestamp)
- ✅ macOS-native styling (blue gradient outgoing, grey incoming)
- ✅ Text selection support
- ✅ Reply-to threading
- ✅ Basic markdown rendering
- ✅ Mentions with keyword notifications (level: everything / mentions / nothing)
- ❌ Reactions, typing indicators, message history (nothing is stored; by design)

**Milestone**: Users can send/receive encrypted text in channels ✅

**Note**: Text is end-to-end encrypted with DAVE using MLS-derived per-sender
keys; the server relays ciphertext only. Messages carry the epoch they were
encrypted under, and clients retain keys for the last few epochs so a message in
flight across a membership change still decrypts. `reply_to_id` is sent in the
clear.

---

## Phase 3: Spatial Audio 🎧

**Goal**: Positional audio (Mumble's killer feature)

### 3.1 Raycasting Engine
- [ ] 3D position tracking per user
- [ ] Attenuation based on distance
- [ ] Occlusion/obstruction simulation
- [ ] Stereo panning based on relative position

### 3.2 Client Integration
- [ ] Position update protocol
- [ ] UI: 2D/3D position visualizer
- [ ] Configurable audio falloff curves

**Milestone**: Spatial audio works in a test map

---

## Phase 4: Mumble Parity Features 📋

### 4.1 Channel Management
- [x] Create/Update channels (Admin)
- [x] Move between channels (via `MSG_JOIN_CHANNEL`)
- [x] Delete channels (Admin) — occupants are evicted and moved to the lobby
- [ ] Temporary channels
- [x] Channel hierarchy in the protocol and DB (`parent_id`); no nested UI
- [x] Channel descriptions/MOTD (via `comment` field)
- [ ] ACL system (view/speak/enter permissions)

### 4.2 User Management
- [x] User list with online status (Server-side tracking complete)
- [x] Mute/deafen (server-broadcast status + local-only per-user mute)
- [x] User comments/avatars (Protocol & DB storage ready)
- [x] Account deletion (self-service; admins can delete others)
- [x] Admin moderation protocol: list, verify, ban (ends live connections), kick, grant admin — server only
- [ ] Moderation console UI — macOS and desktop (in progress)
- [ ] Profile hardening (in progress): server must not rebroadcast the client-supplied `display_name` in `ProfileUpdated`; enforce avatar (128 KB) and bio (512 KB) limits and an image-type check; clients verify profile signatures (`verify_profile` has no caller)
- [x] Invite links (`aura://`, certificate fingerprint pinned in every build)
- [ ] Friend system
- [ ] User registration tokens
- [ ] Direct messages (`dm_groups` / `dm_messages` tables exist; no handlers or UI)

### 4.3 Audio Quality
- [ ] Configurable bitrate (8-128 kbps) — Opus encoder takes a bitrate (default 64 kbps); no setting exposed
- [x] Audio preprocessing (noise suppression, AGC) — RNNoise and WebRTC NS/AGC toggles on macOS and desktop
- [x] Echo cancellation — WebRTC AEC toggle on both clients; needs a `webrtc-audio` core build
- [ ] Stereo/mono toggle

### 4.4 Recording & Playback
- [ ] Local recording (encrypted)
- [ ] Audio file playback to channel
- [ ] Text-to-speech integration — macOS ✅ (`TtsManager`, speaks chat and join/leave); desktop not started

**Milestone**: Feature parity with Mumble 1.4.x

---

## Phase 5: Modern Enhancements ✨

### 5.1 Rich Media
- [ ] Image/file sharing (encrypted) — needs a blob stream (control frames are capped at 256 KB); client encrypts with the text-group key, server relays opaque chunks and stores nothing. `MediaType` and attachment fields exist in `TextMessage`; nothing sends them
- [ ] Voice message recording
- [ ] Screen sharing (video codec)
- [ ] Emoji reactions

### 5.2 Mobile Clients
- [ ] iOS app (Swift)
- [ ] Android app (Kotlin)
- [ ] Push notifications for mentions (macOS and desktop local notifications ✅; no mobile client or push path)

### 5.3 Advanced Privacy
- [ ] Onion routing option (Tor integration)
- [ ] Metadata minimization
- [ ] Disappearing messages (nothing is stored server-side today)
- [ ] Anonymous mode (no persistent identity)
- [x] Hybrid post-quantum identities and key exchange
- [x] Key-change trust sheet and safety-word verification — both clients

### 5.4 Developer Experience
- [ ] Plugin API (WASM?)
- [ ] Bot framework
- [ ] Webhooks for integrations
- [x] Prometheus metrics (`/metrics`)
- [x] OpenTelemetry tracing (opt-in, server and clients)

---

## Phase 6: Scale & Performance 🚀

### 6.1 Server Clustering
- [ ] Multi-server federation
- [ ] Load balancing
- [ ] Geographic routing
- [ ] Horizontal scaling

### 6.2 Optimization
- [ ] Zero-copy audio path
- [ ] SIMD audio processing
- [ ] GPU-accelerated spatial audio
- [ ] Connection migration (QUIC)

---

## Technical Debt & Infrastructure 🔧

### Ongoing
- [ ] Comprehensive integration tests — server has `state_tests.rs` and `acme_tests.rs`; no end-to-end client-to-server test
- [ ] Fuzzing for protocol parsing — `docs/FUZZING.md` is a plan; there are no fuzz targets in the repo
- [ ] Security audit (external)
- [ ] Performance benchmarks
- [x] CI (GitHub Actions): `cargo fmt` and `clippy -D warnings`, `cargo test`, macOS `xcodebuild test`, desktop `dotnet test`, CodeQL, cargo-audit/deny, zizmor, commitlint
- [ ] Make the macOS and desktop jobs required checks (they are path-filtered on PRs; branch protection isn't in the repo)
- [ ] Release pipeline: tags, signed server binaries and client builds (no release workflow)
- [x] Docker image for server
- [ ] Kubernetes deployment manifests
- [ ] Close open items in `docs/08_security_review.md`: A3 Debug builds accept any certificate; A6 profile signatures unchecked; A7 old plaintext exports. Update A1/A2, which it still lists as in review though #64 merged
- [ ] Run the Windows-only paths on Windows: global push-to-talk hook, `aura://` registration

### Documentation
- [ ] API documentation (rustdoc)
- [ ] User manual
- [ ] Admin guide
- [ ] Protocol specification — `docs/protocol.md` and `docs/protocol/control_stream.md` exist; the latter still describes some pre-protobuf layouts
- [ ] Security whitepaper

---

## Success Metrics

| Metric | Target |
|--------|--------|
| Audio latency (P99) | < 50ms |
| Concurrent users per server | 500+ |
| CPU usage per user | < 1% |
| Memory per user | < 10MB |
| Packet loss tolerance | Up to 5% |
| Test coverage | > 80% |

---

## Release Strategy

### v0.1.0 - Alpha (Phase 1 complete)
- Basic voice chat with E2EE
- macOS + Windows clients
- Single-server deployment

### v0.2.0 - Beta (Phase 2-3 complete)
- Text chat + spatial audio
- Mumble migration path
- Public testing

### v1.0.0 - Stable (Phase 4 complete)
- Full Mumble parity
- Production-ready
- Security audit completed

### v2.0.0+ - Beyond (Phase 5-6)
- Modern features
- Mobile apps
- Federation
