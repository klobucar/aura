# Encrypted File Transfer

Files are end-to-end encrypted on the sender's device, relayed as opaque chunks by the server to the people **currently in the channel**, and decrypted on each recipient's device. The server stores nothing.

This document is the protocol reference. Code: wire format in `crates/aura-protocol/src/file_transfer.rs`, client crypto in `crates/aura-core/src/file_transfer.rs`, server relay in `crates/aura-server/src/file_relay.rs`.

## Overview

```
Sender                           Server                         Recipients (in channel now)
  | 1. text msg (0x30) with        |                                  |
  |    encrypted FileManifest ---->|--------- relayed as any text --->|  key, name, size, hash
  | 2. open bidi stream, header    |                                  |
  |    + encrypted chunks -------->|-- open uni stream, header ----->|
  |                                |-- chunks as they arrive ------->|  decrypt chunk by chunk
  | 3. <------ status (8 bytes)    |                                  |  verify SHA-256, then show
```

- The **manifest** (file key, id, name, MIME type, size, hash) travels in an ordinary end-to-end encrypted text message (`MSG_TEXT_PACKET` 0x30). The server never sees the key or the metadata.
- The **bytes** travel on a dedicated QUIC stream per transfer, not the control stream, so a large file cannot head-of-line block MLS commits or other control traffic (control frames are capped at 256 KiB anyway).
- Each file has its own random 256-bit key. There is no key derived from the MLS epoch, so a later epoch change does not affect a transfer in flight.

## 1. Cryptography

| Item | Value |
|---|---|
| Cipher | XChaCha20-Poly1305 (the same `chacha20poly1305` crate as text and audio) |
| Key | 32 random bytes per file, in the manifest |
| `file_id` | 16 random bytes per file, in the manifest and the stream header |
| Chunk size | 65536 plaintext bytes; the last chunk holds the remainder |
| Chunk count | `max(1, ceil(size / 65536))`. An empty file is one chunk holding only a tag. An exact multiple has no empty trailing chunk |
| Nonce (24 B) | 20 zero bytes, then the chunk index as `u32` big-endian. Unique per chunk because the key is unique to the file |
| AAD | `"aura-file-chunk-v1"` (18 ASCII bytes) `‖ file_id (16) ‖ index (u32 LE) ‖ chunk_count (u32 LE) ‖ is_last (1 byte, 0 or 1)` |
| Ciphertext per chunk | plaintext length + 16 (Poly1305 tag) |

Binding `file_id`, index, count and the last flag into the AAD means chunks cannot be reordered, dropped, truncated, duplicated or spliced between files without failing authentication. The recipient also enforces the order itself (it tracks the expected index), checks every ciphertext length against the manifest, checks the total decrypted size, and finally compares SHA-256 of the whole plaintext with the manifest.

The DAVE zero-padding commitment used for text and audio is **not** applied. It mitigates key-guessing oracles against keys shared by many parties; a file key is fresh uniform 256-bit material that opens exactly one file and reaches recipients only inside the E2EE manifest.

A known-answer test (`tests_file_transfer.rs`, `known_answer_vectors`) pins the nonce and AAD construction. Its expected values were produced by an independent Python implementation (HChaCha20 plus ChaCha20-Poly1305), not by the Rust code. Changing either is an interoperability break and needs a new AAD label / header version.

## 2. Manifest

`FileManifest` (`aura.proto`) is carried in the new optional field `TextMessage.attachment` (field 9) of the encrypted `TextMessage`, with `type = FILE`. `content` is a free caption. This is the least invasive option: it reuses the existing encrypt/decrypt, replay protection and routing, and older clients that do not know field 9 skip it (they show an empty message). The existing `file_size` and `sha256_hash` fields (7, 8) are not used for transfers.

