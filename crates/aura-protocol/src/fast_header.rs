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
    
    /// Convert packet to bytes for transmission
    pub fn to_bytes(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(Self::HEADER_SIZE + self.payload.len());
        self.write(&mut buf);
        buf.freeze()
    }
    
    /// Create a packet with a deterministic nonce from sequence number
    /// Use this for testing or when nonce must be reconstructible
    pub fn with_sequence_nonce(
        session_id: u32,
        epoch_hint: u16,
        sequence: u16,
        payload: Bytes,
    ) -> Self {
        let mut nonce = [0u8; NONCE_SIZE];
        // Use session_id and sequence to create a unique nonce
        nonce[0..4].copy_from_slice(&session_id.to_le_bytes());
        nonce[4..6].copy_from_slice(&epoch_hint.to_le_bytes());
        nonce[6..8].copy_from_slice(&sequence.to_le_bytes());
        // Remaining 16 bytes are zero (or could add timestamp)
        
        Self {
            session_id,
            epoch_hint,
            sequence,
            nonce,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_header_size() {
        assert_eq!(FastAudioPacket::HEADER_SIZE, 32);
    }
    
    #[test]
    fn test_parse_write_roundtrip() {
        let original = FastAudioPacket {
            session_id: 12345,
            epoch_hint: 42,
            sequence: 99,
            nonce: [1u8; 24],
            payload: Bytes::from_static(b"hello opus data"),
        };
        
        let bytes = original.to_bytes();
        let parsed = FastAudioPacket::parse(bytes).expect("Parse failed");
        
        assert_eq!(parsed.session_id, 12345);
        assert_eq!(parsed.epoch_hint, 42);
        assert_eq!(parsed.sequence, 99);
        assert_eq!(parsed.nonce, [1u8; 24]);
        assert_eq!(parsed.payload.as_ref(), b"hello opus data");
    }
    
    #[test]
    fn test_parse_too_short() {
        let short = Bytes::from_static(&[0u8; 10]);
        let result = FastAudioPacket::parse(short);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_empty_payload() {
        let packet = FastAudioPacket {
            session_id: 1,
            epoch_hint: 0,
            sequence: 0,
            nonce: [0u8; 24],
            payload: Bytes::new(),
        };
        
        let bytes = packet.to_bytes();
        assert_eq!(bytes.len(), FastAudioPacket::HEADER_SIZE);
        
        let parsed = FastAudioPacket::parse(bytes).expect("Parse failed");
        assert!(parsed.payload.is_empty());
    }
    
    #[test]
    fn test_sequence_nonce() {
        let packet = FastAudioPacket::with_sequence_nonce(
            0x12345678,
            0xABCD,
            0x1234,
            Bytes::from_static(b"test"),
        );
        
        // Verify nonce contains session_id, epoch_hint, sequence
        assert_eq!(&packet.nonce[0..4], &0x12345678u32.to_le_bytes());
        assert_eq!(&packet.nonce[4..6], &0xABCDu16.to_le_bytes());
        assert_eq!(&packet.nonce[6..8], &0x1234u16.to_le_bytes());
    }
    
    #[cfg(feature = "rand")]
    #[test]
    fn test_random_nonce_unique() {
        let p1 = FastAudioPacket::new_with_random_nonce(1, 0, 0, Bytes::new());
        let p2 = FastAudioPacket::new_with_random_nonce(1, 0, 0, Bytes::new());
        
        // Random nonces should be different
        assert_ne!(p1.nonce, p2.nonce);
    }
    
    #[test]
    fn test_max_values() {
        let packet = FastAudioPacket {
            session_id: u32::MAX,
            epoch_hint: u16::MAX,
            sequence: u16::MAX,
            nonce: [0xFF; 24],
            payload: Bytes::from_static(b"max test"),
        };
        
        let bytes = packet.to_bytes();
        let parsed = FastAudioPacket::parse(bytes).expect("Parse failed");
        
        assert_eq!(parsed.session_id, u32::MAX);
        assert_eq!(parsed.epoch_hint, u16::MAX);
        assert_eq!(parsed.sequence, u16::MAX);
        assert_eq!(parsed.nonce, [0xFF; 24]);
    }
}
