# Run Aura locally

Build Aura from source, start a server on your machine, and try voice and text in the Lounge.

> **Pre-alpha.** Keep sensitive conversations off Aura while it’s experimental. [Security review](08_security_review.md)

## Prerequisites

Install stable Rust and the Protocol Buffers compiler (`protoc`).

- On macOS, run `brew install protobuf`. If Cargo cannot find it, set `PROTOC` to the path returned by `command -v protoc`.
- On Debian or Ubuntu, install `protobuf-compiler`, `libasound2-dev`, and `pkg-config`.
- For the native macOS app, use Xcode on macOS 14 or later.
- For the desktop app, install the .NET 10 SDK. Windows requires Windows 11 or Windows Server 2022 or later. Linux requires `libmsquic` 2.2 or later.

## 1. Start the server

Run this from the repository root to listen on the local machine only.

```bash
AURA_BIND_ADDRESS=127.0.0.1:8443 cargo run -p aura-server
```

In PowerShell, use these two commands.

```powershell
$env:AURA_BIND_ADDRESS = "127.0.0.1:8443"
cargo run -p aura-server
```

The server stores its state in `aura.db` and creates a **Lounge** channel on a fresh database. Keep the terminal open while you try the clients. [Server configuration](../server.toml)

## 2. Open a client

### Native macOS app

Open [Aura.xcodeproj](../clients/macos/Aura.xcodeproj) and run the **Aura** scheme. Xcode builds the Rust library and generates the Swift bindings as part of the build. [Xcode build notes](../clients/macos/Aura/RUNNING.md)

### .NET desktop app

From the repository root, build the Rust library.

```bash
cargo build --release -p aura-core
```

Copy the file for your platform into `clients/desktop/Generated/`.

| Platform | File |
| --- | --- |
| Windows | `target/release/aura_core.dll` |
| Linux | `target/release/libaura_core.so` |
| macOS development | `target/release/libaura_core.dylib` |

For .NET development on macOS, install `libmsquic` with Homebrew and use `./run.sh` from `clients/desktop/`. On Windows or Linux, launch from the repository root.

```bash
dotnet run --project clients/desktop/Aura.Desktop.csproj
```

## 3. Join the Lounge

Add a server profile for `localhost:8443`, choose a display name using printable ASCII characters, and join **Lounge**. Connect a second client with a different display name and join the same channel. Send a message or use push-to-talk to try the connection in both directions.

## Optional admin setup

To create the first admin, set `AURA_BOOTSTRAP_ADMIN_KEY` to your client’s 64-character hex Ed25519 public key and `AURA_BOOTSTRAP_ADMIN_NAME` to its display name before starting the server with an empty database. Admins can verify users. The `verification.mode` setting controls whether verification is disabled, optional, or required to join voice channels. [Server configuration](../server.toml)

## Developer checks

Run Rust workspace tests with `cargo test --workspace`. After setting up the desktop library, run desktop unit tests with `dotnet test clients/desktop/Tests/Aura.Desktop.Tests.csproj`. [Contributing guide](../CONTRIBUTING.md) · [Fuzzing guide](FUZZING.md)
