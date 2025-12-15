//! Audio Pipeline for DAVE-over-QUIC
//!
//! Ties together Opus encoding, DAVE encryption, and packet framing
//! for the send/receive hot paths.

use bytes::Bytes;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

use crate::opus::{OpusCodec, OpusError};
use crate::crypto::{DaveCrypto, CryptoError, NONCE_SIZE};
use crate::jitter_buffer::{JitterBuffer, JitterBufferConfig, PopResult};
use aura_protocol::FastAudioPacket;

/// Audio sender for transmitting encrypted Opus audio
pub struct AudioSender {
    /// Opus encoder
    codec: OpusCodec,
    /// Encryption context (mutable for epoch key rotation)
    crypto: RwLock<DaveCrypto>,
    /// Our session ID
    session_id: u32,
    /// Current MLS epoch hint
    epoch_hint: AtomicU16,
    /// Packet sequence number
    sequence: AtomicU16,
}

impl AudioSender {
    /// Create a new audio sender
    pub fn new(session_id: u32, key: &[u8; 32]) -> Result<Self, AudioPipelineError> {
        let codec = OpusCodec::new().map_err(AudioPipelineError::Opus)?;
        let crypto = DaveCrypto::new(key);
        
        Ok(Self {
            codec,
            crypto: RwLock::new(crypto),
            session_id,
            epoch_hint: AtomicU16::new(0),
            sequence: AtomicU16::new(0),
        })
    }
    
    /// Set the current MLS epoch hint
    pub fn set_epoch(&self, epoch: u64) {
        self.epoch_hint.store((epoch & 0xFFFF) as u16, Ordering::SeqCst);
    }
    
    /// Update the encryption key (called when MLS epoch advances)
    pub fn update_key(&self, new_key: &[u8; 32], new_epoch: u64) {
        let mut crypto = self.crypto.write().unwrap();
        *crypto = DaveCrypto::new(new_key);
        self.set_epoch(new_epoch);
    }
    
    /// Encode and encrypt PCM audio for transmission
    /// 
    /// Pipeline: PCM -> Opus -> Zero-pad -> XChaCha20-Poly1305 -> Packet
    /// 
    /// Input: 960 samples of i16 PCM (20ms at 48kHz)
    /// Output: Serialized FastAudioPacket ready for QUIC datagram
    pub fn process(&self, pcm: &[i16]) -> Result<Bytes, AudioPipelineError> {
        // 1. Encode to Opus
        let opus_data = self.codec.encode(pcm).map_err(AudioPipelineError::Opus)?;
        
        // 2. Generate nonce and encrypt
        let nonce = DaveCrypto::random_nonce();
        let ciphertext = {
            let crypto = self.crypto.read().unwrap();
            crypto.encrypt(&opus_data, &nonce)
                .map_err(AudioPipelineError::Crypto)?
        };
        
        // 3. Build packet
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst);
        let epoch_hint = self.epoch_hint.load(Ordering::SeqCst);
        
        let packet = FastAudioPacket {
            session_id: self.session_id,
            epoch_hint,
            sequence,
            nonce,
            payload: Bytes::from(ciphertext),
        };
        
        // 4. Serialize
        Ok(packet.to_bytes())
    }
    
    /// Encode and encrypt f32 PCM audio
    pub fn process_float(&self, pcm: &[f32]) -> Result<Bytes, AudioPipelineError> {
        // Convert f32 to i16
        let pcm_i16: Vec<i16> = pcm.iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        self.process(&pcm_i16)
    }
    
    /// Get current sequence number
    pub fn sequence(&self) -> u16 {
        self.sequence.load(Ordering::SeqCst)
    }
}

/// Audio receiver for decrypting and decoding incoming audio
pub struct AudioReceiver {
    /// Per-sender state
    senders: RwLock<HashMap<u32, SenderState>>,
    /// Default jitter buffer config
    jitter_config: JitterBufferConfig,
}

