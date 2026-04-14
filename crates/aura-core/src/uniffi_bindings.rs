//! UniFFI-compatible wrappers for the audio pipeline
//!
//! These wrapper types are used by the UDL-defined interfaces.
//! They provide a simpler API that works with UniFFI's scaffolding.

use std::sync::{Mutex, RwLock};
use bytes::Bytes;

use aura_protocol::{
    UserProfile as ProtoProfile, 
    ServerState as ProtoState, ChannelIcon as ProtoIcon, channel_icon,
    CreateChannelRequest as ProtoCreateChannel, UpdateChannelRequest as ProtoUpdateChannel,
    UpdateProfile as ProtoUpdateProfile, UserStatusUpdate as ProtoUserStatusUpdate,
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
    pub media_type: i32,
    pub file_size: u64,
    pub sha256_hash: String,
}

impl From<TextMessage> for TextMessageRecord {
    fn from(m: TextMessage) -> Self {
        Self {
            sender_uuid: m.sender_uuid,
            timestamp: m.timestamp,
            content: m.content,
            reply_to_id: m.reply_to_id,
            message_id: m.message_id,
            media_type: m.r#type,
            file_size: m.file_size,
            sha256_hash: m.sha256_hash,
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
            r#type: r.media_type,
            file_size: r.file_size,
            sha256_hash: r.sha256_hash,
        }
    }
}

