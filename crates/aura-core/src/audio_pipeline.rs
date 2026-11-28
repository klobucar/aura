//! Audio Pipeline for DAVE-over-QUIC
//!
//! Ties together Opus encoding, DAVE encryption, and packet framing
//! for the send/receive hot paths.

use bytes::Bytes;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Mutex, RwLock};

use crate::crypto::{CryptoError, DaveCrypto};
use crate::jitter_buffer::{JitterBuffer, JitterBufferConfig, PopResult};
use crate::noise_suppression::NoiseSuppressor;
use crate::opus::{OpusCodec, OpusError};
use crate::vad::VoiceActivityDetector;
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

    /// Voice activity detector. When `enable_vad` is true and the detector
    /// reports silence, `process*` returns `Ok(None)` so the caller skips the
    /// datagram entirely.
    vad: std::sync::Mutex<VoiceActivityDetector>,
    /// Enable/disable VAD-based silence skipping at runtime. Default: false.
    enable_vad: AtomicBool,

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

            vad: std::sync::Mutex::new(VoiceActivityDetector::default_20ms()),
            enable_vad: AtomicBool::new(false), // Off by default — opt-in per client

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
        self.epoch_hint
            .store((epoch & 0xFFFF) as u16, Ordering::SeqCst);
    }

    /// Set the Opus encoder bitrate in bits per second (clamped to `12_000..=128_000`).
    pub fn set_bitrate(&self, bps: u32) -> Result<(), AudioPipelineError> {
        self.codec
            .set_bitrate(bps)
            .map_err(AudioPipelineError::Opus)
    }

    /// Enable or disable RNNoise at runtime
    pub fn set_rnnoise_enabled(&self, enabled: bool) {
        self.enable_rnnoise.store(enabled, Ordering::SeqCst);
    }

    /// Enable or disable VAD-based silence skipping at runtime.
    /// When enabled, `process*` returns `Ok(None)` for frames the VAD classifies
    /// as silence (after hangover), letting the caller skip the network send.
    pub fn set_vad_enabled(&self, enabled: bool) {
        self.enable_vad.store(enabled, Ordering::SeqCst);
        if !enabled {
            // Drop hangover state so re-enabling starts fresh.
            if let Ok(mut vad) = self.vad.lock() {
                vad.reset();
            }
        }
    }

    /// Set the VAD detection threshold in dB (e.g., -40.0 = sensitive, -20.0 = loud-only).
    pub fn set_vad_threshold_db(&self, threshold_db: f32) {
        if let Ok(mut vad) = self.vad.lock() {
            vad.set_threshold_db(threshold_db);
        }
    }

    /// Enable or disable WebRTC AEC at runtime
    #[cfg(feature = "webrtc-audio")]
    pub fn set_webrtc_aec_enabled(&self, enabled: bool) {
        self.enable_webrtc_aec.store(enabled, Ordering::SeqCst);
        if let Some(proc) = &self.webrtc_processor {
            let mut p = proc.lock().unwrap();
            p.reconfigure(
                enabled,
                self.enable_webrtc_ns.load(Ordering::SeqCst),
                self.enable_webrtc_agc.load(Ordering::SeqCst),
            );
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
                self.enable_webrtc_agc.load(Ordering::SeqCst),
            );
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
                enabled,
            );
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
        self.codec
            .set_dred_duration(duration)
            .map_err(AudioPipelineError::Opus)
    }

    /// Encode and encrypt PCM audio for transmission.
    ///
    /// Pipeline: VAD gate -> Opus -> Zero-pad -> XChaCha20-Poly1305 -> Packet
    ///
    /// Input: 960 samples of i16 PCM (20ms at 48kHz)
    /// Output:
    /// * `Ok(Some(bytes))` — serialized FastAudioPacket ready for QUIC datagram
    /// * `Ok(None)` — VAD is enabled and the frame is silence; caller should skip the send
    pub fn process(&self, pcm: &[i16]) -> Result<Option<Bytes>, AudioPipelineError> {
        // 0. VAD gate — only when enabled. Sequence is bumped only on emitted frames so
        //    receivers don't see gaps for skipped silence.
        if self.enable_vad.load(Ordering::Relaxed) {
            let voice = self.vad.lock().map(|mut v| v.process(pcm)).unwrap_or(true);
            if !voice {
                return Ok(None);
            }
        }

        // 1. Encode to Opus
        let opus_data = self.codec.encode(pcm).map_err(AudioPipelineError::Opus)?;

        // 2. Stamp the header, then encrypt with it as associated data
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst);
        let epoch_hint = self.epoch_hint.load(Ordering::SeqCst);
        let nonce = DaveCrypto::random_nonce();
        let aad = FastAudioPacket::aad_for(self.session_id, epoch_hint, sequence);
        let ciphertext = {
            let crypto = self.crypto.read().unwrap();
            crypto
                .encrypt_with_aad(&opus_data, &nonce, &aad)
                .map_err(AudioPipelineError::Crypto)?
        };

        // 3. Build packet

        let packet = FastAudioPacket {
            session_id: self.session_id,
            epoch_hint,
            sequence,
            nonce,
            payload: Bytes::from(ciphertext),
        };

        // 4. Serialize
        Ok(Some(packet.to_bytes()))
    }

    /// Encode and encrypt f32 PCM audio.
    ///
    /// `reference`: Optional playback audio for AEC (only needed if WebRTC AEC is enabled)
    ///
    /// Returns `Ok(None)` if VAD is enabled and the (post-NS) frame is silence.
    /// VAD runs on the noise-suppressed signal so RNNoise / WebRTC NS can attenuate
    /// background hiss before the gate decides voice vs. silence.
    pub fn process_float_with_reference(
        &self,
        pcm: &[f32],
        _reference: Option<&[f32]>,
    ) -> Result<Option<Bytes>, AudioPipelineError> {
        let mut processed = pcm.to_vec();

        // 1. WebRTC processing (AEC/NS/AGC)
        #[cfg(feature = "webrtc-audio")]
        if let Some(proc) = &self.webrtc_processor {
            let mut p = proc.lock().unwrap();
            processed = p.process(&processed, _reference);
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

        // 3. VAD gate — runs on the post-NS signal so noise reduction can attenuate
        //    background hiss before the threshold check.
        if self.enable_vad.load(Ordering::Relaxed) {
            let voice = self
                .vad
                .lock()
                .map(|mut v| v.process_float(&processed))
                .unwrap_or(true);
            if !voice {
                return Ok(None);
            }
        }

        // 4. Encode to Opus using native float API
        let opus_data = self
            .codec
            .encode_float(&processed)
            .map_err(AudioPipelineError::Opus)?;

        // 5. Stamp the header, then encrypt with it as associated data
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst);
        let epoch_hint = self.epoch_hint.load(Ordering::SeqCst);
        let nonce = DaveCrypto::random_nonce();
        let aad = FastAudioPacket::aad_for(self.session_id, epoch_hint, sequence);
        let ciphertext = {
            let crypto = self.crypto.read().unwrap();
            crypto
                .encrypt_with_aad(&opus_data, &nonce, &aad)
                .map_err(AudioPipelineError::Crypto)?
        };

        // 6. Build packet

        let packet = FastAudioPacket {
            session_id: self.session_id,
            epoch_hint,
            sequence,
            nonce,
            payload: Bytes::from(ciphertext),
        };

        Ok(Some(packet.to_bytes()))
    }

    /// Get current sequence number
    pub fn sequence(&self) -> u16 {
        self.sequence.load(Ordering::SeqCst)
    }
}

