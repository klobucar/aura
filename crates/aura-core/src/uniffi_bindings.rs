//! UniFFI-compatible wrappers for the audio pipeline
//!
//! These wrapper types are used by the UDL-defined interfaces.
//! They provide a simpler API that works with UniFFI's scaffolding.

use std::sync::{Mutex, RwLock};
use bytes::Bytes;

use crate::audio_pipeline::{
    AudioSender as InternalSender,
    AudioReceiver as InternalReceiver,
    AudioPipelineError as InternalError,
};
#[cfg(feature = "native-audio")]
use crate::audio_io::AudioDevice;
use crate::crypto::KEY_SIZE;

/// Audio error type for UniFFI
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AudioError {
    #[error("Invalid key size")]
    InvalidKeySize,
    #[error("Opus encoding error")]
    OpusError,
    #[error("Crypto error")]
    CryptoError,
    #[error("Packet parse error")]
    PacketParseError,
    #[error("Unknown sender: {0}")]
    UnknownSender(u32),
}

/// Convert internal error to UniFFI-compatible error
fn convert_error(e: InternalError) -> AudioError {
    match e {
        InternalError::Opus(_) => AudioError::OpusError,
        InternalError::Crypto(_) => AudioError::CryptoError,
        InternalError::PacketParse(_) => AudioError::PacketParseError,
        InternalError::UnknownSender(id) => AudioError::UnknownSender(id),
    }
}

/// Audio sender wrapper - manages an InternalSender with thread safety
#[derive(uniffi::Object)]
pub struct AudioSenderWrapper {
    inner: RwLock<InternalSender>,
}

#[uniffi::export]
impl AudioSenderWrapper {
    /// Create a new audio sender
    #[uniffi::constructor]
    pub fn new(session_id: u32, key: &[u8]) -> Result<Self, AudioError> {
        if key.len() != KEY_SIZE {
            return Err(AudioError::InvalidKeySize);
        }
        
        let mut key_arr = [0u8; KEY_SIZE];
        key_arr.copy_from_slice(key);
        
        let inner = InternalSender::new(session_id, &key_arr)
            .map_err(convert_error)?;
        
        Ok(Self {
            inner: RwLock::new(inner),
        })
    }
    
    /// Set current MLS epoch
    pub fn set_epoch(&self, epoch: u64) {
        if let Ok(inner) = self.inner.read() {
            inner.set_epoch(epoch);
        }
    }
    
    /// Encode and encrypt PCM audio
    pub fn process(&self, pcm: &[i16]) -> Result<Vec<u8>, AudioError> {
        let inner = self.inner.read().map_err(|_| AudioError::CryptoError)?;
        let bytes = inner.process(pcm).map_err(convert_error)?;
        Ok(bytes.to_vec())
    }
    
    /// Encode and encrypt f32 PCM audio
    pub fn process_float(&self, pcm: Vec<f32>) -> Result<Vec<u8>, AudioError> {
        let inner = self.inner.read().map_err(|_| AudioError::CryptoError)?;
        let bytes = inner.process_float(&pcm).map_err(convert_error)?;
        Ok(bytes.to_vec())
    }
    
    /// Set DRED duration in 10ms frames (0 to 100)
    pub fn set_dred_duration(&self, duration: i32) {
        if let Ok(inner) = self.inner.read() {
            let _ = inner.set_dred_duration(duration);
        }
    }

    /// Get current sequence number
    pub fn sequence(&self) -> u16 {
        self.inner.read().map(|i| i.sequence()).unwrap_or(0)
    }
}

/// Audio receiver wrapper - manages an InternalReceiver with thread safety
#[derive(uniffi::Object)]
pub struct AudioReceiverWrapper {
    inner: RwLock<InternalReceiver>,
}

/// Decoded frame with sender info
#[derive(uniffi::Record)]
pub struct DecodedFrame {
    pub session_id: u32,
    pub pcm: Vec<i16>,
}

