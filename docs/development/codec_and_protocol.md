---
title: Codec & Protocol Details
description: Deep dive into the Aura binary protocol and audio pipeline.
---
# Codec & Protocol Implementation

This document provides a deep dive into the Aura binary protocol and the implementation details of the audio pipeline.

## 1. FastAudioPacket (Wire Format)

Audio is transmitted via QUIC datagrams using a custom strictly-typed binary header. This is designed for zero-allocation parsing on the server relay.

A datagram is a one-byte type followed by the packet. The type is the audio format version:

| Type | Format |
|------|--------|
| `0x02` | Version 2: the header is authenticated (below). |
| `0x01` | Version 1: header not authenticated. No longer accepted; see [Versions](#versions). |

The packet (all integers big-endian):

| Offset | Field | Type | Description |
|--------|-------|------|-------------|
| 0 | `session_id` | `u32` | Unique ID for the sender in this server session. |
| 4 | `epoch_hint` | `u16` | Low 16 bits of the current MLS Voice Epoch. |
| 6 | `sequence` | `u16` | Per-sender wrapped sequence number. |
| 8 | `nonce` | `[u8; 24]` | XChaCha20-Poly1305 nonce, random per packet. |
| 32 | `payload` | `Vec<u8>` | Encrypted Ciphertext + Auth Tag. |

### Header authentication

The first eight bytes (`session_id`, `epoch_hint`, `sequence`) are the AEAD's associated data. They stay readable, as the relay needs `session_id`, but a packet whose header was changed in transit fails to decrypt. The associated data is:

```
"aura-audio-v2" || 0x02 || session_id (u32 BE) || epoch_hint (u16 BE) || sequence (u16 BE)
```

That is 21 bytes: the 13-byte label, the format version, and the header fields exactly as they appear on the wire. Without it the server, which is untrusted, could replay any genuine frame of the current epoch under a fresh sequence number, and the receiver would play it again; or it could re-stamp a frame far ahead to move the receiver's replay window. The per-sender keys already stop it forging audio or changing who a frame is attributed to.

The receiver derives the associated data from the header it received, so the epoch fallback (trying older keys) still works: each candidate key is tried with the same associated data.

### Versions

Audio sent on the reliable stream (the fallback when datagrams fail, and the only path of the .NET client) carries the same packet in a `[type][u32 LE length][packet]` message: `0x21` for version 2, `0x20` for version 1.

The format version is carried by the type byte, not the packet, so the server's sender check (the first four bytes of the packet must be the connection's own `session_id`) is the same for both versions. The server relays version 1 under its own tag but nobody decrypts it: a receiver that gets one drops it and logs once per sender, "peer uses an older audio format; update both clients". This is a wire-incompatible change; clients and servers must be upgraded together, and an old client hears nothing from a new one (it ignores the new tags).

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
4. **Encrypt**: DAVE (XChaCha20 + Padding, header as associated data).
5. **Transmit**: Encapsulate in `FastAudioPacket` and send as QUIC datagram.

### Playback (Receiver)
1. **Parser**: Validate header and `session_id`.
2. **Jitter Buffer**: Handle out-of-order and late packets.
3. **Decrypt**: DAVE validation, including the header.
4. **Opus Decode**: Handle Packet Loss Concealment (PLC).
5. **Spatial Mix**: Apply Raycasting/HRTF based on the sender's 3D position.
6. **Playback**: 16kHz or 48kHz PCM output.
