# Aura Product Roadmap

**Goal**: Achieve Mumble feature parity with enhanced privacy (MLS E2EE) and modern UX.

---

## Current State ✅

### Server
- ✅ QUIC transport with TLS 1.3
- ✅ Ed25519 TOFU authentication
- ✅ SQLite persistence (users, admins, profiles)
- ✅ Channel hierarchy
- ✅ Admin verification system
- ✅ UUID-based user identification

### Core Library
- ✅ Opus audio codec
- ✅ DAVE encryption (XChaCha20-Poly1305)
- ✅ MLS client (group lifecycle complete)
- ✅ Text message encryption
- ✅ Jitter buffer
- ✅ UniFFI bindings (Swift/C#)

### Clients
- ✅ macOS SwiftUI client (voice, text, channels, settings, profiles, TOFU pinning, Secure Enclave)
- ✅ Avalonia (.NET 10) desktop client with shared UniFFI core (voice + text parity for Windows/Linux/macOS)

---

## Phase 1: Core Voice Chat 🎯 **[NEXT]**

**Goal**: Functional voice communication with E2EE

### 1.1 Audio Pipeline Integration
- [x] Wire audio encryption (Phase 1: Session Key)
- [x] Implement audio capture (AVAudioEngine)
- [x] Implement audio playback with jitter buffer
- [ ] Add VAD (Voice Activity Detection)
- [x] Test: Two clients can talk with E2EE

### 1.2 Server Audio Relay
- [x] Fast path audio routing (opaque datagrams)
- [x] Session heartbeat/keepalive
- [x] Handle client disconnects gracefully
- [ ] Bandwidth tracking per session

### 1.3 Client UI - Voice
- [x] Push-to-talk keybind (global hotkey, recordable in Settings → Audio)
- [x] Voice activity indicator
- [x] Audio settings (input/output device selection)
- [x] Per-user local volume sliders + local mute (persisted across reconnects)
- [ ] Input gain / master output volume

**Milestone**: Two users can join a channel and talk with E2EE ✅

---

## Phase 2: Text Chat 💬 ✅ **[COMPLETE]**

**Goal**: IRC-style encrypted text messaging

### 2.1 Server Text Relay
- ✅ `broadcast_text_message()` in state.rs
- ✅ Route `EncryptedTextPacket` to online channel members
- ✅ Handle text group membership
- ✅ Batched ratcheting (50 messages or 5 minutes)

### 2.2 Client UI - Text
- ✅ Chat message list view
- ✅ Text input field
- ✅ Message bubbles (sender/timestamp)
- ✅ macOS-native styling (blue gradient outgoing, grey incoming)
- ✅ Text selection support
- ✅ Reply-to threading
- ✅ Basic markdown rendering

**Milestone**: Users can send/receive encrypted text in channels ✅

**Note**: Currently using plaintext for testing. MLS-derived DAVE encryption ready but not wired up yet.

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
- [ ] Delete channels (Admin)
- [ ] Temporary channels
- [x] Channel descriptions/MOTD (via `comment` field)
- [ ] ACL system (view/speak/enter permissions)

### 4.2 User Management
- [x] User list with online status (Server-side tracking complete)
- [x] Mute/deafen (server-broadcast status + local-only per-user mute)
- [x] User comments/avatars (Protocol & DB storage ready)
- [ ] Friend system
- [ ] User registration tokens

### 4.3 Audio Quality
- [ ] Configurable bitrate (8-128 kbps)
- [ ] Audio preprocessing (noise suppression, AGC)
- [ ] Echo cancellation
- [ ] Stereo/mono toggle

### 4.4 Recording & Playback
- [ ] Local recording (encrypted)
- [ ] Audio file playback to channel
- [ ] Text-to-speech integration

**Milestone**: Feature parity with Mumble 1.4.x

---

## Phase 5: Modern Enhancements ✨

### 5.1 Rich Media
- [ ] Image/file sharing (encrypted)
- [ ] Voice message recording
- [ ] Screen sharing (video codec)
- [ ] Emoji reactions

### 5.2 Mobile Clients
- [ ] iOS app (Swift)
- [ ] Android app (Kotlin)
- [ ] Push notifications for mentions

### 5.3 Advanced Privacy
- [ ] Onion routing option (Tor integration)
- [ ] Metadata minimization
- [ ] Disappearing messages
- [ ] Anonymous mode (no persistent identity)

### 5.4 Developer Experience
- [ ] Plugin API (WASM?)
- [ ] Bot framework
- [ ] Webhooks for integrations
- [ ] Prometheus metrics

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
- [ ] Comprehensive integration tests
- [ ] Fuzzing for protocol parsing
- [ ] Security audit (external)
- [ ] Performance benchmarks
- [ ] CI/CD pipeline (GitHub Actions)
- [ ] Docker images for server
- [ ] Kubernetes deployment manifests

### Documentation
- [ ] API documentation (rustdoc)
- [ ] User manual
- [ ] Admin guide
- [ ] Protocol specification
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
