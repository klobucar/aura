//! End-to-end encrypted file transfer: chunk encryption, manifests and the
//! stream header codec. See `docs/protocol/file_transfer.md`.
//!
//! The sender picks a random 32-byte file key and a random 16-byte file id,
//! splits the file into 64 KiB chunks and seals each one with
//! XChaCha20-Poly1305. The nonce is the chunk index (the key is unique to the
//! file, so a counter nonce never repeats), and the AAD binds file id, index,
//! chunk count and an is-last flag, so chunks cannot be reordered, dropped,
//! truncated or moved between files. The key and the file metadata travel in a
//! [`FileManifest`] inside an ordinary end-to-end encrypted text message; the
//! server relays only ciphertext.
//!
//! Unlike text and audio there is no DAVE zero-padding commitment: that
//! mitigates key-guessing oracles against keys shared by a group, whereas a
//! file key is fresh random 256-bit material that only ever opens one file.

use crate::text_crypto::{decrypt_text, encrypt_text, EncryptedTextPacket, TextMessage};
use aura_protocol::file_transfer as wire;
pub use aura_protocol::file_transfer::{
    FileStreamHeader, FILE_CHUNK_SIZE, FILE_HEADER_FIXED_LEN, MAX_FILE_CIPHERTEXT_SIZE,
};
pub use aura_protocol::FileManifest;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use zeroize::Zeroize;

const AAD_LABEL: &[u8] = b"aura-file-chunk-v1";

/// Why a file operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum FileError {
    #[error("manifest is invalid")]
    InvalidManifest,
    #[error("file is larger than the 25 MiB transfer limit")]
    FileTooLarge,
    #[error("plaintext chunk has the wrong length")]
    WrongPlaintextLength,
    #[error("ciphertext chunk has the wrong length")]
    BadChunkLength,
    #[error("chunk failed authentication (tampered, reordered, wrong key or wrong file)")]
    Tampered,
    #[error("more chunks than the manifest declares")]
    TooManyChunks,
    #[error("stream ended before the last chunk")]
    Truncated,
    #[error("decrypted size does not match the manifest")]
    SizeMismatch,
    #[error("SHA-256 of the decrypted file does not match the manifest")]
    HashMismatch,
    #[error("stream header does not match the manifest")]
    HeaderMismatch,
    #[error("stream header is invalid")]
    InvalidHeader,
    #[error("an earlier error ended this transfer")]
    Failed,
    #[error("encryption failed")]
    EncryptionFailed,
    #[error("text message could not be decrypted")]
    TextDecryptionFailed,
    #[error("invalid key size")]
    InvalidKeySize,
}

fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::RngExt;
    rand::rng().random()
}

fn chunk_nonce(index: u32) -> XNonce {
    let mut n = [0u8; 24];
    n[20..].copy_from_slice(&index.to_be_bytes());
    XNonce::from(n)
}

fn chunk_aad(file_id: &[u8], index: u32, count: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_LABEL.len() + 16 + 9);
    aad.extend_from_slice(AAD_LABEL);
    aad.extend_from_slice(file_id);
    aad.extend_from_slice(&index.to_le_bytes());
    aad.extend_from_slice(&count.to_le_bytes());
    aad.push(u8::from(index + 1 == count));
    aad
}

/// Plaintext length of chunk `index` for a file of `size` bytes in `count` chunks.
fn plaintext_chunk_len(size: u64, count: u32, index: u32) -> usize {
    if index + 1 < count {
        FILE_CHUNK_SIZE
    } else {
        (size - (count as u64 - 1) * FILE_CHUNK_SIZE as u64) as usize
    }
}

/// Check a manifest is well formed and within limits.
pub fn validate_manifest(m: &FileManifest) -> Result<(), FileError> {
    if m.file_id.len() != wire::FILE_ID_LEN
        || m.file_key.len() != wire::FILE_KEY_LEN
        || m.sha256.len() != 32
        || m.chunk_size as usize != FILE_CHUNK_SIZE
    {
        return Err(FileError::InvalidManifest);
    }
    if m.size > wire::max_plaintext_size() {
        return Err(FileError::FileTooLarge);
    }
    if m.chunk_count as u64 != wire::chunk_count_for_size(m.size) {
        return Err(FileError::InvalidManifest);
    }
    Ok(())
}

