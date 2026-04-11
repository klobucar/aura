# Welcome to Aura

Aura is a next-generation VoIP platform focusing on **privacy**, **spatial audio (Raycasting)**, and **end-to-end encryption (MLS)**.

This documentation portal provides deep technical insights into how Aura works, how to build it, and how to contribute to its ecosystem.

## Key Features

- **End-to-End Encryption**: Built on the MLS (Messaging Layer Security) and DAVE protocols.
- **Ultra-Low Latency**: Native QUIC transport optimized for audio datagrams.
- **Spatial Audio**: Real-time raycasting for immersive voice environments.
- **Privacy First**: TOFU (Trust-On-First-Use) authentication model with Ed25519 identities.

## Quick Start

- **Architecture Overview**: Understand the [Server Architecture](diagrams/02_server_architecture.md) and [Client Design](diagrams/03_client_architecture.md).
- **Protocol Flow**: See how connections are established in the [Protocol Flow](diagrams/01_protocol_flow.md).
- **Development**: Learn about our [Architecture Patterns](development/architecture_patterns.md) and [Testing Standards](development/testing_standards.md).

## Project Structure

Aura is organized as a monorepo:

- `crates/`: Rust source code for server, protocol, and core library.
- `clients/`: Native client applications (Swift/macOS, C#/Desktop).
- `docs/`: Technical specifications and diagrams.
- `scripts/`: Utility scripts for development and deployment.