| Field | Meaning |
|---|---|
| `file_id` (16 B) | Names the transfer; matches the stream header |
| `file_key` (32 B) | XChaCha20-Poly1305 key |
| `file_name` | **Untrusted**, sender-chosen |
| `mime_type` | **Untrusted**, sender-chosen |
| `size` | Plaintext bytes; at most `max_file_plaintext_size()` (25 MiB of ciphertext, see below) |
| `chunk_size` | 65536 in version 1; anything else is rejected |
| `chunk_count` | Must equal `max(1, ceil(size / chunk_size))` |
| `sha256` (32 B) | SHA-256 of the whole plaintext |

The sender must hash the file **before** sending the manifest (a first pass over the file; `FileHasherWrapper`). The encryptor hashes again as it encrypts and fails `finish()` with `HashMismatch` if the file changed in between.

**Sender authentication is exactly what text provides today**: the manifest is inside a text message encrypted under the group's epoch key, so it proves the sender is a current group member, but `TextMessage.sender_uuid` is not individually signed (see Open issues).

**The server does no sanitising of anything**, because it never sees names or types. Clients must treat `file_name` and `mime_type` as hostile: strip path separators and control and bidirectional-override characters, cap length, never trust the MIME type or extension to decide how to open the file, and never let the name choose a path outside the download folder.

## 3. Transport

### 3.1 Upload (client to server)

The sender opens a **client-initiated bidirectional QUIC stream** (the control stream is server-initiated, so this cannot be confused with it) and writes:

```
header
chunk 0 frame
chunk 1 frame
...
```

then finishes its send side. The server answers on the same stream with an 8-byte status (section 3.4).

### 3.2 Header (both directions)

All integers little-endian. Total length `36 + channel_id_len`.

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | Stream kind, `0x01` (file transfer) |
| 1 | 1 | Version, `1` |
| 2 | 4 | `sender_session_id` (u32). Clients send `0`; the server overwrites it with the real session id on delivery |
| 6 | 16 | `file_id` |
| 22 | 8 | `total_ciphertext_len` (u64): sum of all chunk ciphertext lengths, tags included, length prefixes excluded |
| 30 | 4 | `chunk_count` (u32), at least 1 |
| 34 | 2 | `channel_id_len` (u16), at most 128 |
| 36 | n | `channel_id`, UTF-8 |

