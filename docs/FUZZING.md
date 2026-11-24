---
title: Fuzzing Guide
description: How Aura's cargo-fuzz targets are laid out, how to run them, and what they cover.
---
# Fuzzing Aura

The `fuzz/` directory is a standalone [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz)
crate (`aura-fuzz`). It has its own `[workspace]` and `Cargo.lock`, and the root
`Cargo.toml` lists it under `exclude`, so `cargo build --workspace` and
`cargo test --workspace` never touch it.

Each target feeds attacker-controlled bytes into code that parses or acts on
untrusted input: network messages, key and identity files, MLS messages, and
decrypted audio.

## Running

```bash
cargo install cargo-fuzz --locked --version 0.13.1

# Build every target (AddressSanitizer + libFuzzer coverage).
cargo fuzz build

# Run one target for 60 seconds.
cargo fuzz run protocol_decode fuzz/corpus/protocol_decode fuzz/seeds/protocol_decode \
    -- -max_total_time=60 -max_len=4096 -rss_limit_mb=4096
```

Requirements:

- A nightly toolchain. CI pins `nightly-2026-09-01`; use the same one locally
  (`rustup toolchain install nightly-2026-09-01`, then `cargo +nightly-2026-09-01 fuzz ...`).
  Older nightlies have hit compiler crashes in tokio under `-Zsanitizer=address`.
- The same build dependencies as the main workspace (`protoc`, a C toolchain,
  and the Opus DNN model data; see `ci.yml`).
- Several GB of disk for the sanitizer build in `fuzz/target`.

`fuzz/corpus/` (libFuzzer's working corpus) and `fuzz/artifacts/` (crashes,
timeouts, OOMs) are not committed. `fuzz/seeds/<target>/` holds small committed
seed inputs that are passed as read-only extra corpus directories.

`aura-core` is built with `default-features = false`, which leaves out the
native audio backend (cpal/ALSA). Nothing fuzzed uses it.

## Targets

| Target | What it exercises |
| --- | --- |
| `protocol_decode` | Decodes about 38 `aura-protocol` message types (selected by the first input byte), checks that re-encoding is stable, and round-trips the `FastAudioPacket` header. |
| `server_control` | Builds an in-memory `ServerState` with one admin and two plain-user sessions, then replays a control frame (`[wire type][actor][payload]`) through the same decode and `ServerState`/`AuthService` calls that `connection.rs` makes: join, audio route, text, channel create/update/delete, profile, delete-user, status, list-users, verify, ban, grant-admin, kick, and the MLS handlers. Not covered: the QUIC frame reader, which needs a live `RecvStream`. |
| `server_auth` | Raw `AuthRequest` bytes into `authenticate`; correctly signed logins with fuzzed display names (so name validation is reached past the signature check); corrupted signatures and short challenges; re-login. |
| `core_decode` | All the UniFFI `decode_*` functions that the Swift/Kotlin clients call. |
| `text_decrypt` | `decrypt_text` with a fixed key, given raw protobuf and structured packets. |
| `audio_receive` | `AudioReceiver` driven by a stream of operations: packets encrypted under the real sender key with arbitrary payloads (so they reach the Opus decoder), epoch keys, gain and mute, mixing. |
| `jitter_buffer` | Arbitrary configuration plus push/pop/reset sequences. Sequence numbers are limited to the u16 range that production uses. |
| `identity_backup` | `decrypt_identity_backup` with an arbitrary file and passphrase. If the header asks for scrypt parameters the importer accepts, they are lowered to the cheapest accepted values so the target stays fast. |
| `identity_parse` | Public key, signature, member credential and secret parsing, and `verify_profile`. |
| `mls_process` | A fresh `MlsClient` with a created group receives arbitrary commit, welcome and key-package bytes. |

## CI

`.github/workflows/fuzz.yml`:

- Pull requests that touch `crates/`, `fuzz/`, `Cargo.toml`, `Cargo.lock` or the
  workflow run `cargo fuzz build` only, to catch API changes that break a target.
- A weekly schedule (Mondays) and manual `workflow_dispatch` run every target
  for 60 seconds each (the dispatch form takes a different number of seconds).
- A failing target uploads `fuzz/artifacts/<target>/` as the
  `fuzz-crash-<target>` workflow artifact.

## When a target crashes

1. Download the artifact (or take the file from `fuzz/artifacts/<target>/`).
2. Reproduce: `cargo fuzz run <target> fuzz/artifacts/<target>/crash-<hash>`.
3. Minimise: `cargo fuzz tmin <target> fuzz/artifacts/<target>/crash-<hash>`.
4. Turn the minimised input into a regression `#[test]` in the crate that owns
   the bug, with the bytes inlined, then fix the bug.

## Adding a target

1. Add `fuzz/fuzz_targets/<name>.rs` (`#![no_main]`, `fuzz_target!`) and a
   matching `[[bin]]` entry in `fuzz/Cargo.toml`.
2. Add the name to the matrix in `.github/workflows/fuzz.yml`.
3. Add a few seed inputs under `fuzz/seeds/<name>/`.
4. Prefer structured input (`arbitrary::Arbitrary`) when the code under test
   checks a signature, MAC or checksum first, so the fuzzer can reach the code
   behind the check.

## Not covered

- Swift code (the macOS client). Parsing there goes through the UniFFI
  functions in `core_decode`, but the Swift side has no fuzzing.
- The QUIC transport and TLS stack (quinn, rustls) and the live frame reader.
- Opus itself beyond what `audio_receive` reaches.
