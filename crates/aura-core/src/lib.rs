//! Aura Core - Cross-Platform VoIP Client Engine
//!
//! This crate provides the shared Rust logic for the Aura voice/text client,
//! exposed to Swift (macOS) and C# (Windows) via UniFFI bindings.

#![allow(unpredictable_function_pointer_comparisons)]

use std::sync::{Arc, Mutex, atomic::{AtomicU64, AtomicU16, AtomicBool, Ordering}};
use aura_protocol::Position;
use crate::audio_pipeline::AudioSender;
use crate::crypto::DaveCrypto;

pub mod opus;
pub mod opus16;
pub mod jitter_buffer;
pub mod crypto;
pub mod audio_pipeline;
pub mod mls;
pub mod text_crypto;
pub mod voice_session;
#[cfg(feature = "native-audio")]
pub mod audio_io;
pub mod vad;
pub mod noise_suppression;
#[cfg(feature = "webrtc-audio")]
pub mod webrtc_processor;
pub mod tts;
pub mod uniffi_bindings;
#[cfg(feature = "native-audio")]
use crate::uniffi_bindings::AudioHardware;

uniffi::setup_scaffolding!("aura_core");

// =============================================================================
// SECURITY: XChaCha20-Poly1305 Encryption (DAVE Protocol)
// =============================================================================
//
// This is where the E2EE encryption loop will be implemented.
//
// CRITICAL REQUIREMENTS:
//
// 1. CIPHER: XChaCha20-Poly1305 with 192-bit (24-byte) random nonces
//    - Extended nonce avoids birthday bound issues with high packet rates
//    - Use `chacha20poly1305` crate with `XChaCha20Poly1305` type
//
// 2. ZERO-PADDING COMMITMENT (TOB-DISCE2EC-5 Mitigation):
//    Before encryption, prepend 16 bytes of 0x00 to plaintext:
//
//    ```rust
//    fn encrypt_audio_frame(key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
//        // 1. Zero-Padding Commitment (prevents Partitioning Oracle attacks)
//        let mut padded = vec![0u8; 16];  // 16 bytes of 0x00
//        padded.extend_from_slice(plaintext);
//        
//        // 2. Generate random 192-bit nonce
//        let nonce: [u8; 24] = rand::thread_rng().gen();
//        
//        // 3. Encrypt with XChaCha20-Poly1305
//        let cipher = XChaCha20Poly1305::new(key.into());
//        let ciphertext = cipher.encrypt(&nonce.into(), padded.as_ref())
//            .expect("encryption failure");
//        
//        // 4. Return: nonce || ciphertext
//        [nonce.as_ref(), &ciphertext].concat()
//    }
//    ```
//
// 3. DECRYPTION VALIDATION:
//    After decryption, verify the first 16 bytes are all 0x00.
//    If not, REJECT the packet (potential attack or key mismatch).
//
// 4. KEY DERIVATION:
//    Keys are derived from MLS epoch secrets via HKDF-SHA256.
//    Voice and Text use SEPARATE MLS groups with independent epochs.
//
// =============================================================================

/// Errors that can occur in the Aura client
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AuraError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    
    #[error("Authentication failed")]
    AuthenticationFailed,
    
    #[error("MLS protocol error: {0}")]
    MlsError(String),
    
    #[error("Channel not found")]
    ChannelNotFound,
    
    #[error("Not connected to server")]
    NotConnected,
    
    #[error("Encryption error: {0}")]
    EncryptionError(String),
}

/// Aura Client - Main Entry Point
/// 
/// This is the primary interface for connecting to an Aura server.
/// It handles QUIC connections, MLS group management, and audio/text streaming.
#[derive(uniffi::Object)]
pub struct AuraClient {
    url: String,
    identity_file: String,
    position: Mutex<Position>,
    delegate: Mutex<Option<Box<dyn AuraDelegate>>>,
    
    // Connection state
    connected: AtomicBool,
    
    // Dual MLS Group Epoch Tracking (Voice and Text are separate)
    voice_epoch: AtomicU64,
    text_epoch: AtomicU64,
    
    // Audio packet sequence number (wraps at 65536)
    sequence: AtomicU16,
    
    // Real audio send pipeline: Opus encoder + DAVE encryptor.
    // Initialized on first use so AuraClient construction is cheap.
    // Wrapped in Option so we can detect first-init, Mutex for thread safety.
    audio_sender: Mutex<Option<AudioSender>>,
    
    // The numeric session ID used in audio packet headers.
    // Derived from a hash of the identity file path at construction time
    // and overwritten by the server-assigned session ID on connect.
    session_id: AtomicU16,
}

