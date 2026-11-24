//! Wire format for end-to-end encrypted file transfers.
//!
//! Shared by the server relay (which validates and forwards) and the client
//! core (which builds and parses). See `docs/protocol/file_transfer.md` for the
//! full design. Nothing here touches key material: the server only ever sees
//! the stream header and opaque, length-prefixed ciphertext chunks.

/// First byte of every file-transfer stream (client -> server upload and
/// server -> client delivery).
pub const FILE_STREAM_KIND: u8 = 0x01;
/// Header/format version carried in the second byte.
pub const FILE_STREAM_VERSION: u8 = 1;

/// Plaintext bytes per chunk (the last chunk may be shorter).
pub const FILE_CHUNK_SIZE: usize = 64 * 1024;
/// Poly1305 tag appended to every chunk.
pub const FILE_TAG_SIZE: usize = 16;
/// Largest ciphertext chunk on the wire (excluding its 4-byte length prefix).
pub const FILE_MAX_CHUNK_CIPHERTEXT: usize = FILE_CHUNK_SIZE + FILE_TAG_SIZE;
/// Length of the per-chunk length prefix (u32 little-endian).
pub const FILE_CHUNK_PREFIX_LEN: usize = 4;
/// Length of `file_id`.
pub const FILE_ID_LEN: usize = 16;
/// Length of the per-file key.
pub const FILE_KEY_LEN: usize = 32;

/// Cap on total ciphertext per transfer: 25 MiB. Counts tags, not length
/// prefixes. The server refuses larger declared sizes and aborts a stream
/// that sends more than it declared.
pub const MAX_FILE_CIPHERTEXT_SIZE: u64 = 25 * 1024 * 1024;

/// Longest channel id a header may carry.
pub const FILE_MAX_CHANNEL_ID_LEN: usize = 128;

/// Bytes before the variable-length channel id.
pub const FILE_HEADER_FIXED_LEN: usize = 36;

/// QUIC application error codes used to refuse or abort a transfer.
///
/// The server sends these as `STOP_SENDING` to an uploader and `RESET_STREAM`
/// to a recipient.
pub mod error_code {
    /// Header unreadable, wrong kind/version, or fields inconsistent.
    pub const MALFORMED_HEADER: u32 = 1;
    /// Declared ciphertext size over `MAX_FILE_CIPHERTEXT_SIZE`.
    pub const TOO_LARGE: u32 = 2;
    /// Sender is not currently in the channel named by the header.
    pub const NOT_IN_CHANNEL: u32 = 3;
    /// Per-user transfer limit (concurrency or bytes/minute) exceeded.
    pub const RATE_LIMITED: u32 = 4;
    /// Chunk framing disagrees with the header, or more bytes than declared.
    pub const PROTOCOL_VIOLATION: u32 = 5;
    /// Sender left the channel or disconnected mid-transfer (to recipients).
    pub const SENDER_LEFT: u32 = 6;
    /// Recipient could not keep up; its copy was dropped (to recipients).
    pub const RECIPIENT_TOO_SLOW: u32 = 7;
    /// Sender stopped sending for too long.
    pub const SENDER_STALLED: u32 = 8;
    /// Nobody else is in the channel, so there is nothing to relay.
    pub const NO_RECIPIENTS: u32 = 9;
    /// Server is shutting down or otherwise unable to relay.
    pub const SERVER_UNAVAILABLE: u32 = 10;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileHeaderError {
    #[error("header too short")]
    TooShort,
    #[error("unknown stream kind {0:#04x}")]
    BadKind(u8),
    #[error("unsupported header version {0}")]
    BadVersion(u8),
    #[error("channel id is not valid UTF-8")]
    BadChannelId,
    #[error("channel id too long")]
    ChannelIdTooLong,
    #[error("chunk count must be at least 1")]
    ZeroChunks,
    #[error("declared size exceeds the 25 MiB cap")]
    TooLarge,
    #[error("declared size is inconsistent with the chunk count")]
    InconsistentSize,
    #[error("trailing bytes after header")]
    Trailing,
}

/// The header that opens a file-transfer stream, in both directions.
///
/// Uploads carry `sender_session_id == 0`; the server overwrites it with the
/// real value on delivery so a recipient can tell which text message's
/// manifest (same `file_id`) this belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStreamHeader {
    pub sender_session_id: u32,
    pub channel_id: String,
    pub file_id: [u8; FILE_ID_LEN],
    /// Sum of all chunk ciphertext lengths (tags included, prefixes excluded).
    pub total_ciphertext_len: u64,
    pub chunk_count: u32,
}