/// Samples per 20 ms Opus frame at 48 kHz.
const SAMPLES_PER_FRAME: u64 = 960;

/// How many frames behind the newest authenticated one a frame may arrive and
/// still be considered reordering rather than a replay (about 5 s).
const REORDER_WINDOW: u64 = 256;

/// Place a 16-bit wire sequence on the extended (unwrapped) timeline, RTP
/// style: the signed 16-bit distance from the highest extended sequence seen
/// so far is added to it, so 65535 -> 0 keeps counting up and a slightly late
/// frame from before a wrap maps to a slightly smaller value.
///
/// The first frame is biased by one full cycle (65536) so a frame reordered
/// just ahead of it can never underflow.
fn extend_sequence(highest: Option<u64>, sequence: u16) -> u64 {
    match highest {
        None => 0x1_0000 + u64::from(sequence),
        Some(h) => {
            let offset = i64::from(sequence.wrapping_sub(h as u16) as i16);
            h.saturating_add_signed(offset)
        }
    }
}

/// Aggregated jitter buffer statistics across all active senders.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AudioReceiverStats {
    pub packets_received: u64,
    pub packets_played: u64,
    pub packets_lost: u64,
    pub plc_count: u64,
    pub packets_dropped_late: u64,
    pub packets_dropped_duplicate: u64,
    pub current_buffer_size: usize,
    pub estimated_jitter_ms: f32,
    pub estimated_loss_rate: f32,
}

