//! Audio Pipeline for DAVE-over-QUIC
//!
//! Ties together Opus encoding, DAVE encryption, and packet framing
//! for the send/receive hot paths.

use bytes::Bytes;
use std::sync::atomic::{AtomicU16, AtomicU64, AtomicBool, Ordering};
use std::sync::RwLock;
use std::collections::HashMap;

use crate::opus::{OpusCodec, OpusError};
use crate::crypto::{DaveCrypto, CryptoError};
use crate::jitter_buffer::{JitterBuffer, JitterBufferConfig, PopResult};
use crate::noise_suppression::NoiseSuppressor;
#[cfg(feature = "webrtc-audio")]
use crate::webrtc_processor::WebRtcProcessor;
use aura_protocol::FastAudioPacket;

/// Mixed audio output with speaker metadata
#[derive(Debug, Clone)]
pub struct MixedAudio {
    /// Mixed PCM samples (960 samples, 20ms at 48kHz)
    pub pcm: Vec<i16>,
    /// Session IDs that contributed to this mix
    pub active_speakers: Vec<u32>,
}

/// Audio sender for transmitting encrypted Opus audio
pub struct AudioSender {
    /// Opus encoder
    codec: OpusCodec,
    /// Encryption context (mutable for epoch key rotation)
    crypto: RwLock<DaveCrypto>,
    /// Noise suppressor (RNNoise)
    noise_suppressor: std::sync::Mutex<NoiseSuppressor>,
    /// Enable/disable RNNoise at runtime
    enable_rnnoise: AtomicBool,
    
    /// WebRTC processor (AEC/NS/AGC) - optional feature
    #[cfg(feature = "webrtc-audio")]
    webrtc_processor: Option<std::sync::Mutex<WebRtcProcessor>>,
    #[cfg(feature = "webrtc-audio")]
    enable_webrtc_aec: AtomicBool,
    #[cfg(feature = "webrtc-audio")]
    enable_webrtc_ns: AtomicBool,
    #[cfg(feature = "webrtc-audio")]
    enable_webrtc_agc: AtomicBool,
    
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
        
        #[cfg(feature = "webrtc-audio")]
        let webrtc_processor = WebRtcProcessor::new(false, false, false)
            .ok()
            .map(|p| std::sync::Mutex::new(p));
        