fn cipher_for(m: &FileManifest) -> XChaCha20Poly1305 {
    let key: &[u8; 32] = m
        .file_key
        .as_slice()
        .try_into()
        .expect("manifest validated: 32-byte key");
    XChaCha20Poly1305::new(key.into())
}

/// SHA-256 over a file, fed incrementally. The sender runs this over the file
/// once before building the manifest, because the manifest carries the hash.
#[derive(Default)]
pub struct FileHasher(Sha256);

impl FileHasher {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finalize(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

/// Sender side: encrypts a file one chunk at a time.
pub struct FileEncryptor {
    manifest: FileManifest,
    cipher: XChaCha20Poly1305,
    next: u32,
    hasher: Sha256,
    failed: bool,
}

impl FileEncryptor {
    /// Start a transfer of a file of `size` bytes whose SHA-256 is `sha256`.
    /// Generates a fresh random key and file id.
    pub fn new(
        file_name: &str,
        mime_type: &str,
        size: u64,
        sha256: [u8; 32],
    ) -> Result<Self, FileError> {
        Self::with_key_and_id(
            random_bytes(),
            random_bytes(),
            file_name,
            mime_type,
            size,
            sha256,
        )
    }

    /// As [`new`](Self::new) with caller-chosen key and id. For tests and
    /// known-answer vectors; production code must use random values.
    pub fn with_key_and_id(
        key: [u8; 32],
        file_id: [u8; 16],
        file_name: &str,
        mime_type: &str,
        size: u64,
        sha256: [u8; 32],
    ) -> Result<Self, FileError> {
        let manifest = FileManifest {
            file_id: file_id.to_vec(),
            file_key: key.to_vec(),
            file_name: file_name.to_string(),
            mime_type: mime_type.to_string(),
            size,
            chunk_size: FILE_CHUNK_SIZE as u32,
            chunk_count: wire::chunk_count_for_size(size).min(u32::MAX as u64) as u32,
            sha256: sha256.to_vec(),
        };
        validate_manifest(&manifest)?;
        Ok(Self {
            cipher: cipher_for(&manifest),
            manifest,
            next: 0,
            hasher: Sha256::new(),
            failed: false,
        })
    }

    /// The manifest to put in the text message that announces this file.
    pub fn manifest(&self) -> &FileManifest {
        &self.manifest
    }

    /// The header that opens the upload stream for `channel_id`.
    pub fn stream_header(&self, channel_id: &str) -> Result<FileStreamHeader, FileError> {
        let mut id = [0u8; 16];
        id.copy_from_slice(&self.manifest.file_id);
        FileStreamHeader::for_upload(channel_id, id, self.manifest.size)
            .map_err(|_| FileError::InvalidHeader)
    }

    /// Chunks still to encrypt.
    pub fn remaining_chunks(&self) -> u32 {
        self.manifest.chunk_count - self.next
    }

    /// Plaintext length the next call to `encrypt_next` expects.
    pub fn next_plaintext_len(&self) -> Option<usize> {
        (self.next < self.manifest.chunk_count).then(|| {
            plaintext_chunk_len(self.manifest.size, self.manifest.chunk_count, self.next)
        })
    }

    /// Encrypt the next chunk. `plaintext` must be exactly 64 KiB, except the
    /// last chunk, which carries whatever remains (nothing, for an empty file).
    /// Returns the ciphertext (plaintext length + 16).
    pub fn encrypt_next(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, FileError> {
        if self.failed {
            return Err(FileError::Failed);
        }
        let r = self.encrypt_next_inner(plaintext);
        if r.is_err() {
            self.failed = true;
        }
        r
    }

    fn encrypt_next_inner(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, FileError> {
        let want = self.next_plaintext_len().ok_or(FileError::TooManyChunks)?;
        if plaintext.len() != want {
            return Err(FileError::WrongPlaintextLength);
        }
        let aad = chunk_aad(&self.manifest.file_id, self.next, self.manifest.chunk_count);
        let ct = self
            .cipher
            .encrypt(
                &chunk_nonce(self.next),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| FileError::EncryptionFailed)?;
        self.hasher.update(plaintext);
        self.next += 1;
        Ok(ct)
    }

    /// As [`encrypt_next`](Self::encrypt_next), with the 4-byte little-endian
    /// length prefix the wire format uses, ready to write to the stream.
    pub fn encrypt_next_framed(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, FileError> {
        self.encrypt_next(plaintext).map(|ct| wire::frame_chunk(&ct))
    }

    /// Confirm every chunk was encrypted and the data matched the manifest's
    /// SHA-256 (catching a sender whose file changed between hashing and
    /// sending).
    pub fn finish(&mut self) -> Result<(), FileError> {
        if self.failed {
            return Err(FileError::Failed);
        }
        if self.next != self.manifest.chunk_count {
            return Err(FileError::Truncated);
        }
        let got: [u8; 32] = self.hasher.clone().finalize().into();
        if got.as_slice() != self.manifest.sha256.as_slice() {
            return Err(FileError::HashMismatch);
        }
        Ok(())
    }
}

impl Drop for FileEncryptor {
    fn drop(&mut self) {
        self.manifest.file_key.zeroize();
    }
}

/// Recipient side: decrypts chunks in order and verifies the whole file.
///
/// Plaintext is released chunk by chunk, before the final hash check, so the
/// caller must write it somewhere it can discard (a temporary file) and only
/// present the file once [`finish`](Self::finish) succeeds.
pub struct FileDecryptor {
    manifest: FileManifest,
    cipher: XChaCha20Poly1305,
    next: u32,
    written: u64,
    hasher: Sha256,
    failed: bool,
}

impl FileDecryptor {
    pub fn new(manifest: FileManifest) -> Result<Self, FileError> {
        validate_manifest(&manifest)?;
        Ok(Self {
            cipher: cipher_for(&manifest),
            manifest,
            next: 0,
            written: 0,
            hasher: Sha256::new(),
            failed: false,
        })
    }

    pub fn manifest(&self) -> &FileManifest {
        &self.manifest
    }

    /// Check a delivered stream header describes this manifest's file.
    pub fn check_header(&self, h: &FileStreamHeader) -> Result<(), FileError> {
        h.validate().map_err(|_| FileError::InvalidHeader)?;
        if h.file_id.as_slice() != self.manifest.file_id.as_slice()
            || h.chunk_count != self.manifest.chunk_count
            || h.total_ciphertext_len != wire::ciphertext_len_for_size(self.manifest.size)
        {
            return Err(FileError::HeaderMismatch);
        }
        Ok(())
    }

    /// Chunks decrypted so far.
    pub fn chunks_done(&self) -> u32 {
        self.next
    }

    /// Decrypt the next chunk's ciphertext (without length prefix). The chunk
    /// index is tracked here, so a reordered, replayed or foreign chunk fails.
    pub fn decrypt_next(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, FileError> {
        if self.failed {
            return Err(FileError::Failed);
        }
        let r = self.decrypt_next_inner(ciphertext);
        if r.is_err() {
            self.failed = true;
        }
        r
    }

    fn decrypt_next_inner(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, FileError> {
        let count = self.manifest.chunk_count;
        if self.next >= count {
            return Err(FileError::TooManyChunks);
        }
        let want = plaintext_chunk_len(self.manifest.size, count, self.next) + wire::FILE_TAG_SIZE;
        if ciphertext.len() != want {
            return Err(FileError::BadChunkLength);
        }
        let aad = chunk_aad(&self.manifest.file_id, self.next, count);
        let pt = self
            .cipher
            .decrypt(
                &chunk_nonce(self.next),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| FileError::Tampered)?;
        self.hasher.update(&pt);
        self.written += pt.len() as u64;
        self.next += 1;
        Ok(pt)
    }

    /// Call after the stream ends cleanly. Fails if chunks are missing, the
    /// size is off, or the SHA-256 differs from the manifest.
    pub fn finish(&mut self) -> Result<(), FileError> {
        if self.failed {
            return Err(FileError::Failed);
        }
        let r = self.finish_inner();
        if r.is_err() {
            self.failed = true;
        }
        r
    }

    fn finish_inner(&mut self) -> Result<(), FileError> {
        if self.next != self.manifest.chunk_count {
            return Err(FileError::Truncated);
        }
        if self.written != self.manifest.size {
            return Err(FileError::SizeMismatch);
        }
        let got: [u8; 32] = self.hasher.clone().finalize().into();
        if got.as_slice() != self.manifest.sha256.as_slice() {
            return Err(FileError::HashMismatch);
        }
        Ok(())
    }
}

impl Drop for FileDecryptor {
    fn drop(&mut self) {
        self.manifest.file_key.zeroize();
    }
}

// ---------------------------------------------------------------------------
// Carrying the manifest in an encrypted text message
// ---------------------------------------------------------------------------

/// Encrypt a text message that announces a file. Sets the message type to
/// FILE and attaches `manifest`; the key and metadata are hidden from the
/// server like the rest of the message.
pub fn encrypt_file_announcement(
    dave_key: &[u8; 32],
    epoch: u64,
    channel_id: String,
    sender_session_id: u32,
    mut message: TextMessage,
    manifest: FileManifest,
) -> Result<EncryptedTextPacket, FileError> {
    validate_manifest(&manifest)?;
    message.r#type = aura_protocol::MediaType::File as i32;
    message.attachment = Some(manifest);
    encrypt_text(dave_key, epoch, channel_id, sender_session_id, &message)
        .map_err(|_| FileError::EncryptionFailed)
}

/// Decrypt a text packet; returns the message and, if it announces a file, the
/// validated manifest. Name and MIME type in it are untrusted.
pub fn decrypt_file_announcement(
    dave_key: &[u8; 32],
    packet: &EncryptedTextPacket,
) -> Result<(TextMessage, Option<FileManifest>), FileError> {
    let mut message =
        decrypt_text(dave_key, packet).map_err(|_| FileError::TextDecryptionFailed)?;
    let manifest = message.attachment.take();
    if let Some(m) = &manifest {
        validate_manifest(m)?;
    }
    Ok((message, manifest))
}

// ---------------------------------------------------------------------------
// UniFFI surface
// ---------------------------------------------------------------------------

/// UniFFI form of [`FileManifest`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct FileManifestRecord {
    pub file_id: Vec<u8>,
    pub file_key: Vec<u8>,
    /// Untrusted: sender-controlled.
    pub file_name: String,
    /// Untrusted: sender-controlled.
    pub mime_type: String,
    pub size: u64,
    pub chunk_size: u32,
    pub chunk_count: u32,
    pub sha256: Vec<u8>,
}

impl From<FileManifest> for FileManifestRecord {
    fn from(m: FileManifest) -> Self {
        Self {
            file_id: m.file_id,
            file_key: m.file_key,
            file_name: m.file_name,
            mime_type: m.mime_type,
            size: m.size,
            chunk_size: m.chunk_size,
            chunk_count: m.chunk_count,
            sha256: m.sha256,
        }
    }
}

impl From<FileManifestRecord> for FileManifest {
    fn from(r: FileManifestRecord) -> Self {
        Self {
            file_id: r.file_id,
            file_key: r.file_key,
            file_name: r.file_name,
            mime_type: r.mime_type,
            size: r.size,
            chunk_size: r.chunk_size,
            chunk_count: r.chunk_count,
            sha256: r.sha256,
        }
    }
}

/// UniFFI form of [`FileStreamHeader`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct FileStreamHeaderRecord {
    pub sender_session_id: u32,
    pub channel_id: String,
    pub file_id: Vec<u8>,
    pub total_ciphertext_len: u64,
    pub chunk_count: u32,
}

/// A decrypted text message plus the file manifest it carried, if any.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DecryptedFileAnnouncement {
    pub message: crate::uniffi_bindings::TextMessageRecord,
    pub manifest: Option<FileManifestRecord>,
}

fn key32(key: &[u8]) -> Result<[u8; 32], FileError> {
    key.try_into().map_err(|_| FileError::InvalidKeySize)
}

fn sha32(sha: &[u8]) -> Result<[u8; 32], FileError> {
    sha.try_into().map_err(|_| FileError::InvalidManifest)
}

/// Incremental SHA-256 for the sender's pre-pass over the file.
#[derive(uniffi::Object)]
pub struct FileHasherWrapper {
    inner: Mutex<Option<FileHasher>>,
}

#[uniffi::export]
impl FileHasherWrapper {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Some(FileHasher::new())),
        }
    }

    pub fn update(&self, data: Vec<u8>) {
        if let Some(h) = self.inner.lock().unwrap().as_mut() {
            h.update(&data);
        }
    }

    /// The 32-byte digest. Later calls return an empty vector.
    pub fn finalize(&self) -> Vec<u8> {
        match self.inner.lock().unwrap().take() {
            Some(h) => h.finalize().to_vec(),
            None => Vec::new(),
        }
    }
}

impl Default for FileHasherWrapper {
    fn default() -> Self {
        Self::new()
    }
}

/// Sender-side chunk encryptor for UniFFI.
#[derive(uniffi::Object)]
pub struct FileEncryptorWrapper {
    inner: Mutex<FileEncryptor>,
}

#[uniffi::export]
impl FileEncryptorWrapper {
    /// `sha256` is the digest of the whole file (see `FileHasherWrapper`).
    #[uniffi::constructor]
    pub fn new(
        file_name: String,
        mime_type: String,
        size: u64,
        sha256: Vec<u8>,
    ) -> Result<Self, FileError> {
        Ok(Self {
            inner: Mutex::new(FileEncryptor::new(
                &file_name,
                &mime_type,
                size,
                sha32(&sha256)?,
            )?),
        })
    }

