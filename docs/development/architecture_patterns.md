---
title: Architecture Patterns
description: Core design patterns and architectural decisions in the Aura codebase.
agent_intent: onboarding_guide
---
# Architecture Patterns

This document describes the high-level design patterns and architectural decisions that define the Aura codebase. It is intended for both human developers and AI agents to understand how code should be "crafted" in this project.

## 1. Monorepo Organization

Aura uses a Rust-centric monorepo structure to ensure type safety across the entire stack:

- **`crates/aura-protocol`**: The source of truth for all wire formats. Uses standard Rust structs for "hot path" data (QUIC datagrams) and Protobuf (via `prost`) for control signaling.
- **`crates/aura-server`**: An asynchronous, `tokio`-based relay. It follows the **"Control/Data Plane"** separation:
    - **Control Plane**: Reliable QUIC streams for join/leave, authentication, and MLS handshakes.
    - **Data Plane (Relay)**: Unreliable QUIC datagrams for opaque audio blobs. The server *never* decrypts audio.
- **`crates/aura-core`**: The client-side engine. It encapsulates the complex state machines for MLS, jitter buffering, and audio processing.
- **`clients/`**: Native wrappers that handle the platform-specific UI and audio hardware interaction.

## 2. Cross-Language Bridge (UniFFI)

Aura uses **UniFFI** to expose the Rust `aura-core` to Swift (macOS) and C# (Windows).

### Pattern: The Arc-based Object
Exported objects in `aura-core` should generally be wrapped in `Arc` and use internal mutability (`Mutex`, `Atomic`) to ensure thread safety across the FFI boundary.

```rust
#[derive(uniffi::Object)]
pub struct AuraClient {
    connected: AtomicBool,
    position: Mutex<Position>,
    // ...
}
```

### Pattern: The Callback Delegate
To push events from Rust to the UI (e.g., "User Joined"), use the `uniffi::callback_interface`.

- **Rule**: Always assume callbacks are invoked on a background thread. UI clients **must** dispatch to the main thread.

## 3. Zero-Trust Relay Pattern

The server acts as a **Subject-Blind Relay**.

- **Pattern**: `SessionID` lookup. The server maps a QUIC `Connection` to a `SessionID`. When a datagram arrives, the server performs a lock-free lookup to find other participants in the same channel and fans out the packet.
- **Invariance**: The server must never require the decryption key for any audio packet. All routing metadata (Epoch Hint, SessionID) must be in the unencrypted header.

## 4. State Management

### Server State
`ServerState` in `aura-server` is a globally shared `Arc` containing:
- `ClientSession` map: Active connections.
- `Channel` map: Participants in each channel.
- `Database`: SQLite persistence for TOFU identities.

### Client State
The `AuraClient` manages two independent MLS groups:
1. **Voice Group**: Optimized for low churn. Epochs advance when users join/leave the voice channel.
2. **Text Group**: High churn. May advance more frequently to support reliable message ordering and transcript security.

## 5. Error Handling

- **Rust**: Use `thiserror` for library errors and `anyhow` for application-level logic (server).
- **FFI**: Always map Rust errors to `uniffi::Error` enums to provide rich error information to Swift/C#.