/// Audio receiver for decrypting and decoding incoming audio
pub struct AudioReceiver {
    /// Per-sender state
    senders: RwLock<HashMap<u32, SenderState>>,
    /// Default jitter buffer config
    jitter_config: JitterBufferConfig,
    /// Senders already reported as using the older audio format, so the
    /// notice is logged once per sender rather than once per frame.
    legacy_reported: Mutex<HashSet<u32>>,
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
    /// Highest extended (unwrapped) sequence number seen on an authenticated
    /// frame; `None` until the first one, so a listener who joins a stream in
    /// progress accepts wherever it is. See `extend_sequence`.
    highest_sequence: Option<u64>,
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
            legacy_reported: Mutex::new(HashSet::new()),
        }
    }

    /// Create with custom jitter buffer config
    pub fn with_config(jitter_config: JitterBufferConfig) -> Self {
        Self {
            senders: RwLock::new(HashMap::new()),
            jitter_config,
            legacy_reported: Mutex::new(HashSet::new()),
        }
    }

    /// Set jitter buffer target latency for all senders
    pub fn set_jitter_buffer_ms(&self, latency_ms: u32) {
        let mut senders = self.senders.write().unwrap();
        for state in senders.values_mut() {
            state.jitter.set_target_latency(latency_ms);
        }
    }

    /// Return aggregated jitter buffer statistics across all registered senders.
    pub fn get_stats(&self) -> AudioReceiverStats {
        let senders = self.senders.read().unwrap();
        let mut agg = AudioReceiverStats::default();
        let mut jitter_sum = 0.0f32;
        let mut jitter_senders = 0u32;

        for state in senders.values() {
            let s = state.jitter.stats();
            agg.packets_received = agg.packets_received.saturating_add(s.packets_received);
            agg.packets_played = agg.packets_played.saturating_add(s.packets_played);
            agg.packets_lost = agg.packets_lost.saturating_add(s.packets_lost);
            agg.plc_count = agg.plc_count.saturating_add(s.plc_count);
            agg.packets_dropped_late = agg
                .packets_dropped_late
                .saturating_add(s.packets_dropped_late);
            agg.packets_dropped_duplicate = agg
                .packets_dropped_duplicate
                .saturating_add(s.packets_dropped_duplicate);
            agg.current_buffer_size = agg
                .current_buffer_size
                .saturating_add(s.current_buffer_size);
            if s.packets_received > 0 {
                jitter_sum += s.estimated_jitter_ms;
                jitter_senders += 1;
            }
        }

        if jitter_senders > 0 {
            agg.estimated_jitter_ms = jitter_sum / jitter_senders as f32;
        }
        let total_expected = agg.packets_received.saturating_add(agg.packets_lost);
        if total_expected > 0 {
            agg.estimated_loss_rate = agg.packets_lost as f32 / total_expected as f32;
        }
        agg
    }

    /// Register a sender with their decryption key
    pub fn add_sender(
        &self,
        session_id: u32,
        key: &[u8; 32],
        epoch_hint: u16,
    ) -> Result<(), AudioPipelineError> {
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
            highest_sequence: None,
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
                if let Some(&oldest) = state
                    .key_store
                    .keys()
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

    /// Note a version 1 audio frame (datagram type `0x01` / stream message
    /// `0x20`, header not authenticated). It is never decrypted or played and
    /// moves no receiver state; the first one from each sender logs that the
    /// peer needs to update. `data` is the packet after the type tag.
    pub fn note_legacy_packet(&self, data: &[u8]) {
        let Some(id) = data
            .get(..4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        else {
            return;
        };
        if self.legacy_reported.lock().unwrap().insert(id) {
            tracing::warn!(
                "audio from session {id}: peer uses an older audio format; update both clients"
            );
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
        let state = senders
            .get_mut(&packet.session_id)
            .ok_or(AudioPipelineError::UnknownSender(packet.session_id))?;

        // 3. Replay protection: place the 16-bit sequence on the extended
        //    (unwrapped) timeline and refuse a frame further behind the newest
        //    authenticated one than the reorder window. This only reads the
        //    state; it moves once the frame has authenticated (below), so a
        //    forged frame can't push it ahead of the real stream.
        let extended = extend_sequence(state.highest_sequence, packet.sequence);
        if let Some(highest) = state.highest_sequence {
            if extended.saturating_add(REORDER_WINDOW) < highest {
                return Err(AudioPipelineError::PacketParse("Replayed packet".into()));
            }
        }

        // 4. Try decryption with epoch_hint key first, then fallback
        let aad = packet.aad();
        let opus_data = if let Some(crypto) = state.key_store.get(&packet.epoch_hint) {
            crypto
                .decrypt_with_aad(&packet.payload, &packet.nonce, &aad)
                .map_err(AudioPipelineError::Crypto)?
        } else {
            // Fallback: try other epochs (for packets in-flight during transition)
            let mut decrypted = None;
            for crypto in state.key_store.values() {
                if let Ok(data) = crypto.decrypt_with_aad(&packet.payload, &packet.nonce, &aad) {
                    decrypted = Some(data);
                    break;
                }
            }
            decrypted.ok_or(AudioPipelineError::Crypto(
                crate::crypto::CryptoError::DecryptionFailed,
            ))?
        };

        // Authenticated: the window may move forward.
        if state.highest_sequence.is_none_or(|h| extended > h) {
            state.highest_sequence = Some(extended);
        }

        // 5. Insert into jitter buffer. The RTP-style timestamp is the frame
        //    count in samples and wraps at 32 bits, as RTP timestamps do.
        state.jitter.push(
            extended,
            extended.wrapping_mul(SAMPLES_PER_FRAME) as u32,
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
                PopResult::PacketWithGap {
                    data: opus_data,
                    lost,
                } => {
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
            let (gain, muted) = sender_cfg.get(session_id).copied().unwrap_or((1.0, false));

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
        let pcm = mixed
            .iter()
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
        let packet = sender
            .process(&pcm)
            .expect("Process failed")
            .expect("VAD off by default; expected a packet");

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
        receiver
            .add_sender(session_id, &key, 0)
            .expect("Add sender");

        // Generate test tone
        let pcm: Vec<i16> = (0..960).map(|i| ((i % 100) * 100) as i16).collect();

        // Send
        let packet = sender
            .process(&pcm)
            .expect("Process failed")
            .expect("VAD off; expected a packet");

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
        let pkt1 = sender1.process(&pcm).expect("Send 1").expect("VAD off");
        let pkt2 = sender2.process(&pcm).expect("Send 2").expect("VAD off");

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
        let packet = sender.process(&pcm).expect("Process").expect("VAD off");

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

        sender.process(&pcm).expect("Process 1").expect("VAD off");
        assert_eq!(sender.sequence(), 1);

        sender.process(&pcm).expect("Process 2").expect("VAD off");
        assert_eq!(sender.sequence(), 2);
    }

    #[test]
    fn test_vad_skips_silence_and_holds_sequence() {
        let key = DaveCrypto::random_key();
        let sender = AudioSender::new(7, &key).expect("Create sender");

        // Enable VAD with a moderately strict threshold and short hangover for a
        // deterministic test.
        sender.set_vad_enabled(true);
        sender.set_vad_threshold_db(-30.0);

        // Pure silence -> VAD gates the frame, no packet emitted.
        let silence = vec![0i16; 960];
        let res = sender.process(&silence).expect("Process should not error");
        assert!(res.is_none(), "VAD should silence the frame");
        // Sequence must NOT advance for skipped frames so receivers don't see gaps.
        assert_eq!(sender.sequence(), 0);

        // Loud signal -> VAD lets it through, sequence advances.
        let loud: Vec<i16> = (0..960).map(|_| 16000i16).collect();
        let res = sender.process(&loud).expect("Process should not error");
        assert!(res.is_some(), "VAD should pass loud signal");
        assert_eq!(sender.sequence(), 1);

        // Disabling VAD should let silence through again immediately.
        sender.set_vad_enabled(false);
        let res = sender.process(&silence).expect("Process should not error");
        assert!(res.is_some(), "VAD disabled => packet must emit");
        assert_eq!(sender.sequence(), 2);
    }

    // --- sequence wraparound -------------------------------------------------

    /// A receiver that plays out whatever it holds immediately (no latency
    /// target) and keeps frames long enough for a slow test machine.
    fn eager_receiver(session_id: u32, key: &[u8; 32]) -> AudioReceiver {
        let receiver = AudioReceiver::with_config(JitterBufferConfig {
            target_latency_ms: 0,
            max_packets: 100,
            max_age_ms: 60_000,
            frame_duration_ms: 20,
            ..Default::default()
        });
        receiver.add_sender(session_id, key, 0).unwrap();
        receiver
    }

    /// Authentic frames at any sequence number: the key holder opens one
    /// genuine frame and seals its audio again under each header (the header
    /// is authenticated, so it can't simply be rewritten).
    fn stamper(session_id: u32, key: &[u8; 32]) -> impl Fn(u16) -> Bytes {
        let sender = AudioSender::new(session_id, key).unwrap();
        let template =
            FastAudioPacket::parse(sender.process(&[1000i16; 960]).unwrap().unwrap()).unwrap();
        let crypto = DaveCrypto::new(key);
        let opus = crypto
            .decrypt_with_aad(&template.payload, &template.nonce, &template.aad())
            .unwrap();
        move |seq| {
            let nonce = DaveCrypto::random_nonce();
            let aad = FastAudioPacket::aad_for(session_id, template.epoch_hint, seq);
            FastAudioPacket {
                session_id,
                epoch_hint: template.epoch_hint,
                sequence: seq,
                nonce,
                payload: Bytes::from(crypto.encrypt_with_aad(&opus, &nonce, &aad).unwrap()),
            }
            .to_bytes()
        }
    }

    /// Pop everything the jitter buffer will give up, then return its stats.
    fn drain(receiver: &AudioReceiver, session_id: u32) -> crate::jitter_buffer::JitterBufferStats {
        std::thread::sleep(std::time::Duration::from_millis(2));
        let mut senders = receiver.senders.write().unwrap();
        let state = senders.get_mut(&session_id).unwrap();
        while state.jitter.pop().has_data() {}
        state.jitter.stats().clone()
    }

    #[test]
    fn extend_sequence_unwraps_forward_and_backward() {
        assert_eq!(extend_sequence(None, 7), 0x1_0000 + 7);
        let h = 0x1_0000 + 65_535;
        assert_eq!(extend_sequence(Some(h), 0), 0x2_0000);
        assert_eq!(extend_sequence(Some(h), 5), 0x2_0005);
        // Just after the wrap, a frame from before it maps below.
        assert_eq!(extend_sequence(Some(0x2_0003), 65_534), 0x1_FFFE);
        // Never underflows, even for a frame reordered ahead of the first.
        assert_eq!(extend_sequence(Some(0x1_0000), 65_535), 0xFFFF);
    }

    #[test]
    fn continuous_stream_plays_across_the_wrap() {
        let key = DaveCrypto::random_key();
        let frame = stamper(1, &key);
        let receiver = eager_receiver(1, &key);
        for seq in (65_530u16..=65_535).chain(0..6) {
            receiver.on_packet(frame(seq)).unwrap();
        }
        let stats = drain(&receiver, 1);
        assert_eq!(stats.packets_played, 12);
        assert_eq!(stats.packets_lost, 0);
        assert_eq!(stats.packets_dropped_late, 0);
    }

    #[test]
    fn reordering_straddling_the_wrap_is_put_right() {
        let key = DaveCrypto::random_key();
        let frame = stamper(1, &key);
        let receiver = eager_receiver(1, &key);
        for seq in [65_533u16, 65_534, 0, 65_535, 1, 2] {
            receiver.on_packet(frame(seq)).unwrap();
        }
        let stats = drain(&receiver, 1);
        assert_eq!(stats.packets_played, 6);
        assert_eq!(stats.packets_lost, 0);
        assert_eq!(stats.packets_dropped_late, 0);
    }

    #[test]
    fn loss_across_the_wrap_is_counted_not_fatal() {
        let key = DaveCrypto::random_key();
        let frame = stamper(1, &key);
        let receiver = eager_receiver(1, &key);
        for seq in [65_533u16, 65_534, 1, 2] {
            receiver.on_packet(frame(seq)).unwrap();
        }
        let stats = drain(&receiver, 1);
        assert_eq!(stats.packets_played, 4);
        assert_eq!(stats.packets_lost, 2); // 65535 and 0
    }

    #[test]
    fn duplicate_across_the_wrap_is_dropped() {
        let key = DaveCrypto::random_key();
        let frame = stamper(1, &key);
        let receiver = eager_receiver(1, &key);
        for seq in [65_535u16, 0, 0, 1] {
            receiver.on_packet(frame(seq)).unwrap();
        }
        let stats = drain(&receiver, 1);
        assert_eq!(stats.packets_played, 3);
        assert_eq!(stats.packets_dropped_duplicate, 1);
    }

    #[test]
    fn late_frame_from_before_the_wrap_is_not_played_as_new() {
        let key = DaveCrypto::random_key();
        let frame = stamper(1, &key);
        let receiver = eager_receiver(1, &key);
        for seq in (65_530u16..=65_535).chain(0..10) {
            receiver.on_packet(frame(seq)).unwrap();
        }
        assert_eq!(drain(&receiver, 1).packets_played, 16);

        // Slightly late: inside the reorder window, but already played.
        receiver.on_packet(frame(65_533)).unwrap();
        let stats = drain(&receiver, 1);
        assert_eq!(stats.packets_played, 16);
        assert_eq!(stats.packets_dropped_late, 1);

        // Far too late, however the 16-bit values compare: refused outright.
        for seq in [60_000u16, 40_000] {
            assert!(receiver.on_packet(frame(seq)).is_err(), "{seq} accepted");
        }
        assert_eq!(drain(&receiver, 1).packets_played, 16);

        // The stream carries on.
        receiver.on_packet(frame(10)).unwrap();
        assert_eq!(drain(&receiver, 1).packets_played, 17);
    }

    #[test]
    fn listener_joining_just_before_the_wrap_hears_the_sender() {
        let key = DaveCrypto::random_key();
        let frame = stamper(1, &key);
        let receiver = eager_receiver(1, &key);
        for seq in [65_535u16, 0, 1, 2] {
            receiver.on_packet(frame(seq)).unwrap();
        }
        let stats = drain(&receiver, 1);
        assert_eq!(stats.packets_played, 4);
        assert_eq!(stats.packets_lost, 0);
    }

    #[test]
    fn unauthenticated_frame_near_the_wrap_moves_nothing() {
        let key = DaveCrypto::random_key();
        let genuine = stamper(1, &key);
        let wrong_key = DaveCrypto::random_key();
        let forged = stamper(1, &wrong_key);
        let receiver = eager_receiver(1, &key);
        receiver.on_packet(genuine(65_534)).unwrap();
        receiver.on_packet(genuine(65_535)).unwrap();
        // A forged frame "after" the wrap must not carry the window with it.
        assert!(receiver.on_packet(forged(30_000)).is_err());
        assert!(receiver.on_packet(forged(3)).is_err());
        receiver.on_packet(genuine(0)).unwrap();
        receiver.on_packet(genuine(1)).unwrap();
        assert_eq!(drain(&receiver, 1).packets_played, 4);
    }

    #[test]
    fn sender_that_restarts_is_heard_once_it_is_added_again() {
        // A sender's counter restarts at 0 only with a new AudioSender, which
        // clients create per connection under a fresh session id; receivers
        // learn of a returning sender through add_sender, which starts clean.
        let key = DaveCrypto::random_key();
        let frame = stamper(1, &key);
        let receiver = eager_receiver(1, &key);
        for seq in 40_000u16..40_005 {
            receiver.on_packet(frame(seq)).unwrap();
        }
        assert_eq!(drain(&receiver, 1).packets_played, 5);

        // Key rotation keeps the stream's state: the sender's counter carries on.
        assert!(receiver.update_sender_key(1, &key, 1));
        receiver.on_packet(frame(40_005)).unwrap();

        // Leaving and rejoining (remove + add) forgets the old position.
        receiver.remove_sender(1);
        receiver.add_sender(1, &key, 0).unwrap();
        receiver.on_packet(frame(0)).unwrap();
        receiver.on_packet(frame(1)).unwrap();
        assert_eq!(drain(&receiver, 1).packets_played, 2);
    }

    // --- header authentication -----------------------------------------------

    /// The receiver's replay window for a sender.
    fn highest(receiver: &AudioReceiver, id: u32) -> Option<u64> {
        receiver.senders.read().unwrap()[&id].highest_sequence
    }

    fn rewrite_header(packet: &Bytes, f: impl FnOnce(&mut [u8])) -> Bytes {
        let mut p = packet.to_vec();
        f(&mut p);
        Bytes::from(p)
    }

    #[test]
    fn tampered_header_fields_fail_and_leave_state_alone() {
        let key = DaveCrypto::random_key();
        let sender = AudioSender::new(1, &key).unwrap();
        let receiver = eager_receiver(1, &key);
        let first = sender.process(&[1000i16; 960]).unwrap().unwrap();
        let second = sender.process(&[1000i16; 960]).unwrap().unwrap();
        receiver.on_packet(first).unwrap();
        let before = highest(&receiver, 1);
        assert!(before.is_some());

        // session_id: the receiver looks the sender up by it, so register the
        // id the tampered copy names; the frame must still not open.
        receiver.add_sender(2, &key, 0).unwrap();
        let cases: [(&str, Bytes); 5] = [
            ("session_id", rewrite_header(&second, |p| p[3] = 2)),
            ("epoch_hint", rewrite_header(&second, |p| p[5] ^= 1)),
            ("sequence low", rewrite_header(&second, |p| p[7] ^= 1)),
            (
                "sequence far ahead",
                rewrite_header(&second, |p| p[6] = 0x7F),
            ),
            (
                "sequence zeroed",
                rewrite_header(&second, |p| p[6..8].fill(0)),
            ),
        ];
        for (what, frame) in cases {
            let id = u32::from_be_bytes(frame[..4].try_into().unwrap());
            let err = receiver.on_packet(frame).unwrap_err();
            assert!(
                matches!(err, AudioPipelineError::Crypto(_)),
                "{what}: expected an authentication failure, got {err:?}"
            );
            assert_eq!(highest(&receiver, 1), before, "{what} moved the window");
            if id != 1 {
                assert_eq!(highest(&receiver, id), None, "{what} moved a window");
            }
        }
        // Nothing but the genuine first frame is waiting to play.
        assert_eq!(drain(&receiver, 1).packets_played, 1);

        // The untouched second frame is still accepted afterwards.
        receiver.on_packet(second).unwrap();
        assert_eq!(drain(&receiver, 1).packets_played, 2);
    }

    #[test]
    fn replay_under_a_new_sequence_number_is_refused() {
        let key = DaveCrypto::random_key();
        let sender = AudioSender::new(1, &key).unwrap();
        let receiver = eager_receiver(1, &key);
        let captured = sender.process(&[1000i16; 960]).unwrap().unwrap();
        receiver.on_packet(captured.clone()).unwrap();
        assert_eq!(drain(&receiver, 1).packets_played, 1);
        let before = highest(&receiver, 1);

        for seq in [1u16, 2, 5_000, 65_535] {
            let restamped =
                rewrite_header(&captured, |p| p[6..8].copy_from_slice(&seq.to_be_bytes()));
            assert!(receiver.on_packet(restamped).is_err(), "seq {seq} played");
        }
        assert_eq!(highest(&receiver, 1), before);
        assert_eq!(drain(&receiver, 1).packets_played, 1);
    }

    #[test]
    fn identical_replay_is_not_played_twice() {
        let key = DaveCrypto::random_key();
        let sender = AudioSender::new(1, &key).unwrap();
        let receiver = eager_receiver(1, &key);
        let frames: Vec<_> = (0..4)
            .map(|_| sender.process(&[1000i16; 960]).unwrap().unwrap())
            .collect();
        for f in &frames {
            receiver.on_packet(f.clone()).unwrap();
        }
        assert_eq!(drain(&receiver, 1).packets_played, 4);
        // Replayed byte for byte, inside the reorder window: the jitter
        // buffer sees it was already played.
        receiver.on_packet(frames[1].clone()).unwrap();
        let stats = drain(&receiver, 1);
        assert_eq!(stats.packets_played, 4);
        assert_eq!(stats.packets_dropped_late, 1);
    }

    #[test]
    fn version_1_frame_is_refused_without_moving_state() {
        let key = DaveCrypto::random_key();
        let receiver = eager_receiver(1, &key);
        let sender = AudioSender::new(1, &key).unwrap();
        receiver
            .on_packet(sender.process(&[1000i16; 960]).unwrap().unwrap())
            .unwrap();
        let before = highest(&receiver, 1);

        // What an older client sends: the same header, sealed without AAD.
        let crypto = DaveCrypto::new(&key);
        let nonce = DaveCrypto::random_nonce();
        let v1 = FastAudioPacket {
            session_id: 1,
            epoch_hint: 0,
            sequence: 9,
            nonce,
            payload: Bytes::from(crypto.encrypt(b"opus", &nonce).unwrap()),
        }
        .to_bytes();
        assert!(receiver.on_packet(v1.clone()).is_err());
        // Reporting it never panics, on any input, and is logged once per sender.
        receiver.note_legacy_packet(&v1);
        receiver.note_legacy_packet(&v1);
        receiver.note_legacy_packet(&[]);
        receiver.note_legacy_packet(&[1, 2]);
        assert_eq!(receiver.legacy_reported.lock().unwrap().len(), 1);
        assert_eq!(highest(&receiver, 1), before);
        assert_eq!(drain(&receiver, 1).packets_played, 1);
    }
}