/// State for a single remote sender
struct SenderState {
    /// Opus decoder (per-sender for independent state)
    codec: OpusCodec,
    /// Decryption context
    crypto: DaveCrypto,
    /// Jitter buffer
    jitter: JitterBuffer,
}

impl AudioReceiver {
    /// Create a new audio receiver
    pub fn new() -> Self {
        Self {
            senders: RwLock::new(HashMap::new()),
            jitter_config: JitterBufferConfig::default(),
        }
    }
    
    /// Create with custom jitter buffer config
    pub fn with_config(jitter_config: JitterBufferConfig) -> Self {
        Self {
            senders: RwLock::new(HashMap::new()),
            jitter_config,
        }
    }
    
    /// Register a sender with their decryption key
    pub fn add_sender(&self, session_id: u32, key: &[u8; 32]) -> Result<(), AudioPipelineError> {
        let codec = OpusCodec::new().map_err(AudioPipelineError::Opus)?;
        let crypto = DaveCrypto::new(key);
        let jitter = JitterBuffer::new(self.jitter_config.clone());
        
        let state = SenderState { codec, crypto, jitter };
        self.senders.write().unwrap().insert(session_id, state);
        Ok(())
    }
    
    /// Remove a sender (e.g., when they leave the channel)
    pub fn remove_sender(&self, session_id: u32) {
        self.senders.write().unwrap().remove(&session_id);
    }
    
    /// Update a sender's decryption key (called when MLS epoch advances)
    pub fn update_sender_key(&self, session_id: u32, new_key: &[u8; 32]) -> bool {
        if let Some(state) = self.senders.write().unwrap().get_mut(&session_id) {
            state.crypto = DaveCrypto::new(new_key);
            true
        } else {
            false
        }
    }
    
    /// Process a received packet
    /// 
    /// Pipeline: Parse -> Decrypt -> Remove padding -> Insert into jitter buffer
    pub fn on_packet(&self, data: Bytes) -> Result<(), AudioPipelineError> {
        // 1. Parse packet
        let packet = FastAudioPacket::parse(data)
            .map_err(|e| AudioPipelineError::PacketParse(e.to_string()))?;
        
        // 2. Get sender state
        let mut senders = self.senders.write().unwrap();
        let state = senders.get_mut(&packet.session_id)
            .ok_or(AudioPipelineError::UnknownSender(packet.session_id))?;
        
        // 3. Decrypt
        let opus_data = state.crypto.decrypt(&packet.payload, &packet.nonce)
            .map_err(AudioPipelineError::Crypto)?;
        
        // 4. Insert into jitter buffer
        state.jitter.push(
            packet.sequence as u64,
            packet.sequence as u32 * 960, // Approximate timestamp
            Bytes::from(opus_data),
        );
        
        Ok(())
    }
    
    /// Pop and decode ready frames from all senders
    /// 
    /// Returns: Vec of (session_id, decoded PCM samples)
    pub fn pop_decoded(&self) -> Vec<(u32, Vec<i16>)> {
        let mut senders = self.senders.write().unwrap();
        let mut results = Vec::new();
        
        for (&session_id, state) in senders.iter_mut() {
            match state.jitter.pop() {
                PopResult::Packet(opus_data) | PopResult::PacketWithGap { data: opus_data, .. } => {
                    if let Ok(pcm) = state.codec.decode(&opus_data) {
                        results.push((session_id, pcm));
                    }
                }
                PopResult::NeedPLC => {
                    // Generate concealment audio
                    if let Ok(pcm) = state.codec.decode_plc() {
                        results.push((session_id, pcm));
                    }
                }
                PopResult::Empty => {}
            }
        }
        
        results
    }
    