    pub fn manifest(&self) -> FileManifestRecord {
        self.inner.lock().unwrap().manifest().clone().into()
    }

    /// Header bytes to write first on the upload stream.
    pub fn stream_header(&self, channel_id: String) -> Result<Vec<u8>, FileError> {
        Ok(self.inner.lock().unwrap().stream_header(&channel_id)?.encode())
    }

    /// Plaintext length the next chunk must have; `None` once all are done.
    pub fn next_plaintext_len(&self) -> Option<u32> {
        self.inner.lock().unwrap().next_plaintext_len().map(|n| n as u32)
    }

    /// Encrypt the next chunk and return it length-prefixed, ready to write
    /// to the upload stream.
    pub fn encrypt_next_framed(&self, plaintext: Vec<u8>) -> Result<Vec<u8>, FileError> {
        self.inner.lock().unwrap().encrypt_next_framed(&plaintext)
    }

    /// Call after the last chunk.
    pub fn finish(&self) -> Result<(), FileError> {
        self.inner.lock().unwrap().finish()
    }
}

/// Recipient-side chunk decryptor for UniFFI.
#[derive(uniffi::Object)]
pub struct FileDecryptorWrapper {
    inner: Mutex<FileDecryptor>,
}

#[uniffi::export]
impl FileDecryptorWrapper {
    #[uniffi::constructor]
    pub fn new(manifest: FileManifestRecord) -> Result<Self, FileError> {
        Ok(Self {
            inner: Mutex::new(FileDecryptor::new(manifest.into())?),
        })
    }