#[uniffi::export]
impl AuraClient {
    /// Create a new AuraClient
    #[uniffi::constructor]
    pub fn new(url: String, identity_file: String) -> Self {
        // Derive a stable local session ID from the identity file path until the
        // server assigns us a real one on authentication.
        let local_id = identity_file
            .bytes()
            .fold(0u16, |acc, b| acc.wrapping_add(b as u16)
                .wrapping_mul(31));
        
        Self {
            url,
            identity_file,
            position: Mutex::new(Position { x: 0.0, y: 0.0, z: 0.0 }),
            delegate: Mutex::new(None),
            connected: AtomicBool::new(false),
            voice_epoch: AtomicU64::new(0),
            text_epoch: AtomicU64::new(0),
            sequence: AtomicU16::new(0),
            audio_sender: Mutex::new(None),
            session_id: AtomicU16::new(local_id),
        }
    }
    
    /// Connect to the Aura server
    /// 
    /// This initiates:
    /// 1. QUIC connection with TLS 1.3
    /// 2. Authentication with the provided token
    /// 3. MLS group join (both Voice and Text groups)
    pub fn connect(&self, url: String, token: String) -> Result<(), AuraError> {
        // TODO: Implement QUIC connection via quinn
        // TODO: Implement MLS handshake via openmls
        
        println!("Connecting to {} with token {}...", url, token.len());
        
        // Simulate connection
        self.connected.store(true, Ordering::SeqCst);
        
        // Notify delegate
        if let Some(ref delegate) = *self.delegate.lock().unwrap() {
            delegate.on_connected();
        }
        
        Ok(())
    }
    
    /// Disconnect from the server
    pub fn disconnect(&self) {
        if self.connected.load(Ordering::SeqCst) {
            self.connected.store(false, Ordering::SeqCst);
            
            if let Some(ref delegate) = *self.delegate.lock().unwrap() {
                delegate.on_disconnected("User requested disconnect".into());
            }
        }
    }
    
    /// Join a voice/text channel
    /// 
    /// This creates dual MLS groups:
    /// - Voice group: Low epoch churn (join/leave only)
    /// - Text group: High epoch churn (per message batch)
    pub fn join_channel(&self, channel_id: String) {
        println!("Joining channel: {} (Voice + Text MLS groups)", channel_id);
        // TODO: Send JoinChannelGroups message to server
    }
    
    /// Leave the current channel
    pub fn leave_channel(&self) {
        println!("Leaving current channel");
        // TODO: Send leave message and reset MLS state
    }

    /// Set 3D spatial position for positional audio
    pub fn set_position(&self, x: f32, y: f32, z: f32) {
        let mut pos = self.position.lock().unwrap();
        pos.x = x;
        pos.y = y;
        pos.z = z;
    }
    
    /// Update voice epoch (called when voice MLS group advances)
    pub fn set_voice_epoch(&self, epoch: u64) {
        self.voice_epoch.store(epoch, Ordering::SeqCst);
    }
    
    /// Update text epoch (called when text MLS group advances)
    pub fn set_text_epoch(&self, epoch: u64) {
        self.text_epoch.store(epoch, Ordering::SeqCst);
    }

    /// Send an audio frame with proper epoch hint and XChaCha20 nonce.
    /// 
    /// Pipeline:
    /// 1. Lazy-init AudioSender (Opus encoder + DAVE encryptor) on first call
    /// 2. AudioSender.process() → Opus encode → zero-pad → XChaCha20-Poly1305 encrypt
    /// 3. Build FastAudioPacket with real random nonce and epoch_hint
    /// 4. TODO: Send via QUIC datagram (transport not yet wired)
    pub fn send_audio_frame(&self, pcm_data: Vec<f32>) {
        if !self.connected.load(Ordering::Relaxed) {
            return;
        }
        
        // Convert f32 PCM → i16 for the encoder
        let pcm_i16: Vec<i16> = pcm_data.iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        
        // Lazy-init the AudioSender (Opus + DAVE) on first call.
        // We generate a random key here — in the real E2EE flow this will be
        // replaced by the MLS-derived epoch key via set_voice_epoch.
        let mut guard = self.audio_sender.lock().unwrap();
        if guard.is_none() {
            let key = DaveCrypto::random_key();
            match AudioSender::new(self.session_id.load(Ordering::Relaxed) as u32, &key) {
                Ok(sender) => *guard = Some(sender),
                Err(e) => {
                    tracing::error!("Failed to initialize AudioSender: {}", e);
                    return;
                }
            }
        }
        
        let sender = guard.as_ref().unwrap();
        
        // Sync epoch hint so the packet carries the current voice MLS epoch
        sender.set_epoch(self.voice_epoch.load(Ordering::Relaxed));
        
        match sender.process(&pcm_i16) {
            Ok(_packet_bytes) => {
                // TODO: self.quic_connection.send_datagram(packet_bytes)
                // packet_bytes is a fully-formed FastAudioPacket with:
                //   - Real Opus-encoded audio (not zeros)
                //   - Real 192-bit random XChaCha20 nonce
                //   - DAVE zero-padding commitment (16 bytes of 0x00 prepended)
                //   - Correct epoch_hint from voice MLS group
                let _ = _packet_bytes; // suppress unused warning until transport is wired
            }
            Err(e) => tracing::warn!("Audio encode/encrypt failed: {}", e),
        }
    }
    
