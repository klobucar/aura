//! UniFFI-compatible wrappers for the audio pipeline
//!
//! These wrapper types are used by the UDL-defined interfaces.
//! They provide a simpler API that works with UniFFI's scaffolding.

use std::sync::{Mutex, RwLock};
use bytes::Bytes;

use aura_protocol::{
    FastAudioPacket, UserProfile as ProtoProfile, ChannelInfo as ProtoChannel, 
    ServerState as ProtoState, ChannelIcon as ProtoIcon, channel_icon,
    CreateChannelRequest as ProtoCreateChannel, UpdateChannelRequest as ProtoUpdateChannel,
    UpdateProfile as ProtoUpdateProfile,
};
use prost::Message;
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
    
    /// Update encryption key and epoch (called when MLS epoch advances)
    pub fn update_key(&self, key: Vec<u8>, epoch: u64) -> Result<(), AudioError> {
        if key.len() != KEY_SIZE {
            return Err(AudioError::InvalidKeySize);
        }
        
        let mut key_arr = [0u8; KEY_SIZE];
        key_arr.copy_from_slice(&key);
        
        if let Ok(inner) = self.inner.read() {
            inner.update_key(&key_arr, epoch);
            Ok(())
        } else {
            Err(AudioError::CryptoError)
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
        let bytes = inner.process_float_with_reference(&pcm, None).map_err(convert_error)?;
        Ok(bytes.to_vec())
    }
    
    /// Set DRED duration in 10ms frames (0 to 100)
    pub fn set_dred_duration(&self, duration: i32) {
        if let Ok(inner) = self.inner.read() {
            let _ = inner.set_dred_duration(duration);
        }
    }
    
    /// Enable or disable noise suppression (RNNoise)
    pub fn set_noise_suppression_enabled(&self, enabled: bool) {
        if let Ok(inner) = self.inner.read() {
            inner.set_rnnoise_enabled(enabled);
        }
    }
    
    /// Enable or disable WebRTC AEC (Echo Cancellation)
    /// Note: Only works if compiled with --features webrtc-audio
    pub fn set_webrtc_aec_enabled(&self, enabled: bool) {
        #[cfg(feature = "webrtc-audio")]
        if let Ok(inner) = self.inner.read() {
            inner.set_webrtc_aec_enabled(enabled);
        }
        #[cfg(not(feature = "webrtc-audio"))]
        let _ = enabled; // Suppress unused warning
    }
    
    /// Enable or disable WebRTC NS (Noise Suppression)
    /// Note: Only works if compiled with --features webrtc-audio
    pub fn set_webrtc_ns_enabled(&self, enabled: bool) {
        #[cfg(feature = "webrtc-audio")]
        if let Ok(inner) = self.inner.read() {
            inner.set_webrtc_ns_enabled(enabled);
        }
        #[cfg(not(feature = "webrtc-audio"))]
        let _ = enabled; // Suppress unused warning
    }
    
    /// Enable or disable WebRTC AGC (Auto Gain Control)
    /// Note: Only works if compiled with --features webrtc-audio
    pub fn set_webrtc_agc_enabled(&self, enabled: bool) {
        #[cfg(feature = "webrtc-audio")]
        if let Ok(inner) = self.inner.read() {
            inner.set_webrtc_agc_enabled(enabled);
        }
        #[cfg(not(feature = "webrtc-audio"))]
        let _ = enabled; // Suppress unused warning
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

/// Decoded audio frame from a specific sender
#[derive(uniffi::Record)]
pub struct DecodedFrame {
    pub session_id: u32,
    pub pcm: Vec<i16>,
}

/// Mixed audio with speaker metadata
#[derive(uniffi::Record)]
pub struct MixedAudioResult {
    /// Mixed PCM samples (960 samples, 20ms at 48kHz)
    pub pcm: Vec<i16>,
    /// Session IDs that contributed to this mix
    pub active_speakers: Vec<u32>,
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
    
    /// Pop mixed audio for playback
    /// Returns mixed PCM and list of active speaker session IDs
    pub fn pop_mixed(&self) -> Option<MixedAudioResult> {
        self.inner.read().ok()?.pop_mixed().map(|mixed| MixedAudioResult {
            pcm: mixed.pcm,
            active_speakers: mixed.active_speakers,
        })
    }
    
    /// Set jitter buffer target latency in milliseconds
    pub fn set_jitter_buffer_ms(&self, latency_ms: u32) {
        if let Ok(inner) = self.inner.read() {
            inner.set_jitter_buffer_ms(latency_ms);
        }
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

// =============================================================================
// Metadata & State - UniFFI-compatible records
// =============================================================================

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelIconRecord {
    pub emoji: Option<String>,
    pub preset_id: Option<String>,
    pub custom_data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelInfoRecord {
    pub channel_id: u32,
    pub name: String,
    pub comment: String,
    pub icon: Option<ChannelIconRecord>,
    pub position: i32,
    pub user_ids: Vec<u32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UserProfileRecord {
    pub user_id: u32,
    pub display_name: String,
    pub bio: String,
    pub avatar_data: Vec<u8>,
    pub signature: Vec<u8>,
    pub signing_key: Vec<u8>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ServerStateRecord {
    pub channels: Vec<ChannelInfoRecord>,
    pub profiles: Vec<UserProfileRecord>,
}

#[uniffi::export]
pub fn decode_server_state(data: Vec<u8>) -> Result<ServerStateRecord, AudioError> {
    use prost::Message;
    let proto = ProtoState::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    
    let channels = proto.channels.into_iter().map(|c| {
        let icon = c.icon.and_then(|i| i.icon).map(|icon| {
            match icon {
                channel_icon::Icon::Emoji(e) => ChannelIconRecord { emoji: Some(e), preset_id: None, custom_data: None },
                channel_icon::Icon::PresetId(p) => ChannelIconRecord { emoji: None, preset_id: Some(p), custom_data: None },
                channel_icon::Icon::CustomData(d) => ChannelIconRecord { emoji: None, preset_id: None, custom_data: Some(d.to_vec()) },
            }
        });

        ChannelInfoRecord {
            channel_id: c.channel_id,
            name: c.name,
            comment: c.comment,
            icon,
            position: c.position,
            user_ids: c.user_ids,
        }
    }).collect();

    let profiles = proto.profiles.into_iter().map(|p| {
        UserProfileRecord {
            user_id: p.user_id,
            display_name: p.display_name,
            bio: p.bio,
            avatar_data: p.avatar_data.to_vec(),
            signature: p.signature.to_vec(),
            signing_key: p.signing_key.to_vec(),
        }
    }).collect();

    Ok(ServerStateRecord { channels, profiles })
}

#[uniffi::export]
pub fn encode_update_profile(profile: UserProfileRecord) -> Vec<u8> {
    use prost::Message;
    let proto_profile = ProtoProfile {
        user_id: profile.user_id,
        display_name: profile.display_name,
        bio: profile.bio,
        avatar_data: profile.avatar_data.into(),
        signature: profile.signature.into(),
        signing_key: profile.signing_key.into(),
    };
    
    let req = ProtoUpdateProfile {
        profile: Some(proto_profile),
    };
    
    req.encode_to_vec()
}

// =============================================================================
// MLS Encryption - UniFFI Wrapper
// =============================================================================

use crate::mls::{MlsClient, MlsError as InternalMlsError};

/// Result of adding a member to an MLS group
#[derive(Debug, Clone, uniffi::Record)]
pub struct MlsCommitWelcome {
    /// Serialized Commit message to broadcast to existing members
    pub commit: Vec<u8>,
    /// Serialized Welcome message to send to the new member
    pub welcome: Vec<u8>,
}

/// MLS error type for UniFFI
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MlsError {
    #[error("MLS operation failed: {0}")]
    OperationFailed(String),
    #[error("Group not found")]
    GroupNotFound,
    #[error("Invalid key package")]
    InvalidKeyPackage,
}

impl From<InternalMlsError> for MlsError {
    fn from(e: InternalMlsError) -> Self {
        match e {
            InternalMlsError::GroupNotFound(_msg) => MlsError::GroupNotFound,
            _ => MlsError::OperationFailed(e.to_string()),
        }
    }
}

/// MLS client wrapper for managing groups and deriving keys
#[derive(uniffi::Object)]
pub struct MlsWrapper {
    inner: Mutex<MlsClient>,
}

#[uniffi::export]
impl MlsWrapper {
    /// Create a new MLS client with the given identity name
    /// 
    /// # Arguments
    /// * `identity_name` - User's UUID or unique identifier
    #[uniffi::constructor]
    pub fn new(identity_name: String) -> Result<Self, MlsError> {
        let client = MlsClient::new(&identity_name)?;
        Ok(Self {
            inner: Mutex::new(client),
        })
    }
    
    /// Generate a key package to send to the server
    /// 
    /// Returns serialized KeyPackage bytes that can be sent to server
    pub fn create_key_package(&self) -> Result<Vec<u8>, MlsError> {
        let client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        client.get_key_package_bytes().map_err(Into::into)
    }
    
    /// Create a new MLS group (as the first member)
    /// 
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `is_voice` - true for voice group, false for text group
    pub fn create_group(&self, channel_id: u32, is_voice: bool) -> Result<(), MlsError> {
        let mut client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = make_mls_group_id(channel_id, is_voice);
        client.create_group(&group_id).map_err(Into::into)
    }
    
    /// Join a group via a Welcome message from the server
    /// 
    /// # Arguments
    /// * `welcome_bytes` - Serialized Welcome message
    pub fn join_group(&self, welcome_bytes: Vec<u8>) -> Result<(), MlsError> {
        let mut client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        client.process_welcome(&welcome_bytes)?;
        Ok(())
    }
    
    /// Add a member to a group (returns Commit and Welcome)
    /// 
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `is_voice` - true for voice group, false for text group
    /// * `key_package_bytes` - Serialized KeyPackage from new member
    /// 
    /// # Returns
    /// MlsCommitWelcome containing commit and welcome bytes
    pub fn add_member(
        &self,
        channel_id: u32,
        is_voice: bool,
        key_package_bytes: Vec<u8>,
    ) -> Result<MlsCommitWelcome, MlsError> {
        let mut client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = make_mls_group_id(channel_id, is_voice);
        let (commit, welcome) = client.add_member(&group_id, &key_package_bytes)?;
        Ok(MlsCommitWelcome { commit, welcome })
    }
    
    /// Process a Commit message from another member
    /// 
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `is_voice` - true for voice group, false for text group
    /// * `commit_bytes` - Serialized Commit message
    pub fn process_commit(
        &self,
        channel_id: u32,
        is_voice: bool,
        commit_bytes: Vec<u8>,
    ) -> Result<u64, MlsError> {
        let mut client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = make_mls_group_id(channel_id, is_voice);
        client.process_commit(&group_id, &commit_bytes).map_err(Into::into)
    }
    
    /// Export encryption key for audio (voice group)
    /// 
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `sender_session_id` - Session ID of the audio sender
    /// 
    /// # Returns
    /// 32-byte encryption key for this sender
    pub fn export_audio_key(&self, channel_id: u32, sender_session_id: u32) -> Result<Vec<u8>, MlsError> {
        let client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = make_mls_group_id(channel_id, true);
        let (key, _epoch) = client.export_sender_key(&group_id, sender_session_id)?;
        Ok(key.to_vec())
    }
    
    /// Export encryption key for text (text group)
    /// 
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `sender_session_id` - Session ID of the text sender
    /// 
    /// # Returns
    /// 32-byte encryption key for this sender
    pub fn export_text_key(&self, channel_id: u32, sender_session_id: u32) -> Result<Vec<u8>, MlsError> {
        let client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = make_mls_group_id(channel_id, false);
        let (key, _epoch) = client.export_sender_key(&group_id, sender_session_id)?;
        Ok(key.to_vec())
    }
    
    /// Get current epoch for a group
    /// 
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `is_voice` - true for voice group, false for text group
    pub fn current_epoch(&self, channel_id: u32, is_voice: bool) -> Result<u64, MlsError> {
        let client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = make_mls_group_id(channel_id, is_voice);
        client.epoch(&group_id).map_err(Into::into)
    }
    
    /// Check if we're a member of a group
    pub fn is_member(&self, channel_id: u32, is_voice: bool) -> bool {
        if let Ok(client) = self.inner.lock() {
            let group_id = make_mls_group_id(channel_id, is_voice);
            client.is_member(&group_id)
        } else {
            false
        }
    }
}

// Internal helper to generate group IDs (outside UniFFI export)
fn make_mls_group_id(channel_id: u32, is_voice: bool) -> Vec<u8> {
    let group_type = if is_voice { "voice" } else { "text" };
    format!("aura-ch{}-{}", channel_id, group_type).into_bytes()
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
#[uniffi::export]
pub fn encode_create_channel(name: String, comment: String, icon: Option<ChannelIconRecord>) -> Vec<u8> {
    let proto_icon = icon.map(|i| ProtoIcon {
        icon: i.emoji.map(|e| channel_icon::Icon::Emoji(e))
            .or_else(|| i.preset_id.map(|p| channel_icon::Icon::PresetId(p)))
            .or_else(|| i.custom_data.map(|c| channel_icon::Icon::CustomData(c))),
    });

    let req = ProtoCreateChannel {
        name,
        comment,
        icon: proto_icon,
    };
    req.encode_to_vec()
}

#[uniffi::export]
pub fn encode_update_channel(
    channel_id: u32,
    name: Option<String>,
    comment: Option<String>,
    icon: Option<ChannelIconRecord>,
    position: Option<i32>,
) -> Vec<u8> {
    let proto_icon = icon.map(|i| ProtoIcon {
        icon: i.emoji.map(|e| channel_icon::Icon::Emoji(e))
            .or_else(|| i.preset_id.map(|p| channel_icon::Icon::PresetId(p)))
            .or_else(|| i.custom_data.map(|c| channel_icon::Icon::CustomData(c))),
    });

    let req = ProtoUpdateChannel {
        channel_id,
        name,
        comment,
        icon: proto_icon,
        position,
    };
    req.encode_to_vec()
}