#[uniffi::export]
impl AudioReceiverWrapper {
    /// Create a new audio receiver
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(InternalReceiver::new()),
        }
    }
    
    /// Add a sender with their key and epoch hint
    /// 
    /// # Arguments
    /// * `session_id` - Unique session ID for this sender
    /// * `key` - 32-byte encryption key derived from MLS
    /// * `epoch_hint` - Current MLS epoch (low 16 bits)
    pub fn add_sender(&self, session_id: u32, key: &[u8], epoch_hint: u16) -> Result<(), AudioError> {
        if key.len() != KEY_SIZE {
            return Err(AudioError::InvalidKeySize);
        }
        
        let mut key_arr = [0u8; KEY_SIZE];
        key_arr.copy_from_slice(key);
        
        let inner = self.inner.read().map_err(|_| AudioError::CryptoError)?;
        inner.add_sender(session_id, &key_arr, epoch_hint).map_err(convert_error)?;
        Ok(())
    }
    
    /// Update a sender's key (called when MLS epoch advances)
    /// 
    /// Old keys are retained for graceful epoch handover.
    pub fn update_sender_key(&self, session_id: u32, key: &[u8], epoch_hint: u16) -> Result<bool, AudioError> {
        if key.len() != KEY_SIZE {
            return Err(AudioError::InvalidKeySize);
        }
        
        let mut key_arr = [0u8; KEY_SIZE];
        key_arr.copy_from_slice(key);
        
        let inner = self.inner.read().map_err(|_| AudioError::CryptoError)?;
        Ok(inner.update_sender_key(session_id, &key_arr, epoch_hint))
    }
    
    /// Remove a sender
    pub fn remove_sender(&self, session_id: u32) {
        if let Ok(inner) = self.inner.read() {
            inner.remove_sender(session_id);
        }
    }
    
    /// Process a received packet
    pub fn on_packet(&self, data: &[u8]) -> Result<(), AudioError> {
        let inner = self.inner.read().map_err(|_| AudioError::CryptoError)?;
        inner.on_packet(Bytes::copy_from_slice(data)).map_err(convert_error)?;
        Ok(())
    }
    
    /// Pop decoded frames
    pub fn pop_decoded(&self) -> Vec<DecodedFrame> {
        self.inner.read()
            .map(|i| {
                i.pop_decoded()
                    .into_iter()
                    .map(|(session_id, pcm)| DecodedFrame { session_id, pcm })
                    .collect()
            })
            .unwrap_or_default()
    }
    
    /// Pop mixed audio
    pub fn pop_mixed(&self) -> Option<Vec<i16>> {
        self.inner.read().ok()?.pop_mixed()
    }
}


impl Default for AudioReceiverWrapper {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Audio Hardware - UniFFI-compatible wrapper for cpal
// =============================================================================

#[cfg(feature = "native-audio")]
#[derive(uniffi::Object)]
pub struct AudioHardware {
    device: Mutex<AudioDevice>,
}

#[cfg(feature = "native-audio")]
#[uniffi::export]
impl AudioHardware {
    #[uniffi::constructor]
    pub fn new() -> Result<Self, AudioError> {
        let device = AudioDevice::new().map_err(|_e| AudioError::OpusError)?;
        Ok(Self {
            device: Mutex::new(device),
        })
    }

    pub fn start(&self) -> Result<(), AudioError> {
        let device = self.device.lock().map_err(|_| AudioError::OpusError)?;
        device.start().map_err(|_| AudioError::OpusError)?;
        Ok(())
    }

    pub fn stop(&self) -> Result<(), AudioError> {
        let device = self.device.lock().map_err(|_| AudioError::OpusError)?;
        device.stop().map_err(|_| AudioError::OpusError)?;
        Ok(())
    }

    pub fn start_capture(&self) -> Result<(), AudioError> {
        let device = self.device.lock().map_err(|_| AudioError::OpusError)?;
        device.start_capture().map_err(|_| AudioError::OpusError)?;
        Ok(())
    }

    pub fn stop_capture(&self) -> Result<(), AudioError> {
        let device = self.device.lock().map_err(|_| AudioError::OpusError)?;
        device.stop_capture().map_err(|_| AudioError::OpusError)?;
        Ok(())
    }

    pub fn read_capture(&self) -> Option<Vec<i16>> {
        let device = self.device.lock().ok()?;
        device.try_recv_capture()
    }