    /// Send a text message to the current channel
    pub fn send_text_message(&self, message: String) {
        if !self.connected.load(Ordering::Relaxed) {
            return;
        }
        
        println!("Sending text message: {} chars", message.len());
        // TODO: Encrypt with text group keys and send via reliable QUIC stream
    }

    /// Set the delegate for receiving async events
    pub fn set_delegate(&self, delegate: Box<dyn AuraDelegate>) {
        *self.delegate.lock().unwrap() = Some(delegate);
    }

    /// Set own profile comment/bio
    pub fn set_comment(&self, text: String) {
        println!("Setting own comment: {} chars", text.len());
        // TODO: Sign profile and send to server
    }

    /// Get a TTS formatter for sanitizing text
    pub fn get_tts_formatter(&self) -> Arc<crate::tts::TtsFormatter> {
        Arc::new(crate::tts::TtsFormatter::new())
    }
}

// =============================================================================
// Simple C-FFI for C# Audio (Fallback when UniFFI generator is unavailable)
// =============================================================================

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_new() -> *mut AudioHardware {
    if let Ok(hw) = AudioHardware::new() {
        Box::into_raw(Box::new(hw))
    } else {
        std::ptr::null_mut()
    }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_free(hw: *mut AudioHardware) {
    if !hw.is_null() {
        unsafe { drop(Box::from_raw(hw)) };
    }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_start_capture(hw: *mut AudioHardware) -> i32 {
    let hw = unsafe { &*hw };
    if hw.start_capture().is_ok() { 0 } else { -1 }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_stop_capture(hw: *mut AudioHardware) -> i32 {
    let hw = unsafe { &*hw };
    if hw.stop_capture().is_ok() { 0 } else { -1 }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_read_capture(hw: *mut AudioHardware, buf: *mut i16, len: usize) -> i32 {
    let hw = unsafe { &*hw };
    if let Some(samples) = hw.read_capture() {
        let to_copy = samples.len().min(len);
        unsafe {
            std::ptr::copy_nonoverlapping(samples.as_ptr(), buf, to_copy);
        }
        to_copy as i32
    } else {
        0
    }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_write_playback(hw: *mut AudioHardware, buf: *const i16, len: usize) -> i32 {
    let hw = unsafe { &*hw };
    let mut vec = vec![0i16; len];
    unsafe {
        std::ptr::copy_nonoverlapping(buf, vec.as_mut_ptr(), len);
    }
    if hw.write_playback(vec).is_ok() { 0 } else { -1 }
}

/// Callback interface for receiving async events from Rust
/// 
/// IMPORTANT: All callbacks are invoked on background threads.
/// UI code MUST dispatch to the main thread:
/// 
/// Swift: DispatchQueue.main.async { ... }
/// C#:   Dispatcher.UIThread.Post(() => { ... });
#[uniffi::export(callback_interface)]
pub trait AuraDelegate: Send + Sync {
    // Connection events
    fn on_connected(&self);
    fn on_disconnected(&self, reason: String);
    fn on_error(&self, error: String);
    
    // User events
    fn on_user_joined(&self, user_id: u32, name: String);
    fn on_user_left(&self, user_id: u32);
    fn on_user_moved(&self, user_id: u32, x: f32, y: f32, z: f32);
    fn on_user_comment(&self, user_id: u32, text: String);
    
    // Audio events
    fn on_audio_packet(&self, user_id: u32, opus_data: Vec<u8>);
    
    // Text events
    fn on_text_message(&self, msg: String);
    fn on_channel_message(&self, user_id: u32, message: String);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = AuraClient::new("127.0.0.1:443".into(), "id.key".into());
        assert!(!client.connected.load(Ordering::Relaxed));
    }
    
    #[test]
    fn test_connect_disconnect() {
        let client = AuraClient::new("127.0.0.1:443".into(), "id.key".into());
        
        client.connect("127.0.0.1:443".into(), "test_token".into()).unwrap();
        assert!(client.connected.load(Ordering::Relaxed));
        
        client.disconnect();
        assert!(!client.connected.load(Ordering::Relaxed));
    }
    
    #[test]
    fn test_dual_epoch_tracking() {
        let client = AuraClient::new("127.0.0.1:443".into(), "id.key".into());
        
        client.set_voice_epoch(5);
        client.set_text_epoch(500);
        
        assert_eq!(client.voice_epoch.load(Ordering::Relaxed), 5);
        assert_eq!(client.text_epoch.load(Ordering::Relaxed), 500);
    }
}

#[cfg(test)]
mod tests_audio;
#[cfg(test)]
mod tests_crypto;
