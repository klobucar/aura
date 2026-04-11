---
title: Fuzzing Guide
description: Documentation for protocol fuzzing using libfuzzer.
agent_intent: testing_guide
---
# Fuzzing Aura Protocol

## Overview

Fuzzing is critical for finding edge cases, security vulnerabilities, and crashes in the Aura codebase. This document outlines a comprehensive fuzzing strategy for both Rust and Swift components.

## Rust Fuzzing with cargo-fuzz

### Setup

```bash
cargo install cargo-fuzz
```

### Targets to Fuzz

#### 1. Protocol Parsing (High Priority)
**Target**: Message deserialization from network
- **Input**: Raw bytes from QUIC streams
- **Why**: Untrusted network data, potential for crashes/exploits
- **Fuzz**: Protobuf parsing, packet headers, malformed data

```rust
// fuzz/fuzz_targets/parse_server_message.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to parse as ServerMessage
    let _ = ServerMessage::decode(data);
});
```

#### 2. Audio Pipeline (High Priority)
**Target**: Opus decoding, jitter buffer, audio processing
- **Input**: Opus-encoded audio frames
- **Why**: Audio data from network, potential for buffer overflows
- **Fuzz**: Invalid Opus frames, extreme packet loss, out-of-order packets

```rust
// fuzz/fuzz_targets/audio_decode.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut decoder = OpusDecoder::new();
    let _ = decoder.decode(data);
});
```

#### 3. MLS Protocol (Critical)
**Target**: MLS message processing, key derivation
- **Input**: MLS Welcome, Commit, Proposal messages
- **Why**: Cryptographic operations, security-critical
- **Fuzz**: Malformed MLS messages, invalid epochs, corrupted ciphertexts

```rust
// fuzz/fuzz_targets/mls_process.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut group = MlsGroup::new();
    let _ = group.process_message(data);
});
```

#### 4. DAVE Protocol (Critical)
**Target**: Per-sender key derivation, nonce generation
- **Input**: Session IDs, sequence numbers, key material
- **Why**: Encryption security depends on correct implementation
- **Fuzz**: Duplicate nonces, key reuse, invalid session IDs

```rust
// fuzz/fuzz_targets/dave_crypto.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() >= 40 {
        let session_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let seq = u64::from_le_bytes([data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11]]);
        let key = &data[12..44]; // 32 bytes
        
        let _ = derive_per_sender_key(session_id, seq, key);
    }
});
```

#### 5. Text Encryption (High Priority)
**Target**: ChaCha20-Poly1305 encryption/decryption
- **Input**: Plaintext, ciphertext, keys, nonces
- **Why**: Security-critical, potential for timing attacks
- **Fuzz**: Invalid nonces, corrupted ciphertexts, wrong key lengths

#### 6. Server State Management (Medium Priority)
**Target**: Channel/user state updates
- **Input**: State update messages
- **Why**: Potential for race conditions, inconsistent state
- **Fuzz**: Concurrent updates, invalid user IDs, duplicate channels

### Fuzzing Configuration

Create `fuzz/Cargo.toml`:
```toml
[package]
name = "aura-fuzz"
version = "0.0.0"
publish = false
edition = "2021"

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
aura-core = { path = "../crates/aura-core" }

[[bin]]
name = "parse_server_message"
path = "fuzz_targets/parse_server_message.rs"
test = false
doc = false

[[bin]]
name = "audio_decode"
path = "fuzz_targets/audio_decode.rs"
test = false
doc = false

[[bin]]
name = "mls_process"
path = "fuzz_targets/mls_process.rs"
test = false
doc = false
```

### Running Fuzz Tests

```bash
# Run specific target
cargo fuzz run parse_server_message

# Run with corpus
cargo fuzz run parse_server_message corpus/

# Run for specific duration
cargo fuzz run parse_server_message -- -max_total_time=3600

# Minimize corpus
cargo fuzz cmin parse_server_message

# Triage crashes
cargo fuzz tmin parse_server_message crash-file
```

### Continuous Fuzzing

Set up OSS-Fuzz integration for continuous fuzzing:
```yaml
# .github/workflows/fuzz.yml
name: Continuous Fuzzing
on:
  schedule:
    - cron: '0 0 * * *'  # Daily
jobs:
  fuzz:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - run: cargo install cargo-fuzz
      - run: |
          for target in parse_server_message audio_decode mls_process; do
            cargo fuzz run $target -- -max_total_time=3600
          done
```

## Swift Fuzzing

### Approach 1: XCTest with Random Data

```swift
// AuraTests/FuzzTests.swift
import XCTest
@testable import Aura

final class FuzzTests: XCTestCase {
    
    func testServerProfileFuzzing() {
        for _ in 0..<1000 {
            let randomName = randomString(length: Int.random(in: 0...1000))
            let randomHost = randomString(length: Int.random(in: 0...255))
            let randomPort = UInt16.random(in: 0...65535)
            
            let server = ServerProfile(
                name: randomName,
                host: randomHost,
                port: randomPort
            )
            
            let manager = ServerManager()
            manager.addServer(server)
            
            // Should not crash
            XCTAssertTrue(true)
        }
    }
    
    func testProfileImportFuzzing() {
        for _ in 0..<1000 {
            let randomData = randomData(length: Int.random(in: 0...10000))
            
            // Should handle gracefully
            let result = UserIdentity.importProfile(from: randomData)
            // Most should fail, but shouldn't crash
        }
    }
    
    private func randomString(length: Int) -> String {
        let letters = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 !@#$%^&*()"
        return String((0..<length).map { _ in letters.randomElement()! })
    }
    
    private func randomData(length: Int) -> Data {
        var data = Data(count: length)
        data.withUnsafeMutableBytes { ptr in
            arc4random_buf(ptr.baseAddress, length)
        }
        return data
    }
}
```