/// Number of chunks for a plaintext of `size` bytes: `max(1, ceil(size / 64 KiB))`.
pub fn chunk_count_for_size(size: u64) -> u64 {
    size.div_ceil(FILE_CHUNK_SIZE as u64).max(1)
}

/// Total ciphertext length for a plaintext of `size` bytes.
pub fn ciphertext_len_for_size(size: u64) -> u64 {
    size + chunk_count_for_size(size) * FILE_TAG_SIZE as u64
}

/// Largest plaintext that fits under `MAX_FILE_CIPHERTEXT_SIZE`.
pub fn max_plaintext_size() -> u64 {
    // Full chunks cost 16 bytes of tag per 64 KiB.
    let full = FILE_MAX_CHUNK_CIPHERTEXT as u64;
    let chunks = MAX_FILE_CIPHERTEXT_SIZE / full;
    let rem = MAX_FILE_CIPHERTEXT_SIZE % full;
    chunks * FILE_CHUNK_SIZE as u64 + rem.saturating_sub(FILE_TAG_SIZE as u64)
}

impl FileStreamHeader {
    /// Build a header for an upload of `plaintext_size` bytes.
    pub fn for_upload(
        channel_id: &str,
        file_id: [u8; FILE_ID_LEN],
        plaintext_size: u64,
    ) -> Result<Self, FileHeaderError> {
        let h = Self {
            sender_session_id: 0,
            channel_id: channel_id.to_string(),
            file_id,
            total_ciphertext_len: ciphertext_len_for_size(plaintext_size),
            chunk_count: u32::try_from(chunk_count_for_size(plaintext_size))
                .map_err(|_| FileHeaderError::TooLarge)?,
        };
        h.validate()?;
        Ok(h)
    }

    /// Serialize: kind, version, sender session id (u32 LE), file id,
    /// total ciphertext length (u64 LE), chunk count (u32 LE), channel id
    /// length (u16 LE), channel id (UTF-8).
    pub fn encode(&self) -> Vec<u8> {
        let ch = self.channel_id.as_bytes();
        let mut out = Vec::with_capacity(FILE_HEADER_FIXED_LEN + ch.len());
        out.push(FILE_STREAM_KIND);
        out.push(FILE_STREAM_VERSION);
        out.extend_from_slice(&self.sender_session_id.to_le_bytes());
        out.extend_from_slice(&self.file_id);
        out.extend_from_slice(&self.total_ciphertext_len.to_le_bytes());
        out.extend_from_slice(&self.chunk_count.to_le_bytes());
        out.extend_from_slice(&(ch.len() as u16).to_le_bytes());
        out.extend_from_slice(ch);
        out
    }

    /// Given the first `FILE_HEADER_FIXED_LEN` bytes of a stream, the number
    /// of channel-id bytes that follow. Lets a reader pull the header off a
    /// stream in two reads.
    pub fn channel_id_len_from_prefix(prefix: &[u8]) -> Result<usize, FileHeaderError> {
        if prefix.len() < FILE_HEADER_FIXED_LEN {
            return Err(FileHeaderError::TooShort);
        }
        if prefix[0] != FILE_STREAM_KIND {
            return Err(FileHeaderError::BadKind(prefix[0]));
        }
        if prefix[1] != FILE_STREAM_VERSION {
            return Err(FileHeaderError::BadVersion(prefix[1]));
        }
        let len = u16::from_le_bytes([prefix[34], prefix[35]]) as usize;
        if len > FILE_MAX_CHANNEL_ID_LEN {
            return Err(FileHeaderError::ChannelIdTooLong);
        }
        Ok(len)
    }

