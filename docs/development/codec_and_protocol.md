---
title: Codec & Protocol Details
description: Deep dive into the Aura binary protocol and audio pipeline.
agent_intent: technical_reference
---
# Codec & Protocol Implementation

This document provides a deep dive into the Aura binary protocol and the implementation details of the audio pipeline.

## 1. FastAudioPacket (Wire Format)

Audio is transmitted via QUIC datagrams using a custom strictly-typed binary header. This is designed for zero-allocation parsing on the server relay.

| Offset | Field | Type | Description |
|--------|-------|------|-------------|
| 0 | `session_id` | `u32` (LE) | Unique ID for the sender in this server session. |
| 4 | `epoch_hint` | `u16` (LE) | Low 16 bits of the current MLS Voice Epoch. |
| 6 | `sequence` | `u16` (LE) | Per-sender wrapped sequence number. |
| 8 | `nonce` | `[u8; 24]` | XChaCha20-Poly1305 nonce. |
| 32 | `payload` | `Vec<u8>` | Encrypted Ciphertext + Auth Tag. |

### Epoch Hinting
MLS is synchronous—everyone needs the same epoch to decrypt. In high-latency or packet-loss scenarios, a client might advance their epoch before others receive the update.
- **Solution**: The `epoch_hint` allows the receiver to identify which key to use from their local key cache (e.g., "Current Epoch" vs "Previous Epoch").

## 2. DAVE Protocol (E2EE)

Aura implements the DAVE (Dynamic Audio Voice Encryption) architecture with specific security hardening.

### Cipher: XChaCha20-Poly1305
We use **XChaCha20** instead of standard AES-GCM for:
1. **Extended Nonce (192-bit)**: Safe to generate randomly even with extremely high packet rates.
2. **Software Performance**: Faster on mobile/older desktop CPUs without dedicated AES instructions.

### The Zero-Padding Commitment
To mitigate **Partitioning Oracle Attacks** (where a single ciphertext could decrypt to two valid plaintexts under different keys):
1. **Sender**: Prepend 16 bytes of `0x00` to the Opus frame before encryption.
2. **Receiver**: After decryption, verify the first 16 bytes are `0x00`. If they are not, the packet **MUST** be dropped.

## 3. Audio Pipeline

The pipeline follows this strict flow:

### Capture (Sender)
1. **PCM Capture**: Native hardware (Swift/C#).
2. **NS/AGC**: Noise Suppression and Automatic Gain Control.
3. **Opus Encode**: 20ms frames, bitrates from 16kbps to 128kbps.
4. **Encrypt**: DAVE (XChaCha20 + Padding).
5. **Transmit**: Encapsulate in `FastAudioPacket` and send as QUIC datagram.

### Playback (Receiver)
1. **Parser**: Validate header and `session_id`.
2. **Jitter Buffer**: Handle out-of-order and late packets.
3. **Decrypt**: DAVE validation.
4. **Opus Decode**: Handle Packet Loss Concealment (PLC).
5. **Spatial Mix**: Apply Raycasting/HRTF based on the sender's 3D position.
6. **Playback**: 16kHz or 48kHz PCM output.
