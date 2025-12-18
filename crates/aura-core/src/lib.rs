//! Aura Core - Cross-Platform VoIP Client Engine
//!
//! This crate provides the shared Rust logic for the Aura voice/text client,
//! exposed to Swift (macOS) and C# (Windows) via UniFFI bindings.

#![allow(unpredictable_function_pointer_comparisons)]

use std::sync::{Arc, Mutex, atomic::{AtomicU64, AtomicU16, AtomicBool, Ordering}};
use aura_protocol::Position;

pub mod opus;
pub mod jitter_buffer;
pub mod crypto;
pub mod audio_pipeline;
pub mod mls;
pub mod text_crypto;
pub mod voice_session;
#[cfg(feature = "native-audio")]
pub mod audio_io;
pub mod vad;
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
}

#[uniffi::export]
impl AuraClient {
    /// Create a new AuraClient
    #[uniffi::constructor]
    pub fn new(url: String, identity_file: String) -> Self {
        Self {
            url,
            identity_file,
            position: Mutex::new(Position { x: 0.0, y: 0.0, z: 0.0 }),
            delegate: Mutex::new(None),
            connected: AtomicBool::new(false),
            voice_epoch: AtomicU64::new(0),
            text_epoch: AtomicU64::new(0),
            sequence: AtomicU16::new(0),
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
    /// 1. Opus encode PCM -> compressed audio
    /// 2. Zero-pad (16 bytes) for DAVE commitment
    /// 3. XChaCha20-Poly1305 encrypt with random nonce
    /// 4. Build FastAudioPacket with epoch_hint
    /// 5. Send via QUIC datagram
    pub fn send_audio_frame(&self, pcm_data: Vec<f32>) {
        if !self.connected.load(Ordering::Relaxed) {
            return;
        }
        
        // 1. Encode PCM -> Opus (Simulation)
        let opus_data = vec![0u8; pcm_data.len()]; // TODO: Use audiopus

        // 2. Generate random 192-bit nonce for XChaCha20-Poly1305
        // TODO: use rand::thread_rng().gen()
        let nonce: [u8; 24] = [0u8; 24];
        
        // 3. Get epoch hint (low 16 bits of voice epoch)
        let epoch_hint = (self.voice_epoch.load(Ordering::Relaxed) & 0xFFFF) as u16;
        
        // 4. Get and increment sequence number
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);

        // 5. Encrypt with XChaCha20-Poly1305
        // See security comment block above for zero-padding requirement
        let ciphertext = opus_data; // TODO: Implement actual encryption

        // 6. Build FastAudioPacket
        let pos = self.position.lock().unwrap();
        let user_id = 1; // TODO: From identity

        let _fast_packet = aura_protocol::FastAudioPacket {
            session_id: user_id,
            epoch_hint,
            sequence,
            nonce,
            payload: bytes::Bytes::from(ciphertext),
        };
        
        // 7. Serialize header packet for logging/debugging
        let packet = aura_protocol::AudioPacket {
            header: Some(aura_protocol::Header {
                user_id,
                sequence: sequence as u64,
                position: Some(pos.clone()),
            }),
            ciphertext: vec![],
        };
        
        use prost::Message;
        let _bytes = packet.encode_to_vec();
        // TODO: self.quic_connection.send_datagram(fast_packet.to_bytes())
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

    /// Update own profile comment/bio
    pub fn set_comment(&self, text: String) {
        println!("Setting own comment: {} chars", text.len());
        // TODO: Sign profile and send to server
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