    /// Parse a complete header (exactly the bytes `encode` produces) and
    /// validate its fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, FileHeaderError> {
        let ch_len = Self::channel_id_len_from_prefix(bytes)?;
        let total = FILE_HEADER_FIXED_LEN + ch_len;
        if bytes.len() < total {
            return Err(FileHeaderError::TooShort);
        }
        if bytes.len() > total {
            return Err(FileHeaderError::Trailing);
        }
        let mut file_id = [0u8; FILE_ID_LEN];
        file_id.copy_from_slice(&bytes[6..22]);
        let channel_id = std::str::from_utf8(&bytes[FILE_HEADER_FIXED_LEN..total])
            .map_err(|_| FileHeaderError::BadChannelId)?
            .to_string();
        let h = Self {
            sender_session_id: u32::from_le_bytes(bytes[2..6].try_into().unwrap()),
            channel_id,
            file_id,
            total_ciphertext_len: u64::from_le_bytes(bytes[22..30].try_into().unwrap()),
            chunk_count: u32::from_le_bytes(bytes[30..34].try_into().unwrap()),
        };
        h.validate()?;
        Ok(h)
    }

    /// Check the size fields against each other and the cap.
    ///
    /// Every chunk but the last is exactly `FILE_MAX_CHUNK_CIPHERTEXT` bytes;
    /// the last holds between 0 and 64 KiB of plaintext (0 only for an empty
    /// file, which is a single tag-only chunk).
    pub fn validate(&self) -> Result<(), FileHeaderError> {
        if self.chunk_count == 0 {
            return Err(FileHeaderError::ZeroChunks);
        }
        if self.channel_id.len() > FILE_MAX_CHANNEL_ID_LEN {
            return Err(FileHeaderError::ChannelIdTooLong);
        }
        if self.total_ciphertext_len > MAX_FILE_CIPHERTEXT_SIZE {
            return Err(FileHeaderError::TooLarge);
        }
        let full = (self.chunk_count as u64 - 1) * FILE_MAX_CHUNK_CIPHERTEXT as u64;
        let min_last = if self.chunk_count == 1 {
            FILE_TAG_SIZE as u64
        } else {
            FILE_TAG_SIZE as u64 + 1
        };
        let lo = full + min_last;
        let hi = full + FILE_MAX_CHUNK_CIPHERTEXT as u64;
        if self.total_ciphertext_len < lo || self.total_ciphertext_len > hi {
            return Err(FileHeaderError::InconsistentSize);
        }
        Ok(())
    }

    /// Exact ciphertext length (no prefix) of chunk `index`, or `None` if out
    /// of range. Assumes `validate()` passed.
    pub fn chunk_ciphertext_len(&self, index: u32) -> Option<usize> {
        if index >= self.chunk_count {
            return None;
        }
        if index + 1 < self.chunk_count {
            Some(FILE_MAX_CHUNK_CIPHERTEXT)
        } else {
            let full = (self.chunk_count as u64 - 1) * FILE_MAX_CHUNK_CIPHERTEXT as u64;
            Some((self.total_ciphertext_len - full) as usize)
        }
    }
}

