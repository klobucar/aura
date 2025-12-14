use bytes::{Buf, BufMut, Bytes, BytesMut};
use thiserror::Error;

/// Custom Binary Header for Audio Payload (Hot Path)
/// 
/// Format: [SessionID (4)] [EpochHint (2)] [Sequence (2)] [Nonce (24)] [Ciphertext (...)]
/// Total Overhead: 32 bytes (8 byte header + 24 byte XChaCha20 nonce)
///
/// Design Rationale:
/// - EpochHint: Low 16 bits of MLS epoch. Receiver uses this to select the correct
///   decryption key when epoch changes mid-stream. Collisions are handled by trying
///   both candidate epochs.
/// - Nonce: Full 192-bit random nonce for XChaCha20-Poly1305. With 2^96 birthday bound,
///   random nonces are safe for the lifetime of any MLS epoch.
/// - Sequence: For jitter buffer ordering. Wraps at 65536 (~22 min at 50 pkt/s).
pub struct FastAudioPacket {
    pub session_id: u32,
    pub epoch_hint: u16,
    pub sequence: u16,
    pub nonce: [u8; 24],
    pub payload: Bytes, // XChaCha20-Poly1305 ciphertext (includes 16-byte auth tag)
}

/// XChaCha20 nonce size in bytes
pub const NONCE_SIZE: usize = 24;

#[derive(Error, Debug)]
pub enum PacketError {
    #[error("Packet too short: need at least {0} bytes, got {1}")]
    TooShort(usize, usize),
}

impl FastAudioPacket {
    /// Header size: SessionID(4) + EpochHint(2) + Sequence(2) + Nonce(24) = 32 bytes
    pub const HEADER_SIZE: usize = 4 + 2 + 2 + NONCE_SIZE;

    /// Zero-copy-ish parse. 
    /// Takes a Bytes object, consumes the header, and returns the struct.
    /// The payload field shares the underlying memory of the input Bytes.
    pub fn parse(mut data: Bytes) -> Result<Self, PacketError> {
        if data.len() < Self::HEADER_SIZE {
            return Err(PacketError::TooShort(Self::HEADER_SIZE, data.len()));
        }

        // Parse header fields manually using `bytes::Buf`
        let session_id = data.get_u32();
        let epoch_hint = data.get_u16();
        let sequence = data.get_u16();
        
        // Extract nonce (24 bytes)
        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(&data[..NONCE_SIZE]);
        data.advance(NONCE_SIZE);

        // Remaining bytes are the encrypted payload
        let payload = data;

        Ok(Self {
            session_id,
            epoch_hint,
            sequence,
            nonce,
            payload,
        })
    }

    /// Writes the packet to a buffer.
    pub fn write(&self, buf: &mut BytesMut) {
        buf.put_u32(self.session_id);
        buf.put_u16(self.epoch_hint);
        buf.put_u16(self.sequence);
        buf.put_slice(&self.nonce);
        buf.put(self.payload.clone());
    }
    
    /// Create a new packet with a random nonce.
    /// In production, use a cryptographically secure RNG.
    #[cfg(feature = "rand")]
    pub fn new_with_random_nonce(
        session_id: u32,
        epoch_hint: u16,
        sequence: u16,
        payload: Bytes,
    ) -> Self {
        use rand::Rng;
        let nonce: [u8; NONCE_SIZE] = rand::thread_rng().gen();
        Self {
            session_id,
            epoch_hint,
            sequence,
            nonce,
            payload,
        }
    }
}