    pub fn write_playback(&self, pcm: Vec<i16>) -> Result<(), AudioError> {
        let device = self.device.lock().map_err(|_| AudioError::OpusError)?;
        device.send_playback(pcm).map_err(|_| AudioError::OpusError)?;
        Ok(())
    }
}

// =============================================================================
// Text Crypto - UniFFI-compatible wrappers
// =============================================================================

use crate::text_crypto::{encrypt_text, decrypt_text, create_text_message, TextMessage, EncryptedTextPacket};

/// UniFFI-compatible TextMessage (avoids protobuf dependency in bindings)
#[derive(Debug, Clone, uniffi::Record)]
pub struct TextMessageRecord {
    pub sender_uuid: String,
    pub timestamp: u64,
    pub content: String,
    pub reply_to_id: String,
    pub message_id: String,
}

impl From<TextMessage> for TextMessageRecord {
    fn from(m: TextMessage) -> Self {
        Self {
            sender_uuid: m.sender_uuid,
            timestamp: m.timestamp,
            content: m.content,
            reply_to_id: m.reply_to_id,
            message_id: m.message_id,
        }
    }
}

impl From<TextMessageRecord> for TextMessage {
    fn from(r: TextMessageRecord) -> Self {
        Self {
            sender_uuid: r.sender_uuid,
            timestamp: r.timestamp,
            content: r.content,
            reply_to_id: r.reply_to_id,
            message_id: r.message_id,
        }
    }
}

/// UniFFI-compatible EncryptedTextPacket
#[derive(Debug, Clone, uniffi::Record)]
pub struct EncryptedTextPacketRecord {
    pub sender_session_id: u32,
    pub channel_id: u32,
    pub epoch: u64,
    pub message_id: String,
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub tag: Vec<u8>,
    pub reply_to_id: String,
}

impl From<EncryptedTextPacket> for EncryptedTextPacketRecord {
    fn from(p: EncryptedTextPacket) -> Self {
        Self {
            sender_session_id: p.sender_session_id,
            channel_id: p.channel_id,
            epoch: p.epoch,
            message_id: p.message_id,
            ciphertext: p.ciphertext,
            nonce: p.nonce,
            tag: p.tag,
            reply_to_id: p.reply_to_id,
        }
    }
}

impl From<EncryptedTextPacketRecord> for EncryptedTextPacket {
    fn from(r: EncryptedTextPacketRecord) -> Self {
        Self {
            sender_session_id: r.sender_session_id,
            channel_id: r.channel_id,
            epoch: r.epoch,
            message_id: r.message_id,
            ciphertext: r.ciphertext,
            nonce: r.nonce,
            tag: r.tag,
            reply_to_id: r.reply_to_id,
        }
    }
}

/// Text crypto error type for UniFFI
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TextCryptoError {
    #[error("Invalid key size")]
    InvalidKeySize,
    #[error("Encryption failed")]
    EncryptionFailed,
    #[error("Decryption failed")]
    DecryptionFailed,
}

/// Text crypto wrapper for UniFFI - exposes text encryption/decryption
#[derive(uniffi::Object)]
pub struct TextCryptoWrapper {
    dave_key: [u8; 32],
}

#[uniffi::export]
impl TextCryptoWrapper {
    /// Create a new text crypto context with the given DAVE key
    #[uniffi::constructor]
    pub fn new(key: Vec<u8>) -> Result<Self, TextCryptoError> {
        if key.len() != 32 {
            return Err(TextCryptoError::InvalidKeySize);
        }
        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(&key);
        Ok(Self { dave_key: key_arr })
    }
    
    /// Encrypt a text message
    pub fn encrypt(
        &self,
        epoch: u64,
        channel_id: u32,
        sender_session_id: u32,
        message: TextMessageRecord,
    ) -> Result<EncryptedTextPacketRecord, TextCryptoError> {
        let text_msg: TextMessage = message.into();
        encrypt_text(&self.dave_key, epoch, channel_id, sender_session_id, &text_msg)
            .map(|p| p.into())
            .map_err(|_| TextCryptoError::EncryptionFailed)
    }
    
    /// Decrypt an encrypted text packet
    pub fn decrypt(&self, packet: EncryptedTextPacketRecord) -> Result<TextMessageRecord, TextCryptoError> {
        let enc_packet: EncryptedTextPacket = packet.into();
       decrypt_text(&self.dave_key, &enc_packet)
            .map(|m| m.into())
            .map_err(|_| TextCryptoError::DecryptionFailed)
    }
}

/// Create a new text message with auto-generated ID and timestamp
#[uniffi::export]
pub fn create_text_message_record(
    sender_uuid: String,
    content: String,
    reply_to_id: Option<String>,
) -> TextMessageRecord {
    let msg = create_text_message(&sender_uuid, &content, reply_to_id.as_deref());
    msg.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::DaveCrypto;
    
    #[test]
    fn test_wrapper_roundtrip() {
        let key = DaveCrypto::random_key();
        let session_id = 42;
        
        let sender = AudioSenderWrapper::new(session_id, &key).expect("Create sender");
        let receiver = AudioReceiverWrapper::new();
        receiver.add_sender(session_id, &key, 0).expect("Add sender");
        
        let pcm = vec![1000i16; 960];
        let packet = sender.process(&pcm).expect("Process");
        
        receiver.on_packet(&packet).expect("On packet");
        
        let decoded = receiver.pop_decoded();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].session_id, session_id);
    }
}