/// Prefix `ciphertext` with its u32 LE length, as it goes on the wire.
pub fn frame_chunk(ciphertext: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FILE_CHUNK_PREFIX_LEN + ciphertext.len());
    out.extend_from_slice(&(ciphertext.len() as u32).to_le_bytes());
    out.extend_from_slice(ciphertext);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(size: u64) -> FileStreamHeader {
        FileStreamHeader::for_upload("C_1", [7; 16], size).unwrap()
    }

    #[test]
    fn roundtrip() {
        let mut h = hdr(200_000);
        h.sender_session_id = 9;
        let bytes = h.encode();
        assert_eq!(FileStreamHeader::decode(&bytes).unwrap(), h);
        assert_eq!(
            FileStreamHeader::channel_id_len_from_prefix(&bytes[..FILE_HEADER_FIXED_LEN]).unwrap(),
            3
        );
    }

    #[test]
    fn known_answer_header() {
        let h = FileStreamHeader {
            sender_session_id: 0x0102_0304,
            channel_id: "ab".into(),
            file_id: [0xAA; 16],
            total_ciphertext_len: 16,
            chunk_count: 1,
        };
        let mut want = vec![0x01, 0x01, 0x04, 0x03, 0x02, 0x01];
        want.extend_from_slice(&[0xAA; 16]);
        want.extend_from_slice(&16u64.to_le_bytes());
        want.extend_from_slice(&1u32.to_le_bytes());
        want.extend_from_slice(&[2, 0, b'a', b'b']);
        assert_eq!(h.encode(), want);
    }

    #[test]
    fn size_arithmetic() {
        assert_eq!(chunk_count_for_size(0), 1);
        assert_eq!(chunk_count_for_size(1), 1);
        assert_eq!(chunk_count_for_size(65536), 1);
        assert_eq!(chunk_count_for_size(65537), 2);
        assert_eq!(ciphertext_len_for_size(0), 16);
        assert_eq!(ciphertext_len_for_size(65536), 65552);
        assert_eq!(max_plaintext_size(), 26_208_000);
        assert!(ciphertext_len_for_size(max_plaintext_size()) <= MAX_FILE_CIPHERTEXT_SIZE);
        assert!(ciphertext_len_for_size(max_plaintext_size() + 1) > MAX_FILE_CIPHERTEXT_SIZE);
    }

    #[test]
    fn rejects_bad_headers() {
        let mut h = hdr(100);
        h.total_ciphertext_len = MAX_FILE_CIPHERTEXT_SIZE + 1;
        assert_eq!(h.validate(), Err(FileHeaderError::TooLarge));
        let mut h = hdr(100);
        h.chunk_count = 0;
        assert_eq!(h.validate(), Err(FileHeaderError::ZeroChunks));
        let mut h = hdr(100);
        h.chunk_count = 3; // far too many chunks for 116 bytes
        assert_eq!(h.validate(), Err(FileHeaderError::InconsistentSize));
        // Multi-chunk with an empty last chunk is not canonical.
        let h = FileStreamHeader {
            total_ciphertext_len: FILE_MAX_CHUNK_CIPHERTEXT as u64 + 16,
            chunk_count: 2,
            ..hdr(100)
        };
        assert_eq!(h.validate(), Err(FileHeaderError::InconsistentSize));

        let mut bytes = hdr(100).encode();
        bytes[0] = 0x02;
        assert_eq!(
            FileStreamHeader::decode(&bytes),
            Err(FileHeaderError::BadKind(2))
        );
        let mut bytes = hdr(100).encode();
        bytes[1] = 9;
        assert_eq!(
            FileStreamHeader::decode(&bytes),
            Err(FileHeaderError::BadVersion(9))
        );
        let mut bytes = hdr(100).encode();
        bytes.push(0);
        assert_eq!(
            FileStreamHeader::decode(&bytes),
            Err(FileHeaderError::Trailing)
        );
        let bytes = hdr(100).encode();
        assert_eq!(
            FileStreamHeader::decode(&bytes[..bytes.len() - 1]),
            Err(FileHeaderError::TooShort)
        );
    }

    #[test]
    fn chunk_lengths_sum_to_total() {
        for size in [0u64, 1, 65535, 65536, 65537, 200_000, 131_072] {
            let h = hdr(size);
            let sum: usize = (0..h.chunk_count)
                .map(|i| h.chunk_ciphertext_len(i).unwrap())
                .sum();
            assert_eq!(sum as u64, h.total_ciphertext_len, "size {size}");
            assert!(h.chunk_ciphertext_len(h.chunk_count).is_none());
        }
    }
}