    /// Mix all decoded frames into a single output
    pub fn pop_mixed(&self) -> Option<Vec<i16>> {
        let frames = self.pop_decoded();
        if frames.is_empty() {
            return None;
        }
        
        let frame_size = 960; // 20ms at 48kHz
        let mut mixed = vec![0i32; frame_size];
        
        for (_, pcm) in &frames {
            for (i, &sample) in pcm.iter().enumerate().take(frame_size) {
                mixed[i] += sample as i32;
            }
        }
        
        // Clip to i16 range
        Some(mixed.iter()
            .map(|&s| s.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
            .collect())
    }
}

impl Default for AudioReceiver {
    fn default() -> Self {
        Self::new()
    }
}

/// Audio pipeline errors
#[derive(Debug, thiserror::Error)]
pub enum AudioPipelineError {
    #[error("Opus codec error: {0}")]
    Opus(#[from] OpusError),
    
    #[error("Crypto error: {0}")]
    Crypto(#[from] CryptoError),
    
    #[error("Packet parse error: {0}")]
    PacketParse(String),
    
    #[error("Unknown sender: {0}")]
    UnknownSender(u32),
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_sender_encode() {
        let key = DaveCrypto::random_key();
        let sender = AudioSender::new(42, &key).expect("Create sender");
        
        // Generate test PCM
        let pcm = vec![0i16; 960];
        let packet = sender.process(&pcm).expect("Process failed");
        
        // Should have header + encrypted data
        assert!(packet.len() > FastAudioPacket::HEADER_SIZE);
    }
    
    #[test]
    fn test_sender_receiver_roundtrip() {
        let key = DaveCrypto::random_key();
        let session_id = 123;
        
        // Create sender and receiver
        let sender = AudioSender::new(session_id, &key).expect("Create sender");
        let receiver = AudioReceiver::new();
        receiver.add_sender(session_id, &key).expect("Add sender");
        
        // Generate test tone
        let pcm: Vec<i16> = (0..960).map(|i| ((i % 100) * 100) as i16).collect();
        
        // Send
        let packet = sender.process(&pcm).expect("Process failed");
        
        // Receive
        receiver.on_packet(packet).expect("On packet failed");
        
        // Pop decoded
        let decoded = receiver.pop_decoded();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, session_id);
        // Opus is lossy, but length should match
        assert_eq!(decoded[0].1.len(), 960);
    }
    
    #[test]
    fn test_multiple_senders() {
        let key1 = DaveCrypto::random_key();
        let key2 = DaveCrypto::random_key();
        
        let sender1 = AudioSender::new(1, &key1).expect("Create sender 1");
        let sender2 = AudioSender::new(2, &key2).expect("Create sender 2");
        let receiver = AudioReceiver::new();
        
        receiver.add_sender(1, &key1).expect("Add sender 1");
        receiver.add_sender(2, &key2).expect("Add sender 2");
        
        let pcm = vec![1000i16; 960];
        
        // Both send
        let pkt1 = sender1.process(&pcm).expect("Send 1");
        let pkt2 = sender2.process(&pcm).expect("Send 2");
        
        receiver.on_packet(pkt1).expect("Receive 1");
        receiver.on_packet(pkt2).expect("Receive 2");
        
        // Should get mixed output
        let mixed = receiver.pop_mixed();
        assert!(mixed.is_some());
    }
    
    #[test]
    fn test_unknown_sender_rejected() {
        let receiver = AudioReceiver::new();
        
        // Create packet from unknown sender
        let key = DaveCrypto::random_key();
        let sender = AudioSender::new(999, &key).expect("Create sender");
        let pcm = vec![0i16; 960];
        let packet = sender.process(&pcm).expect("Process");
        
        // Should fail
        let result = receiver.on_packet(packet);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_sequence_increments() {
        let key = DaveCrypto::random_key();
        let sender = AudioSender::new(1, &key).expect("Create sender");
        let pcm = vec![0i16; 960];
        
        assert_eq!(sender.sequence(), 0);
        
        sender.process(&pcm).expect("Process 1");
        assert_eq!(sender.sequence(), 1);
        
        sender.process(&pcm).expect("Process 2");
        assert_eq!(sender.sequence(), 2);
    }
}