        Ok(Self {
            codec,
            crypto: RwLock::new(crypto),
            noise_suppressor: std::sync::Mutex::new(NoiseSuppressor::new()),
            enable_rnnoise: AtomicBool::new(true), // Enabled by default
            
            #[cfg(feature = "webrtc-audio")]
            webrtc_processor,
            #[cfg(feature = "webrtc-audio")]
            enable_webrtc_aec: AtomicBool::new(false),
            #[cfg(feature = "webrtc-audio")]
            enable_webrtc_ns: AtomicBool::new(false),
            #[cfg(feature = "webrtc-audio")]
            enable_webrtc_agc: AtomicBool::new(false),
            
            session_id,
            epoch_hint: AtomicU16::new(0),
            sequence: AtomicU16::new(0),
        })
    }
    
    /// Set the current MLS epoch hint
    pub fn set_epoch(&self, epoch: u64) {
        self.epoch_hint.store((epoch & 0xFFFF) as u16, Ordering::SeqCst);
    }
    
    /// Enable or disable RNNoise at runtime
    pub fn set_rnnoise_enabled(&self, enabled: bool) {
        self.enable_rnnoise.store(enabled, Ordering::SeqCst);
    }
    
    /// Enable or disable WebRTC AEC at runtime
    #[cfg(feature = "webrtc-audio")]
    pub fn set_webrtc_aec_enabled(&self, enabled: bool) {
        self.enable_webrtc_aec.store(enabled, Ordering::SeqCst);
        if let Some(proc) = &self.webrtc_processor {
            let mut p = proc.lock().unwrap();
            p.reconfigure(enabled, 
                self.enable_webrtc_ns.load(Ordering::SeqCst),
                self.enable_webrtc_agc.load(Ordering::SeqCst));
        }
    }
    
    /// Enable or disable WebRTC NS at runtime
    #[cfg(feature = "webrtc-audio")]
    pub fn set_webrtc_ns_enabled(&self, enabled: bool) {
        self.enable_webrtc_ns.store(enabled, Ordering::SeqCst);
        if let Some(proc) = &self.webrtc_processor {
            let mut p = proc.lock().unwrap();
            p.reconfigure(
                self.enable_webrtc_aec.load(Ordering::SeqCst),
                enabled,
                self.enable_webrtc_agc.load(Ordering::SeqCst));
        }
    }
    
    /// Enable or disable WebRTC AGC at runtime
    #[cfg(feature = "webrtc-audio")]
    pub fn set_webrtc_agc_enabled(&self, enabled: bool) {
        self.enable_webrtc_agc.store(enabled, Ordering::SeqCst);
        if let Some(proc) = &self.webrtc_processor {
            let mut p = proc.lock().unwrap();
            p.reconfigure(
                self.enable_webrtc_aec.load(Ordering::SeqCst),
                self.enable_webrtc_ns.load(Ordering::SeqCst),
                enabled);
        }
    }
    
    /// Update the encryption key (called when MLS epoch advances)
    pub fn update_key(&self, new_key: &[u8; 32], new_epoch: u64) {
        let mut crypto = self.crypto.write().unwrap();
        *crypto = DaveCrypto::new(new_key);
        self.set_epoch(new_epoch);
    }
    
    /// Set DRED duration (number of 10ms frames of redundancy)
    /// 
    /// Example: duration=10 means 100ms of redundancy
    pub fn set_dred_duration(&self, duration: i32) -> Result<(), AudioPipelineError> {
        self.codec.set_dred_duration(duration)
            .map_err(AudioPipelineError::Opus)
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
    /// 
    /// `reference`: Optional playback audio for AEC (only needed if WebRTC AEC is enabled)
    pub fn process_float_with_reference(&self, pcm: &[f32], _reference: Option<&[f32]>) -> Result<Bytes, AudioPipelineError> {
        let mut processed = pcm.to_vec();
        
        // 1. WebRTC processing (AEC/NS/AGC)
        #[cfg(feature = "webrtc-audio")]
        if let Some(proc) = &self.webrtc_processor {
            let mut p = proc.lock().unwrap();
            processed = p.process(&processed, reference);
        }
        
        // 2. RNNoise (if enabled and WebRTC NS is disabled)
        #[cfg(feature = "webrtc-audio")]
        let use_rnnoise = self.enable_rnnoise.load(Ordering::SeqCst) 
            && !self.enable_webrtc_ns.load(Ordering::SeqCst);
        
        #[cfg(not(feature = "webrtc-audio"))]
        let use_rnnoise = self.enable_rnnoise.load(Ordering::SeqCst);
        
        if use_rnnoise {
            let mut suppressor = self.noise_suppressor.lock().unwrap();
            processed = suppressor.process(&processed);
        }
        
        // 3. Encode to Opus using native float API
        let opus_data = self.codec.encode_float(&processed).map_err(AudioPipelineError::Opus)?;
        
        // 4. Generate nonce and encrypt
        let nonce = DaveCrypto::random_nonce();
        let ciphertext = {
            let crypto = self.crypto.read().unwrap();
            crypto.encrypt(&opus_data, &nonce)
                .map_err(AudioPipelineError::Crypto)?
        };
        
        // 5. Build packet
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst);
        let epoch_hint = self.epoch_hint.load(Ordering::SeqCst);
        
        let packet = FastAudioPacket {
            session_id: self.session_id,
            epoch_hint,
            sequence,
            nonce,
            payload: Bytes::from(ciphertext),
        };
        
        Ok(packet.to_bytes())
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
    /// Decryption keys indexed by epoch_hint (retains old epochs for handover)
    key_store: HashMap<u16, DaveCrypto>,
    /// Current epoch hint
    current_epoch: u16,
    /// Jitter buffer
    jitter: JitterBuffer,
    /// Last processed sequence number (for replay protection)
    last_sequence: u16,
    /// Local playback gain multiplier (1.0 = unchanged, 0.0..4.0 permitted)
    gain: f32,
    /// Local-only mute — audio still decodes (to keep Opus state healthy)
    /// but is dropped during mixing.
    muted: bool,
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
    
    /// Set jitter buffer target latency for all senders
    pub fn set_jitter_buffer_ms(&self, latency_ms: u32) {
        let mut senders = self.senders.write().unwrap();
        for state in senders.values_mut() {
            state.jitter.set_target_latency(latency_ms);
        }
    }
    
    /// Register a sender with their decryption key
    pub fn add_sender(&self, session_id: u32, key: &[u8; 32], epoch_hint: u16) -> Result<(), AudioPipelineError> {
        let codec = OpusCodec::new().map_err(AudioPipelineError::Opus)?;
        let crypto = DaveCrypto::new(key);
        let jitter = JitterBuffer::new(self.jitter_config.clone());
        
        let mut key_store = HashMap::new();
        key_store.insert(epoch_hint, crypto);
        
        let state = SenderState {
            codec,
            key_store,
            current_epoch: epoch_hint,
            jitter,
            last_sequence: 0,
            gain: 1.0,
            muted: false,
        };
        self.senders.write().unwrap().insert(session_id, state);
        Ok(())
    }

    /// Set per-sender local playback gain. Returns `true` if the sender was
    /// known. Clamped to `[0.0, 4.0]` so a runaway value can't blow speakers.
    pub fn set_sender_gain(&self, session_id: u32, gain: f32) -> bool {
        let mut senders = self.senders.write().unwrap();
        if let Some(state) = senders.get_mut(&session_id) {
            state.gain = gain.clamp(0.0, 4.0);
            true
        } else {
            false
        }
    }

    /// Toggle local-only mute for a sender. Audio continues to decode so
    /// the Opus decoder's internal state stays consistent, but the mixer
    /// drops the samples on the floor.
    pub fn set_sender_muted(&self, session_id: u32, muted: bool) -> bool {
        let mut senders = self.senders.write().unwrap();
        if let Some(state) = senders.get_mut(&session_id) {
            state.muted = muted;
            true
        } else {
            false
        }
    }
    
    /// Remove a sender (e.g., when they leave the channel)
    pub fn remove_sender(&self, session_id: u32) {
        self.senders.write().unwrap().remove(&session_id);
    }
    
    /// Update a sender's decryption key (called when MLS epoch advances)
    /// 
    /// Retains old keys for graceful epoch handover (~10s worth of packets).
    /// Old epochs are pruned when more than 3 are stored.
    pub fn update_sender_key(&self, session_id: u32, new_key: &[u8; 32], new_epoch: u16) -> bool {
        if let Some(state) = self.senders.write().unwrap().get_mut(&session_id) {
            // Add new key
            state.key_store.insert(new_epoch, DaveCrypto::new(new_key));
            state.current_epoch = new_epoch;
            
            // Prune old epochs (keep at most 3)
            while state.key_store.len() > 3 {
                // Find and remove the oldest epoch
                if let Some(&oldest) = state.key_store.keys()
                    .filter(|&&e| e != state.current_epoch)
                    .min() 
                {
                    state.key_store.remove(&oldest);
                }
            }
            true
        } else {
            false
        }
    }
    
    /// Process a received packet
    /// 
    /// Pipeline: Parse -> Try decrypt with epoch_hint key -> Fallback to other epochs -> Insert
    pub fn on_packet(&self, data: Bytes) -> Result<(), AudioPipelineError> {
        // 1. Parse packet
        let packet = FastAudioPacket::parse(data)
            .map_err(|e| AudioPipelineError::PacketParse(e.to_string()))?;
        
        // 2. Get sender state
        let mut senders = self.senders.write().unwrap();
        let state = senders.get_mut(&packet.session_id)
            .ok_or(AudioPipelineError::UnknownSender(packet.session_id))?;
        
        // 3. Replay protection: reject if sequence is too old
        let seq_diff = packet.sequence.wrapping_sub(state.last_sequence);
        if seq_diff > 32768 && seq_diff != 0 {
            // Sequence went backwards by more than half the range - likely replay
            return Err(AudioPipelineError::PacketParse("Replayed packet".into()));
        }
        state.last_sequence = state.last_sequence.max(packet.sequence);
        
        // 4. Try decryption with epoch_hint key first, then fallback
        let opus_data = if let Some(crypto) = state.key_store.get(&packet.epoch_hint) {
            crypto.decrypt(&packet.payload, &packet.nonce)
                .map_err(AudioPipelineError::Crypto)?
        } else {
            // Fallback: try other epochs (for packets in-flight during transition)
            let mut decrypted = None;
            for crypto in state.key_store.values() {
                if let Ok(data) = crypto.decrypt(&packet.payload, &packet.nonce) {
                    decrypted = Some(data);
                    break;
                }
            }
            decrypted.ok_or(AudioPipelineError::Crypto(
                crate::crypto::CryptoError::DecryptionFailed
            ))?
        };
        
        // 5. Insert into jitter buffer
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
                PopResult::Packet(opus_data) => {
                    if let Ok(pcm) = state.codec.decode(&opus_data, false) {
                        results.push((session_id, pcm));
                    }
                }
                PopResult::PacketWithGap { data: opus_data, lost } => {
                    // If we have a gap of 1 packet, we can try to use FEC from this packet
                    // to recover the missing one before playing THIS packet.
                    if lost == 1 {
                        if let Ok(recovered_pcm) = state.codec.decode(&opus_data, true) {
                            results.push((session_id, recovered_pcm));
                        }
                    }
                    
                    // Then play the actual current packet
                    if let Ok(pcm) = state.codec.decode(&opus_data, false) {
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
    
    /// Mix all decoded frames into a single output with speaker metadata
    pub fn pop_mixed(&self) -> Option<MixedAudio> {
        let frames = self.pop_decoded();
        if frames.is_empty() {
            return None;
        }

        // Snapshot per-sender gain/mute in a single read-lock acquisition
        // so the mixing loop below doesn't re-lock for every frame.
        let sender_cfg: HashMap<u32, (f32, bool)> = {
            let senders = self.senders.read().unwrap();
            senders
                .iter()
                .map(|(id, s)| (*id, (s.gain, s.muted)))
                .collect()
        };

        let frame_size = 960; // 20ms at 48kHz
        let mut mixed = vec![0i32; frame_size];
        let mut active_speakers = Vec::new();

        for (session_id, pcm) in &frames {
            let (gain, muted) = sender_cfg
                .get(session_id)
                .copied()
                .unwrap_or((1.0, false));

            if muted {
                continue;
            }

            active_speakers.push(*session_id);

            if (gain - 1.0).abs() < f32::EPSILON {
                // Fast path — no floating-point multiply when gain is unchanged.
                for (i, &sample) in pcm.iter().enumerate().take(frame_size) {
                    mixed[i] += sample as i32;
                }
            } else {
                for (i, &sample) in pcm.iter().enumerate().take(frame_size) {
                    mixed[i] += (sample as f32 * gain) as i32;
                }
            }
        }

        // Clip to i16 range
        let pcm = mixed.iter()
            .map(|&s| s.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
            .collect();

        Some(MixedAudio {
            pcm,
            active_speakers,
        })
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
        receiver.add_sender(session_id, &key, 0).expect("Add sender");
        
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
        
        receiver.add_sender(1, &key1, 0).expect("Add sender 1");
        receiver.add_sender(2, &key2, 0).expect("Add sender 2");
        
        let pcm = vec![1000i16; 960];
        
        // Both send
        let pkt1 = sender1.process(&pcm).expect("Send 1");
        let pkt2 = sender2.process(&pcm).expect("Send 2");
        
        receiver.on_packet(pkt1).expect("Receive 1");
        receiver.on_packet(pkt2).expect("Receive 2");
        
        // Should get mixed output with both speakers
        let mixed = receiver.pop_mixed();
        assert!(mixed.is_some());
        let mixed = mixed.unwrap();
        assert_eq!(mixed.active_speakers.len(), 2);
        assert!(mixed.active_speakers.contains(&1));
        assert!(mixed.active_speakers.contains(&2));
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
