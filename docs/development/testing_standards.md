---
title: Testing Standards
description: Guidelines for testing Rust code and FFI boundaries in Aura.
---
# Testing Standards

Aura maintains a rigorous testing standard to ensure the stability of the zero-trust relay and the security of the E2EE implementation.

## 1. Rust Unit & Integration Tests

All Rust code must include tests in the same file (for private logic) or in `tests/` (for public APIs).

### Workspace Testing
Run all tests across all crates:
```bash
make test
# or
cargo test --workspace
```

### Protocol Fuzzing
The `aura-protocol` crate uses `libfuzzer` to ensure the binary parser is resilient against malformed packets.
- **Location**: `docs/FUZZING.md`
- **Goal**: 100% coverage of the `FastAudioPacket::parse` function.

## 2. FFI & Client Testing

### UniFFI Verification
Since we bridge to Swift and C#, we must verify the bridge itself.
- **Pattern**: Create a "Mock Delegate" in Rust tests to verify that callbacks are triggered correctly on the other side.
- **Swift**: Located in `clients/macos/AuraTests/`.

## 3. Security Verification (DAVE)

Critical security logic (like the Zero-Padding Commitment) must be tested against **Negative Cases**:
- **Mismatching Epochs**: Verify packets are dropped if the `epoch_hint` refers to a key the client hasn't derived yet.
- **Padding Attack**: Manually craft a packet with valid `XChaCha20` tag but invalid padding; verify it is rejected.

## 4. Performance Benchmarks

For the `aura-server` relay, we use `criterion` to measure:
- **Relay Latency**: Time from datagram receipt to fan-out.
- **Lookup Speed**: Performance of the `SessionID` map under high concurrency.

Run benchmarks with:
```bash
cargo bench -p aura-server
```

## 5. Code Coverage

We target **95% coverage** for `aura-core` and `aura-protocol`.
- Review previous coverage reports or run:
```bash
cargo tarpaulin
```
