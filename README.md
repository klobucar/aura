# Aura - Privacy-First Spatial Audio Platform

Aura is a next-generation VoIP platform focusing on privacy, spatial audio (Raycasting), and end-to-end encryption (MLS). This repository contains the Monorepo for the Aura server, protocol definitions, and client engines.

## Repository Structure

- **`crates/`**: Rust source code.
  - **`aura-protocol`**: Shared binary wire format definitions.
  - **`aura-server`**: UDP/QUIC relay server with spatial audio logic.
  - **`aura-core`**: Client engine library exposed via UniFFI.
- **`clients/`**: Native client applications.
  - **`macos/`**: SwiftUI-based macOS client.

## Server Configuration

The server is configured via `server.toml` in the working directory:

```toml
[server]
bind_address = "0.0.0.0:8443"
max_connections = 1000
log_level = "info"
# password = "optional-server-password"  # Uncomment to require password

[database]
path = "aura.db"

[verification]
# "none" = open server, "optional" = badge only, "required" = verified users only
mode = "optional"
```

### Verification Modes

| Mode | Description |
|------|-------------|
| `none` | Anyone can connect and join channels |
| `optional` | Users can be verified by admins (cosmetic badge) |
| `required` | Only admin-verified users can join voice channels |

### First-Time Admin Setup

On first startup with an empty database, create a bootstrap admin:

```bash
export AURA_BOOTSTRAP_ADMIN_KEY=<64-char-hex-ed25519-public-key>
export AURA_BOOTSTRAP_ADMIN_NAME="AdminUser"
cargo run -p aura-server
```

### Authentication Model

Aura uses **TOFU (Trust On First Use)** with Ed25519 keys:

1. Clients generate an Ed25519 keypair locally
2. First user to claim a display name owns it permanently (tied to their public key)
3. Subsequent connections verify signature against stored public key
4. Optional server password adds a second layer of access control

**Security Properties:**
- No passwords stored for users (cryptographic identity only)
- TOFU key pinning prevents MITM attacks
- Usernames are case-insensitive and first-come-first-served

- **Rust**: Latest stable toolchain (`rustup install stable`).
- **Xcode**: Version 15+ (for macOS client).
- **Cargo**: Included with Rust.

## Building & Testing

### Rust Backend
Run all unit tests across the workspace:
```bash
cargo test --workspace
```

### macOS Client
1. **Build Rust Static Library**:
   ```bash
   cargo build --release -p aura-core
   ```
   This produces `target/release/libaura_core.a`.

2. **Generate Swift Bindings**:
   ```bash
   cargo run -p aura-core --bin uniffi-bindgen generate crates/aura-core/src/aura.udl --language swift --out-dir clients/macos/Aura/Generated
   ```

3. **Open Xcode**:
   - Open `clients/macos/Aura/Aura.xcodeproj`.
   - Ensure `libaura_core.a` is linked and "Library Search Paths" points to `target/release`.
   - Run the app (Cmd+R).

## License

Copyright 2025 Google DeepMind

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