/// UniFFI-compatible EncryptedTextPacket
#[derive(Debug, Clone, uniffi::Record)]
pub struct EncryptedTextPacketRecord {
    pub sender_session_id: u32,
    pub channel_id: String,
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
        channel_id: String,
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

#[derive(Debug, Clone, uniffi::Enum)]
pub enum ChannelType {
    Regular,
    Lobby,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelIconRecord {
    pub emoji: Option<String>,
    pub preset_id: Option<String>,
    pub custom_data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelInfoRecord {
    pub channel_id: String,
    pub name: String,
    pub comment: String,
    pub icon: Option<ChannelIconRecord>,
    pub position: i32,
    pub user_ids: Vec<u32>,
    pub users: Vec<ChannelUserStatusRecord>,
    pub channel_type: ChannelType,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelUserStatusRecord {
    pub session_id: u32,
    pub is_muted: bool,
    pub is_deafened: bool,
    pub display_name: String,
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
pub struct UserStatusUpdate {
    pub session_id: u32,
    pub is_muted: bool,
    pub is_deafened: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ServerStateRecord {
    pub channels: Vec<ChannelInfoRecord>,
    pub profiles: Vec<UserProfileRecord>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UserJoinedRecord {
    pub channel_id: String,
    pub session_id: u32,
    pub display_name: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UserLeftRecord {
    pub channel_id: String,
    pub session_id: u32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct JoinChannelRequestRecord {
    pub channel_id: String,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum MlsGroupType {
    Voice,
    Text,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MlsCommitWelcomeDetailRecord {
    pub commit: Vec<u8>,
    pub welcome: Vec<u8>,
    pub new_member_session_id: u32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MlsEnvelopeRecord {
    pub sender_id: u32,
    pub channel_id: String,
    pub group_type: MlsGroupType,
    pub target_session_id: u32,
    pub target_uuid: String,
    pub epoch: u64,
    pub key_package: Option<Vec<u8>>,
    pub commit: Option<Vec<u8>>,
    pub welcome: Option<Vec<u8>>,
    pub commit_welcome: Option<MlsCommitWelcomeDetailRecord>,
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
            users: c.users.into_iter().map(|u| ChannelUserStatusRecord {
                session_id: u.session_id,
                is_muted: u.is_muted,
                is_deafened: u.is_deafened,
                display_name: u.display_name,
            }).collect(),
            channel_type: match c.r#type {
                1 => ChannelType::Lobby,
                _ => ChannelType::Regular,
            },
        }
    }).collect();

    let profiles = proto.profiles.into_iter().map(|p| {
        UserProfileRecord {
            user_id: p.user_id.parse().unwrap_or(0),
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
        user_id: profile.user_id.to_string(),
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

/// Decode a standalone `UserProfile` protobuf payload as broadcast by the
/// server on MSG_PROFILE_UPDATED (0x46). Note: this is the bare profile
/// struct, not an `UpdateProfile` request wrapper.
#[uniffi::export]
pub fn decode_user_profile(data: Vec<u8>) -> Result<UserProfileRecord, AudioError> {
    use prost::Message;
    let proto = ProtoProfile::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    Ok(UserProfileRecord {
        user_id: proto.user_id.parse().unwrap_or(0),
        display_name: proto.display_name,
        bio: proto.bio,
        avatar_data: proto.avatar_data.to_vec(),
        signature: proto.signature.to_vec(),
        signing_key: proto.signing_key.to_vec(),
    })
}

#[uniffi::export]
pub fn encode_user_status_update(update: UserStatusUpdate) -> Vec<u8> {
    use prost::Message;
    let proto = ProtoUserStatusUpdate {
        session_id: update.session_id,
        is_muted: update.is_muted,
        is_deafened: update.is_deafened,
    };
    proto.encode_to_vec()
}

#[uniffi::export]
pub fn decode_user_status_update(data: Vec<u8>) -> Result<UserStatusUpdate, AudioError> {
    use prost::Message;
    let proto = ProtoUserStatusUpdate::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    Ok(UserStatusUpdate {
        session_id: proto.session_id,
        is_muted: proto.is_muted,
        is_deafened: proto.is_deafened,
    })
}

#[uniffi::export]
pub fn decode_user_joined(data: Vec<u8>) -> Result<UserJoinedRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::UserJoined::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    Ok(UserJoinedRecord {
        channel_id: proto.channel_id,
        session_id: proto.session_id,
        display_name: proto.display_name,
    })
}

#[uniffi::export]
pub fn decode_user_left(data: Vec<u8>) -> Result<UserLeftRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::UserLeft::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    Ok(UserLeftRecord {
        channel_id: proto.channel_id,
        session_id: proto.session_id,
    })
}

#[uniffi::export]
pub fn encode_join_channel_request(req: JoinChannelRequestRecord) -> Vec<u8> {
    use prost::Message;
    let proto = aura_protocol::JoinChannelRequest {
        channel_id: req.channel_id,
    };
    proto.encode_to_vec()
}

#[uniffi::export]
pub fn decode_encrypted_text_packet(data: Vec<u8>) -> Result<EncryptedTextPacketRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::EncryptedTextPacket::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    Ok(EncryptedTextPacketRecord {
        sender_session_id: proto.sender_session_id,
        channel_id: proto.channel_id,
        epoch: proto.epoch,
        message_id: proto.message_id,
        ciphertext: proto.ciphertext.to_vec(),
        nonce: proto.nonce.to_vec(),
        tag: proto.tag.to_vec(),
        reply_to_id: proto.reply_to_id,
    })
}

#[uniffi::export]
pub fn encode_encrypted_text_packet(packet: EncryptedTextPacketRecord) -> Vec<u8> {
    use prost::Message;
    let proto = aura_protocol::EncryptedTextPacket {
        sender_session_id: packet.sender_session_id,
        channel_id: packet.channel_id,
        epoch: packet.epoch,
        message_id: packet.message_id,
        ciphertext: packet.ciphertext.into(),
        nonce: packet.nonce.into(),
        tag: packet.tag.into(),
        reply_to_id: packet.reply_to_id,
    };
    proto.encode_to_vec()
}

#[uniffi::export]
pub fn decode_mls_envelope(data: Vec<u8>) -> Result<MlsEnvelopeRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::MlsEnvelope::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    
    let group_type = if proto.group_type == aura_protocol::MlsGroupType::Voice as i32 {
        MlsGroupType::Voice
    } else {
        MlsGroupType::Text
    };

    let mut key_package = None;
    let mut commit = None;
    let mut welcome = None;
    let mut commit_welcome = None;

    if let Some(content) = proto.content {
        match content {
            aura_protocol::mls_envelope::Content::KeyPackage(kp) => key_package = Some(kp),
            aura_protocol::mls_envelope::Content::Commit(c) => commit = Some(c),
            aura_protocol::mls_envelope::Content::Welcome(w) => welcome = Some(w),
            aura_protocol::mls_envelope::Content::CommitWelcome(cw) => {
                commit_welcome = Some(MlsCommitWelcomeDetailRecord {
                    commit: cw.commit,
                    welcome: cw.welcome,
                    new_member_session_id: cw.new_member_session_id,
                });
            }
            _ => {}
        }
    }

    Ok(MlsEnvelopeRecord {
        sender_id: proto.sender_id,
        channel_id: proto.channel_id,
        group_type,
        target_session_id: proto.target_session_id,
        target_uuid: proto.target_uuid,
        epoch: proto.epoch,
        key_package,
        commit,
        welcome,
        commit_welcome,
    })
}

#[uniffi::export]
pub fn encode_mls_envelope(envelope: MlsEnvelopeRecord) -> Vec<u8> {
    use prost::Message;
    
    let group_type = match envelope.group_type {
        MlsGroupType::Voice => aura_protocol::MlsGroupType::Voice as i32,
        MlsGroupType::Text => aura_protocol::MlsGroupType::Text as i32,
    };

    let content = if let Some(kp) = envelope.key_package {
        Some(aura_protocol::mls_envelope::Content::KeyPackage(kp))
    } else if let Some(c) = envelope.commit {
        Some(aura_protocol::mls_envelope::Content::Commit(c))
    } else if let Some(w) = envelope.welcome {
        Some(aura_protocol::mls_envelope::Content::Welcome(w))
    } else if let Some(cw) = envelope.commit_welcome {
        Some(aura_protocol::mls_envelope::Content::CommitWelcome(aura_protocol::MlsCommitWelcome {
            commit: cw.commit,
            welcome: cw.welcome,
            new_member_session_id: cw.new_member_session_id,
        }))
    } else {
        None
    };

    let proto = aura_protocol::MlsEnvelope {
        sender_id: envelope.sender_id,
        channel_id: envelope.channel_id,
        group_type,
        target_session_id: envelope.target_session_id,
        target_uuid: envelope.target_uuid,
        epoch: envelope.epoch,
        content,
    };
    
    proto.encode_to_vec()
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
    pub fn create_group(&self, channel_id: String, is_voice: bool) -> Result<(), MlsError> {
        let mut client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
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
        channel_id: String,
        is_voice: bool,
        key_package_bytes: Vec<u8>,
    ) -> Result<MlsCommitWelcome, MlsError> {
        let mut client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
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
        channel_id: String,
        is_voice: bool,
        commit_bytes: Vec<u8>,
    ) -> Result<u64, MlsError> {
        let mut client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
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
    pub fn export_audio_key(&self, channel_id: String, sender_session_id: u32) -> Result<Vec<u8>, MlsError> {
        let client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, true).into_bytes();
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
    pub fn export_text_key(&self, channel_id: String, sender_session_id: u32) -> Result<Vec<u8>, MlsError> {
        let client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, false).into_bytes();
        let (key, _epoch) = client.export_sender_key(&group_id, sender_session_id)?;
        Ok(key.to_vec())
    }
    
    /// Get current epoch for a group
    /// 
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `is_voice` - true for voice group, false for text group
    pub fn current_epoch(&self, channel_id: String, is_voice: bool) -> Result<u64, MlsError> {
        let client = self.inner.lock().map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
        client.epoch(&group_id).map_err(Into::into)
    }
    
    /// Check if we're a member of a group
    pub fn is_member(&self, channel_id: String, is_voice: bool) -> bool {
        if let Ok(client) = self.inner.lock() {
            let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
            client.is_member(&group_id)
        } else {
            false
        }
    }
}

// Internal helper to generate group IDs (outside UniFFI export)

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
    channel_id: String,
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