    /// Check the header of a delivered stream against the manifest.
    pub fn check_header(&self, header: Vec<u8>) -> Result<(), FileError> {
        let h = FileStreamHeader::decode(&header).map_err(|_| FileError::InvalidHeader)?;
        self.inner.lock().unwrap().check_header(&h)
    }

    /// Decrypt one chunk (ciphertext only, without its length prefix), in
    /// stream order.
    pub fn decrypt_next(&self, ciphertext: Vec<u8>) -> Result<Vec<u8>, FileError> {
        self.inner.lock().unwrap().decrypt_next(&ciphertext)
    }

    pub fn chunks_done(&self) -> u32 {
        self.inner.lock().unwrap().chunks_done()
    }

    /// Call when the stream ends cleanly. Only after this succeeds may the
    /// decrypted file be shown.
    pub fn finish(&self) -> Result<(), FileError> {
        self.inner.lock().unwrap().finish()
    }
}

/// Encode a stream header.
#[uniffi::export]
pub fn encode_file_stream_header(header: FileStreamHeaderRecord) -> Result<Vec<u8>, FileError> {
    let h = FileStreamHeader {
        sender_session_id: header.sender_session_id,
        channel_id: header.channel_id,
        file_id: header
            .file_id
            .as_slice()
            .try_into()
            .map_err(|_| FileError::InvalidHeader)?,
        total_ciphertext_len: header.total_ciphertext_len,
        chunk_count: header.chunk_count,
    };
    h.validate().map_err(|_| FileError::InvalidHeader)?;
    Ok(h.encode())
}

/// Decode and validate a complete stream header.
#[uniffi::export]
pub fn decode_file_stream_header(data: Vec<u8>) -> Result<FileStreamHeaderRecord, FileError> {
    let h = FileStreamHeader::decode(&data).map_err(|_| FileError::InvalidHeader)?;
    Ok(FileStreamHeaderRecord {
        sender_session_id: h.sender_session_id,
        channel_id: h.channel_id,
        file_id: h.file_id.to_vec(),
        total_ciphertext_len: h.total_ciphertext_len,
        chunk_count: h.chunk_count,
    })
}

/// Number of bytes at the start of a stream that hold the fixed part of the
/// header. Read this many, pass them to `file_stream_header_channel_id_len`,
/// then read that many more and call `decode_file_stream_header` on the whole.
#[uniffi::export]
pub fn file_stream_header_fixed_len() -> u32 {
    FILE_HEADER_FIXED_LEN as u32
}

/// Channel-id length announced by the fixed part of a header.
#[uniffi::export]
pub fn file_stream_header_channel_id_len(prefix: Vec<u8>) -> Result<u32, FileError> {
    FileStreamHeader::channel_id_len_from_prefix(&prefix)
        .map(|n| n as u32)
        .map_err(|_| FileError::InvalidHeader)
}

/// Plaintext bytes per chunk (65536).
#[uniffi::export]
pub fn file_chunk_size() -> u32 {
    FILE_CHUNK_SIZE as u32
}

/// Largest file, in plaintext bytes, that fits the 25 MiB ciphertext cap.
#[uniffi::export]
pub fn max_file_plaintext_size() -> u64 {
    wire::max_plaintext_size()
}

/// Encrypt a text message announcing a file; see [`encrypt_file_announcement`].
#[uniffi::export]
pub fn encrypt_text_with_attachment(
    dave_key: Vec<u8>,
    epoch: u64,
    channel_id: String,
    sender_session_id: u32,
    message: crate::uniffi_bindings::TextMessageRecord,
    manifest: FileManifestRecord,
) -> Result<crate::uniffi_bindings::EncryptedTextPacketRecord, FileError> {
    encrypt_file_announcement(
        &key32(&dave_key)?,
        epoch,
        channel_id,
        sender_session_id,
        message.into(),
        manifest.into(),
    )
    .map(Into::into)
}

/// Decrypt a text packet, returning any file manifest it carried.
#[uniffi::export]
pub fn decrypt_text_with_attachment(
    dave_key: Vec<u8>,
    packet: crate::uniffi_bindings::EncryptedTextPacketRecord,
) -> Result<DecryptedFileAnnouncement, FileError> {
    let (message, manifest) = decrypt_file_announcement(&key32(&dave_key)?, &packet.into())?;
    Ok(DecryptedFileAnnouncement {
        message: message.into(),
        manifest: manifest.map(Into::into),
    })
}