### Approach 2: Property-Based Testing with SwiftCheck

```swift
import SwiftCheck
@testable import Aura

class PropertyTests: XCTestCase {
    func testServerProfileRoundTrip() {
        property("Server profile encode/decode round-trip") <- forAll { (name: String, host: String, port: UInt16) in
            let server = ServerProfile(name: name, host: host, port: port)
            let encoded = try? JSONEncoder().encode(server)
            let decoded = try? JSONDecoder().decode(ServerProfile.self, from: encoded!)
            return decoded?.name == name && decoded?.host == host && decoded?.port == port
        }
    }
}
```

## Coverage-Guided Fuzzing

### libFuzzer Integration

For Rust, use libFuzzer with sanitizers:

```bash
# Address Sanitizer (memory safety)
RUSTFLAGS="-Z sanitizer=address" cargo fuzz run parse_server_message

# Memory Sanitizer (uninitialized memory)
RUSTFLAGS="-Z sanitizer=memory" cargo fuzz run parse_server_message

# Thread Sanitizer (data races)
RUSTFLAGS="-Z sanitizer=thread" cargo fuzz run parse_server_message

# Undefined Behavior Sanitizer
RUSTFLAGS="-Z sanitizer=undefined" cargo fuzz run parse_server_message
```

## Differential Fuzzing

Compare implementations across platforms:

```rust
// Test that Swift and Rust produce same results
fuzz_target!(|data: &[u8]| {
    let rust_result = rust_parse_message(data);
    let swift_result = call_swift_parse_message(data);
    assert_eq!(rust_result, swift_result);
});
```

## Security-Focused Fuzzing

### Timing Attack Detection

```rust
use std::time::Instant;

fuzz_target!(|data: &[u8]| {
    let start = Instant::now();
    let _ = constant_time_compare(data);
    let duration = start.elapsed();
    
    // Flag if timing varies significantly
    assert!(duration.as_micros() < 1000);
});
```

### Nonce Reuse Detection

```rust
use std::collections::HashSet;

fuzz_target!(|data: &[u8]| {
    static mut NONCES: Option<HashSet<Vec<u8>>> = None;
    
    unsafe {
        if NONCES.is_none() {
            NONCES = Some(HashSet::new());
        }
        
        let nonce = generate_nonce(data);
        assert!(!NONCES.as_ref().unwrap().contains(&nonce), "Nonce reuse detected!");
        NONCES.as_mut().unwrap().insert(nonce);
    }
});
```

## Corpus Management

### Seed Corpus

Create initial corpus from real-world data:
```bash
# Capture real network traffic
tcpdump -i any -w aura-traffic.pcap port 8443

# Extract payloads
tcpdump -r aura-traffic.pcap -x > corpus/real-traffic-*.bin

# Add hand-crafted edge cases
echo -n "\x00\x00\x00\x00" > corpus/all-zeros
echo -n "\xff\xff\xff\xff" > corpus/all-ones
```

### Corpus Minimization

```bash
# Reduce corpus to minimal set
cargo fuzz cmin parse_server_message

# Merge new findings
cargo fuzz cmin parse_server_message corpus/ new-corpus/
```

## Crash Triage

When fuzzing finds a crash:

1. **Reproduce**: `cargo fuzz run target crash-file`
2. **Minimize**: `cargo fuzz tmin target crash-file`
3. **Debug**: `rust-lldb target/debug/target crash-file`
4. **Fix**: Create regression test
5. **Verify**: Re-run fuzzer to ensure fix

## Metrics and Reporting

Track fuzzing effectiveness:
- **Coverage**: Lines/branches covered
- **Crashes**: Unique crashes found
- **Corpus size**: Number of interesting inputs
- **Executions/sec**: Fuzzing throughput

```bash
# Generate coverage report
cargo fuzz coverage parse_server_message
llvm-cov show target/coverage/parse_server_message -format=html > coverage.html
```

## Integration with CI/CD

```yaml
# .github/workflows/fuzz-pr.yml
name: Fuzz PR Changes
on: pull_request
jobs:
  fuzz:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - run: cargo fuzz run parse_server_message -- -max_total_time=300
      - run: cargo fuzz run audio_decode -- -max_total_time=300
```

## Best Practices

1. **Start with high-value targets**: Protocol parsing, crypto, audio
2. **Use sanitizers**: ASAN, MSAN, TSAN, UBSAN
3. **Maintain corpus**: Keep successful inputs for regression testing
4. **Continuous fuzzing**: Run overnight, on CI/CD
5. **Triage quickly**: Fix crashes as they're found
6. **Document findings**: Create issues for each unique crash
7. **Regression tests**: Add crash inputs to test suite

## Expected Findings

Common issues fuzzing typically finds:
- Buffer overflows
- Integer overflows
- Null pointer dereferences
- Assertion failures
- Infinite loops
- Memory leaks
- Uninitialized memory
- Race conditions
- Cryptographic vulnerabilities