Header validation (shared code, `FileStreamHeader::validate`): kind and version known, `chunk_count >= 1`, `total_ciphertext_len <= 25 MiB`, and the total consistent with the count: every chunk but the last is exactly 65552 bytes (65536 + tag), the last is 16 to 65552 bytes (exactly 16 only when `chunk_count == 1`, the empty file; a multi-chunk file's last chunk holds at least one plaintext byte).

### 3.3 Chunk frame

```
[ciphertext_len: u32 LE] [ciphertext: ciphertext_len bytes]
```

`ciphertext_len` must equal exactly what the header implies for that index (65552 for all but the last chunk). The server enforces this, so it never buffers more than one chunk of anything the sender declares, and there is no way to smuggle extra data inside a chunk.

### 3.4 Upload status (server to sender)

When the transfer ends the server writes 8 bytes and finishes its side: `[code: u32 LE][recipients: u32 LE]`.

- `code == 0`: the server received exactly the declared bytes, saw the sender finish the stream, and relayed it. `recipients` is the number of members whose copy was still live when the last chunk was queued; recipients can still fail afterwards, and nothing here is a delivery receipt.
- `code != 0`: refused or aborted; see the codes below. `recipients` is 0. The server also issues `STOP_SENDING` with the same code so the sender stops writing promptly.

The status arrives after the server has handed the last bytes to every live recipient connection (not merely when it read the last byte from the sender).

### 3.5 Delivery (server to recipient)

For each other member of the channel's text group at the moment the header is validated, the server opens a **server-initiated unidirectional QUIC stream**, writes the header (with `sender_session_id` filled in) and forwards the chunk frames byte-for-byte as they arrive, then finishes the stream. If the transfer fails for any reason after the stream opened, the server resets it (`RESET_STREAM`) with an error code; **a reset always means "discard everything received for this file"**.

A recipient can decline a transfer, or abandon one (for example when the manifest never arrives or the sender is not trusted), by calling `STOP_SENDING` on the stream; the server then drops only that copy.

The text message carrying the manifest and the delivery stream are separate QUIC streams, so **either can arrive first**. A client must match them by `file_id`, hold or buffer a stream whose manifest has not arrived (bounded; reset it after a short wait), and never assume order.

### 3.6 Error codes

Used as QUIC application error codes, and as the status `code` for uploads.

| Code | Name | Meaning |
|---|---|---|
| 1 | `MALFORMED_HEADER` | Header unreadable, wrong kind or version, inconsistent fields, or not delivered in 10 s |
| 2 | `TOO_LARGE` | Declared ciphertext over 25 MiB |
| 3 | `NOT_IN_CHANNEL` | Sender not in the named channel's group (also sent to a recipient who left mid-transfer) |
| 4 | `RATE_LIMITED` | Per-user concurrency or bytes-per-minute limit |
| 5 | `PROTOCOL_VIOLATION` | Chunk length disagrees with the header, or more bytes than declared, or the stream did not finish after the last chunk |
| 6 | `SENDER_LEFT` | Sender left the channel, disconnected or reset the stream mid-transfer |
| 7 | `RECIPIENT_TOO_SLOW` | This recipient's copy was dropped for falling behind (also: a write to it stalled for 30 s, or it refused the stream) |
| 8 | `SENDER_STALLED` | No complete chunk within 30 s, or the transfer exceeded 15 minutes |
| 9 | `NO_RECIPIENTS` | Nobody else in the channel, or every recipient was dropped or left |
| 10 | `SERVER_UNAVAILABLE` | Reserved |

## 4. Server rules

- **No storage.** File bytes exist only in memory in flight; nothing is written to disk or the database. Logs and metrics carry no names, types or content (the server has none to log); a transfer is identified in logs by a local counter only.
- **Bounded streaming.** The server reads one chunk (at most 64 KiB) from the sender at a time and offers it to each recipient's queue as a shared `Bytes` handle (no per-recipient copy). Each queue holds `RECIPIENT_BUFFER_CHUNKS = 64` chunks (4 MiB). When a recipient's queue is full that recipient is cut off (`RECIPIENT_TOO_SLOW`); the sender's reads and the other recipients are unaffected. The server never awaits a slow recipient while reading from the sender. Consequence: server memory per transfer is about 4 MiB plus QUIC buffers regardless of recipient count, but a recipient whose downlink is much slower than the sender's uplink will lose the file once the 4 MiB queue plus QUIC's flow-control window are exhausted.
- **Size cap: 25 MiB of ciphertext** (`MAX_FILE_CIPHERTEXT_SIZE`; tags counted, length prefixes not), which makes the largest file 26,208,000 bytes of plaintext (`max_file_plaintext_size()`; the tags take the difference). Declared sizes above the cap are refused up front (`TOO_LARGE`). Extra bytes beyond the declared total abort the transfer (`PROTOCOL_VIOLATION`), detected before any recipient is told the file is complete.
- **Membership.** The sender must be in the channel's text group when the header is validated, and again before every chunk. Recipients are the other members at that moment; someone who joins later never receives the file, and a recipient who leaves mid-transfer is cut off. Files are never queued for people who are offline or in another channel.
- **Sender leaves or disconnects**: the relay aborts immediately (leaving a channel triggers it directly; disconnecting resets the stream; kicks and removals are caught by the per-chunk membership check). Recipients see a reset with `SENDER_LEFT`.
- **Rate limits (per account, not per connection)**:
  - at most **3 concurrent transfers** per user (a slot is held until the recipients have drained their queues);
  - at most **100 MiB of declared ciphertext per minute** per user (token bucket, burst equal to the budget), charged when the transfer starts, at least 64 KiB per transfer so floods of tiny files are limited too.
  - Client-opened bidirectional streams per connection are capped at 16 by the QUIC transport settings.
- **Timeouts**: header 10 s; no complete chunk for 30 s; a recipient write blocked for 30 s; opening a recipient stream 5 s; 15 minutes per transfer.
- **Metrics** (`aura_` prefix; aggregate counts only): `file_transfers_started_total`, `file_transfers_rejected_total{reason}`, `file_transfers_finished_total{outcome}` (`completed`, `sender_left`, `sender_stalled`, `protocol_violation`, `recipients_gone`, `other`), `file_bytes_received_total`, `file_bytes_relayed_total`, `file_recipients_dropped_total`, and the gauge `file_transfers_active`.

## 5. Core API (`aura-core`, exposed through UniFFI)

| Item | Purpose |
|---|---|
| `FileHasherWrapper` (`update`, `finalize`) | Sender's pre-pass: SHA-256 of the file |
| `FileEncryptorWrapper(file_name, mime_type, size, sha256)` | Generates key and `file_id`. `manifest()`, `stream_header(channel_id)`, `next_plaintext_len()`, `encrypt_next_framed(plaintext)` (returns the length-prefixed chunk ready to write), `finish()` |
| `FileDecryptorWrapper(manifest)` | `check_header(header_bytes)`, `decrypt_next(ciphertext)` (ciphertext without prefix, in order), `chunks_done()`, `finish()` |
| `encrypt_text_with_attachment(dave_key, epoch, channel_id, sender_session_id, message, manifest)` | Encrypts the announcing text message (sets `type = FILE`) |
| `decrypt_text_with_attachment(dave_key, packet)` | Returns the message plus the validated manifest, if any |
| `encode_file_stream_header`, `decode_file_stream_header`, `file_stream_header_fixed_len` (36), `file_stream_header_channel_id_len(prefix)` | Header codec; read 36 bytes, ask how many channel-id bytes follow, read them, decode |
| `file_chunk_size()`, `max_file_plaintext_size()` | Constants |
| `FileError` | `InvalidManifest`, `FileTooLarge`, `WrongPlaintextLength`, `BadChunkLength`, `Tampered`, `TooManyChunks`, `Truncated`, `SizeMismatch`, `HashMismatch`, `HeaderMismatch`, `InvalidHeader`, `Failed`, `EncryptionFailed`, `TextDecryptionFailed`, `InvalidKeySize` |

Encryptor and decryptor are **chunk-at-a-time**: neither holds the file. After any error they refuse further calls (`Failed`). The decryptor releases each chunk's plaintext after it authenticates, before the whole-file hash can be checked, so clients **must write plaintext to a temporary file and show or move it only after `finish()` succeeds**, deleting it on any error or reset.

The existing `TextCryptoWrapper.decrypt` still works on announcement messages but drops the attachment; use `decrypt_text_with_attachment` for receiving.

## 6. What each client must implement

### Sending

1. Let the user pick a file or drop it on the channel view. Refuse anything larger than `max_file_plaintext_size()` before doing work. Require that the user is in the channel.
2. Hash the file in a streaming pass (`FileHasherWrapper`), then create a `FileEncryptorWrapper(name, mime, size, sha256)`.
3. Send the announcement: build a `TextMessageRecord` (caption in `content`), call `encrypt_text_with_attachment` with the current epoch key and the encryptor's `manifest()`, send it as `MSG_TEXT_PACKET`. Do this **first**, so recipients usually know the key before bytes arrive.
4. Open a bidirectional QUIC stream (not the control stream), write `stream_header(channel_id)`, then loop: read `next_plaintext_len()` bytes from the file, `encrypt_next_framed`, write. Finish the send side after the last chunk, call `finish()`, then read the 8-byte status.
5. UI: show *encrypting / sending* progress by chunks written, then *sent* on status 0 (optionally "to N people"), or the failure by code (too large, rate limited, you left the channel, ...). Let the user cancel (reset the stream). Never retry automatically on `RATE_LIMITED`.
6. Keep the file key only in memory for the transfer; do not persist it beyond the announcing message the user's own client stores.

### Receiving

1. On a text message with a manifest (`decrypt_text_with_attachment`): validate (done by core), then sanitise `file_name` and treat `mime_type` as untrusted. Show a file card.
2. **Hold back files from unverified senders**: if the sender's identity is not verified (safety words), do not auto-accept or auto-preview. Show a placeholder ("File from an unverified user: Accept?") and, until accepted, do not read the delivery stream (call `STOP_SENDING` to decline, or hold it unread within flow-control limits for a short time). Pick one policy per client; never auto-open.
3. Accept server-initiated unidirectional streams on the connection (make sure the QUIC stack allows at least 16 incoming unidirectional streams). Read 36 bytes, then `file_stream_header_channel_id_len`, then the channel id, then decode. Match `file_id` to a known manifest; if none arrives within a short window, `STOP_SENDING` the stream.
4. Create a `FileDecryptorWrapper(manifest)` and `check_header`. Loop: read `u32 LE` length, read that many bytes, `decrypt_next`, append plaintext to a **temporary file**.
5. At end of stream call `finish()`. Only on success move the file to its final place and mark it *decrypted locally*; on any error, or a stream reset (any code), delete the partial file and show a failed state.
6. UI states: *drop overlay* (sending side, when a file is dragged over the channel), *encrypting / progress* (both sides), *decrypted locally* (verified, safe to open, still untrusted content), *held back, sender unverified*, *failed / cancelled* (with the reason; a reset with `RECIPIENT_TOO_SLOW` is "your connection was too slow, ask them to resend").
7. Opening a received file is the user's decision; never execute it, and never trust the MIME type to choose a handler without confirmation.

## 7. Threat model notes

What the **server learns**: that a transfer happened, who sent it (session and account), to which channel, when, how long it took, and the ciphertext size (so the file size to within 16 bytes per 64 KiB) and chunk timing. It learns who the recipients are because it opens the streams. It does not learn the name, type, content, hash or key.

- **Confidentiality** rests on the E2EE text path: anyone who can read the announcing message (current group members at that epoch) can read the file. A passive observer of the network sees QUIC.
- **No forward secrecy beyond the per-file key's lifetime**: each file has its own key, so compromising one key exposes one file, but the key travels inside a text message protected by the MLS epoch key (which ratchets). Anyone who later compromises a client holding the manifest, or the stored message history, gets the key. Clients should drop the key from memory once the transfer ends and should not persist manifests beyond what chat history needs.
- **Integrity**: a malicious server or network can drop or truncate a transfer (detected: reset or `Truncated`), reorder or splice chunks (detected: AEAD), or substitute another file's chunks (detected: AAD with `file_id`, and the hash). It cannot make a recipient accept altered content.
- **Availability**: the server can refuse to relay, delay, or cut a transfer. Per-user limits stop one user exhausting the relay; a malicious member can still send up to 100 MiB per minute per account into a channel, and a channel's members each carry that bandwidth.
- **Untrusted metadata**: file names and MIME types come from the sender (possibly a malicious member). Clients sanitise and never trust them (section 2).
- **Late joiners and absentees never receive a file.** Only members present when the transfer starts get it; there is no store-and-forward and no re-send from the server. Someone who was offline must ask the sender to resend.
- **Unverified senders**: because sender authentication is only as strong as text, clients hold files from unverified identities (section 6).
- **Resource use on recipients**: a recipient reads at most the size the manifest declares; chunk lengths are fixed by the header, so a hostile sender cannot make the client allocate more than one chunk at a time. Clients must still bound how many partial transfers they keep and delete temporary files on abort.

## 8. Open issues

- `TextMessage.sender_uuid` inside the encrypted payload is not signed per message, and all members share the epoch key, so a malicious member could announce a file under another member's name. File integrity is unaffected (hash and AEAD), but attribution is only as good as the existing text path.
- Per-recipient queue sizing is a trade-off: 64 chunks suits typical links; senders on very fast uplinks can outrun slow recipients, who get `RECIPIENT_TOO_SLOW` and must ask for a resend. There is no server-side retry by design.
- There is no global (cross-user) cap on concurrent transfers; the bound is per user. Operators with many accounts should rely on QUIC connection limits and metrics.
- Chunk size is fixed at 64 KiB in version 1; changing it needs a header and AAD version bump.
