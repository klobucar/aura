//! UniFFI-compatible wrappers for the audio pipeline
//!
//! These wrapper types are used by the UDL-defined interfaces.
//! They provide a simpler API that works with UniFFI's scaffolding.

use bytes::Bytes;
use std::sync::{Mutex, RwLock};

#[cfg(feature = "native-audio")]
use crate::audio_io::AudioDevice;
use crate::audio_pipeline::{
    AudioPipelineError as InternalError, AudioReceiver as InternalReceiver,
    AudioSender as InternalSender,
};
use crate::crypto::KEY_SIZE;
use aura_protocol::{
    channel_icon, ChannelIcon as ProtoIcon, CreateChannelRequest as ProtoCreateChannel,
    ServerState as ProtoState, UpdateChannelRequest as ProtoUpdateChannel,
    UpdateProfile as ProtoUpdateProfile, UserProfile as ProtoProfile,
    UserStatusUpdate as ProtoUserStatusUpdate,
};
use prost::Message;

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
    #[error("Audio device not found: {0}")]
    DeviceNotFound(String),
    #[error("Audio device error: {0}")]
    DeviceError(String),
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

        let inner = InternalSender::new(session_id, &key_arr).map_err(convert_error)?;

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

    /// Encode and encrypt PCM audio.
    ///
    /// Returns `Ok(None)` when VAD is enabled and the frame is silence — callers
    /// should skip the network send for that frame.
    pub fn process(&self, pcm: &[i16]) -> Result<Option<Vec<u8>>, AudioError> {
        let inner = self.inner.read().map_err(|_| AudioError::CryptoError)?;
        let maybe = inner.process(pcm).map_err(convert_error)?;
        Ok(maybe.map(|b| b.to_vec()))
    }

    /// Encode and encrypt f32 PCM audio.
    ///
    /// Returns `Ok(None)` when VAD is enabled and the (post-NS) frame is silence.
    pub fn process_float(&self, pcm: Vec<f32>) -> Result<Option<Vec<u8>>, AudioError> {
        let inner = self.inner.read().map_err(|_| AudioError::CryptoError)?;
        let maybe = inner
            .process_float_with_reference(&pcm, None)
            .map_err(convert_error)?;
        Ok(maybe.map(|b| b.to_vec()))
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

    /// Enable or disable VAD (Voice Activity Detection) silence skipping.
    /// When enabled, `process` / `process_float` return `None` for silent frames
    /// so the caller can skip the network send.
    pub fn set_vad_enabled(&self, enabled: bool) {
        if let Ok(inner) = self.inner.read() {
            inner.set_vad_enabled(enabled);
        }
    }

    /// Set the VAD detection threshold in dB.
    /// Lower (more negative) is more sensitive; -40 dB is a typical default.
    pub fn set_vad_threshold_db(&self, threshold_db: f32) {
        if let Ok(inner) = self.inner.read() {
            inner.set_vad_threshold_db(threshold_db);
        }
    }

    /// Enable or disable WebRTC AEC (Echo Cancellation).
    /// Returns `true` if applied, `false` if `aura-core` was built without `--features webrtc-audio`.
    pub fn set_webrtc_aec_enabled(&self, enabled: bool) -> bool {
        #[cfg(feature = "webrtc-audio")]
        {
            if let Ok(inner) = self.inner.read() {
                inner.set_webrtc_aec_enabled(enabled);
                return true;
            }
            false
        }
        #[cfg(not(feature = "webrtc-audio"))]
        {
            if enabled {
                tracing::warn!("WebRTC AEC requested, but aura-core was built without the `webrtc-audio` feature");
            }
            false
        }
    }

    /// Enable or disable WebRTC NS (Noise Suppression).
    /// Returns `true` if applied, `false` if `aura-core` was built without `--features webrtc-audio`.
    pub fn set_webrtc_ns_enabled(&self, enabled: bool) -> bool {
        #[cfg(feature = "webrtc-audio")]
        {
            if let Ok(inner) = self.inner.read() {
                inner.set_webrtc_ns_enabled(enabled);
                return true;
            }
            false
        }
        #[cfg(not(feature = "webrtc-audio"))]
        {
            if enabled {
                tracing::warn!("WebRTC NS requested, but aura-core was built without the `webrtc-audio` feature");
            }
            false
        }
    }

    /// Enable or disable WebRTC AGC (Auto Gain Control).
    /// Returns `true` if applied, `false` if `aura-core` was built without `--features webrtc-audio`.
    pub fn set_webrtc_agc_enabled(&self, enabled: bool) -> bool {
        #[cfg(feature = "webrtc-audio")]
        {
            if let Ok(inner) = self.inner.read() {
                inner.set_webrtc_agc_enabled(enabled);
                return true;
            }
            false
        }
        #[cfg(not(feature = "webrtc-audio"))]
        {
            if enabled {
                tracing::warn!("WebRTC AGC requested, but aura-core was built without the `webrtc-audio` feature");
            }
            false
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
    pub fn add_sender(
        &self,
        session_id: u32,
        key: &[u8],
        epoch_hint: u16,
    ) -> Result<(), AudioError> {
        if key.len() != KEY_SIZE {
            return Err(AudioError::InvalidKeySize);
        }

        let mut key_arr = [0u8; KEY_SIZE];
        key_arr.copy_from_slice(key);

        let inner = self.inner.read().map_err(|_| AudioError::CryptoError)?;
        inner
            .add_sender(session_id, &key_arr, epoch_hint)
            .map_err(convert_error)?;
        Ok(())
    }

    /// Update a sender's key (called when MLS epoch advances)
    ///
    /// Old keys are retained for graceful epoch handover.
    pub fn update_sender_key(
        &self,
        session_id: u32,
        key: &[u8],
        epoch_hint: u16,
    ) -> Result<bool, AudioError> {
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

    /// Set local playback gain for a specific sender.
    /// `gain` is clamped to `[0.0, 4.0]`; 1.0 is "unchanged".
    pub fn set_sender_gain(&self, session_id: u32, gain: f32) {
        if let Ok(inner) = self.inner.read() {
            inner.set_sender_gain(session_id, gain);
        }
    }

    /// Mute or unmute a specific sender locally. The audio still decodes
    /// so the Opus state stays consistent, but the mixer drops the samples.
    pub fn set_sender_muted(&self, session_id: u32, muted: bool) {
        if let Ok(inner) = self.inner.read() {
            inner.set_sender_muted(session_id, muted);
        }
    }

    /// Process a received packet
    pub fn on_packet(&self, data: &[u8]) -> Result<(), AudioError> {
        let inner = self.inner.read().map_err(|_| AudioError::CryptoError)?;
        inner
            .on_packet(Bytes::copy_from_slice(data))
            .map_err(convert_error)?;
        Ok(())
    }

    /// Report a version 1 audio frame (datagram type 0x01 / stream message
    /// 0x20) without its type byte. It is dropped, and the first one from each
    /// sender logs that the peer must update.
    pub fn note_legacy_packet(&self, data: &[u8]) {
        if let Ok(inner) = self.inner.read() {
            inner.note_legacy_packet(data);
        }
    }

    /// Pop decoded frames
    pub fn pop_decoded(&self) -> Vec<DecodedFrame> {
        self.inner
            .read()
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
        self.inner
            .read()
            .ok()?
            .pop_mixed()
            .map(|mixed| MixedAudioResult {
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

/// Returns `true` if `aura-core` was compiled with WebRTC audio processing (`webrtc-audio` feature).
#[uniffi::export]
pub fn is_webrtc_audio_available() -> bool {
    cfg!(feature = "webrtc-audio")
}

/// Metadata for an available audio input or output device.
#[derive(Debug, Clone, uniffi::Record)]
pub struct AudioDeviceInfoRecord {
    pub name: String,
    pub is_default: bool,
}

/// List available audio input (microphone) devices.
#[uniffi::export]
pub fn list_input_audio_devices() -> Vec<AudioDeviceInfoRecord> {
    #[cfg(feature = "native-audio")]
    {
        crate::audio_io::list_input_devices()
            .unwrap_or_default()
            .into_iter()
            .map(|d| AudioDeviceInfoRecord {
                name: d.name,
                is_default: d.is_default,
            })
            .collect()
    }
    #[cfg(not(feature = "native-audio"))]
    {
        Vec::new()
    }
}

/// List available audio output (speaker) devices.
#[uniffi::export]
pub fn list_output_audio_devices() -> Vec<AudioDeviceInfoRecord> {
    #[cfg(feature = "native-audio")]
    {
        crate::audio_io::list_output_devices()
            .unwrap_or_default()
            .into_iter()
            .map(|d| AudioDeviceInfoRecord {
                name: d.name,
                is_default: d.is_default,
            })
            .collect()
    }
    #[cfg(not(feature = "native-audio"))]
    {
        Vec::new()
    }
}

#[cfg(feature = "native-audio")]
fn map_audio_io_error(e: crate::audio_io::AudioError) -> AudioError {
    match e {
        crate::audio_io::AudioError::NoDevice => {
            AudioError::DeviceNotFound("No default audio device found".into())
        }
        crate::audio_io::AudioError::Device(msg) if msg.contains("not found") => {
            AudioError::DeviceNotFound(msg)
        }
        other => AudioError::DeviceError(other.to_string()),
    }
}

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
        let device = AudioDevice::new().map_err(map_audio_io_error)?;
        Ok(Self {
            device: Mutex::new(device),
        })
    }

    #[uniffi::constructor]
    pub fn new_with_devices(
        input_device: Option<String>,
        output_device: Option<String>,
    ) -> Result<Self, AudioError> {
        let device =
            AudioDevice::new_with_devices(input_device.as_deref(), output_device.as_deref())
                .map_err(map_audio_io_error)?;
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
        device
            .send_playback(pcm)
            .map_err(|_| AudioError::OpusError)?;
        Ok(())
    }
}

// =============================================================================
// Text Crypto - UniFFI-compatible wrappers
// =============================================================================

use crate::text_crypto::{
    create_text_message, decrypt_text, encrypt_text, EncryptedTextPacket, TextMessage,
};

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
            attachment: None,
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
        encrypt_text(
            &self.dave_key,
            epoch,
            channel_id,
            sender_session_id,
            &text_msg,
        )
        .map(|p| p.into())
        .map_err(|_| TextCryptoError::EncryptionFailed)
    }

    /// Decrypt an encrypted text packet
    pub fn decrypt(
        &self,
        packet: EncryptedTextPacketRecord,
    ) -> Result<TextMessageRecord, TextCryptoError> {
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
    /// Stable per-user identifier. May be an empty string if the server
    /// hasn't resolved the session (transient ghost case).
    pub user_uuid: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UserProfileRecord {
    /// The account's server-assigned id (`U_…`), as carried on the wire. It
    /// was a `u32` once, which turned every id into 0 on decode and sent the
    /// session number on encode — and the server refuses a profile whose id
    /// is not the caller's account.
    pub user_id: String,
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
    /// Stable per-user identifier so clients can persist local-only
    /// state (volume / mute) across reconnects.
    pub user_uuid: String,
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

/// Why the server took the client out of a channel (`ChannelDeletedRecord`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ChannelRemovalReason {
    /// The channel was deleted.
    Deleted,
    /// The client asked to join a channel that doesn't exist; it is now in
    /// no channel.
    NotFound,
    /// An admin kicked the client out of the channel, which still exists.
    Kicked,
}

/// Notification (MSG_CHANNEL_DELETED, 0x47) that the client is no longer in
/// a channel and should move to `fallback_channel_id`: the channel was
/// deleted, the channel it tried to join doesn't exist, or it was kicked.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelDeletedRecord {
    pub channel_id: String,
    /// Channel to fall back to (the lobby, when the server has one). Empty
    /// when the server has no suggestion and the client should pick.
    pub fallback_channel_id: String,
    pub reason: ChannelRemovalReason,
    /// The admin's reason, for a kick. Display text; empty otherwise.
    pub detail: String,
}

/// Server's reply to a `CreateChannelRequest` (MSG_CREATE_CHANNEL, 0x40).
#[derive(Debug, Clone, uniffi::Record)]
pub struct CreateChannelResponseRecord {
    pub success: bool,
    pub channel_id: String,
    pub error_message: String,
}

/// Server's reply to `UpdateChannelRequest` (0x41) or `UpdateProfile` (0x42).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MetadataUpdateResponseRecord {
    pub success: bool,
    pub error_message: String,
}

/// Server's reply to `DeleteChannelRequest` (0x43), `DeleteUserRequest`
/// (0x44) and the moderation requests: `VerifyUserRequest` (0x49),
/// `BanUserRequest` (0x4A), `GrantAdminRequest` (0x4B) and
/// `KickUserRequest` (0x4C).
#[derive(Debug, Clone, uniffi::Record)]
pub struct AdminResponseRecord {
    pub success: bool,
    pub error_message: String,
}

/// Admin permissions, as set with [`encode_grant_admin_request`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct AdminPermissionsRecord {
    /// Verify users.
    pub verify_users: bool,
    /// Ban, unban and kick users, and delete their accounts.
    pub ban_users: bool,
    /// Set other users' admin permissions, and act on other admins.
    pub grant_admin: bool,
    /// Create, edit and delete channels.
    pub manage_channels: bool,
}

/// One account in a [`ListUsersResponseRecord`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct UserInfoRecord {
    pub user_uuid: String,
    pub display_name: String,
    pub verified: bool,
    pub banned: bool,
    pub is_admin: bool,
    /// Unix time (seconds) the account was created.
    pub created_at: i64,
}

/// Server's reply to `ListUsersRequest` (MSG_LIST_USERS, 0x48).
#[derive(Debug, Clone, uniffi::Record)]
pub struct ListUsersResponseRecord {
    /// `false` when the request was refused (see `error_message`).
    pub success: bool,
    pub error_message: String,
    /// This page of accounts, oldest first.
    pub users: Vec<UserInfoRecord>,
    /// Accounts on the server in all.
    pub total_count: u32,
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

    let channels = proto
        .channels
        .into_iter()
        .map(|c| {
            let icon = c.icon.and_then(|i| i.icon).map(|icon| match icon {
                channel_icon::Icon::Emoji(e) => ChannelIconRecord {
                    emoji: Some(e),
                    preset_id: None,
                    custom_data: None,
                },
                channel_icon::Icon::PresetId(p) => ChannelIconRecord {
                    emoji: None,
                    preset_id: Some(p),
                    custom_data: None,
                },
                channel_icon::Icon::CustomData(d) => ChannelIconRecord {
                    emoji: None,
                    preset_id: None,
                    custom_data: Some(d.to_vec()),
                },
            });

            ChannelInfoRecord {
                channel_id: c.channel_id,
                name: c.name,
                comment: c.comment,
                icon,
                position: c.position,
                user_ids: c.user_ids,
                users: c
                    .users
                    .into_iter()
                    .map(|u| ChannelUserStatusRecord {
                        session_id: u.session_id,
                        is_muted: u.is_muted,
                        is_deafened: u.is_deafened,
                        display_name: u.display_name,
                        user_uuid: u.user_uuid,
                    })
                    .collect(),
                channel_type: match c.r#type {
                    1 => ChannelType::Lobby,
                    _ => ChannelType::Regular,
                },
            }
        })
        .collect();

    let profiles = proto
        .profiles
        .into_iter()
        .map(|p| UserProfileRecord {
            user_id: p.user_id,
            display_name: p.display_name,
            bio: p.bio,
            avatar_data: p.avatar_data.to_vec(),
            signature: p.signature.to_vec(),
            signing_key: p.signing_key.to_vec(),
        })
        .collect();

    Ok(ServerStateRecord { channels, profiles })
}

#[uniffi::export]
pub fn encode_update_profile(profile: UserProfileRecord) -> Vec<u8> {
    use prost::Message;
    let proto_profile = ProtoProfile {
        user_id: profile.user_id,
        display_name: profile.display_name,
        bio: profile.bio,
        avatar_data: profile.avatar_data,
        signature: profile.signature,
        signing_key: profile.signing_key,
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
        user_id: proto.user_id,
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
    let proto =
        ProtoUserStatusUpdate::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    Ok(UserStatusUpdate {
        session_id: proto.session_id,
        is_muted: proto.is_muted,
        is_deafened: proto.is_deafened,
    })
}

#[uniffi::export]
pub fn decode_user_joined(data: Vec<u8>) -> Result<UserJoinedRecord, AudioError> {
    use prost::Message;
    let proto =
        aura_protocol::UserJoined::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    Ok(UserJoinedRecord {
        channel_id: proto.channel_id,
        session_id: proto.session_id,
        display_name: proto.display_name,
        user_uuid: proto.user_uuid,
    })
}

#[uniffi::export]
pub fn decode_user_left(data: Vec<u8>) -> Result<UserLeftRecord, AudioError> {
    use prost::Message;
    let proto =
        aura_protocol::UserLeft::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;
    Ok(UserLeftRecord {
        channel_id: proto.channel_id,
        session_id: proto.session_id,
    })
}

/// `MlsSyncMembers` (0x58): the account ids the server says should still be
/// in a channel's group. Pass `member_uuids` to
/// [`MlsWrapper::remove_members_except`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct MlsSyncMembersRecord {
    pub channel_id: String,
    pub is_voice: bool,
    pub member_uuids: Vec<String>,
}

#[uniffi::export]
pub fn decode_mls_sync_members(data: Vec<u8>) -> Result<MlsSyncMembersRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::MlsSyncMembers::decode(&data[..])
        .map_err(|_| AudioError::PacketParseError)?;
    Ok(MlsSyncMembersRecord {
        is_voice: proto.group_type() == aura_protocol::MlsGroupType::Voice,
        channel_id: proto.channel_id,
        member_uuids: proto.member_uuids,
    })
}

#[uniffi::export]
pub fn decode_channel_deleted(data: Vec<u8>) -> Result<ChannelDeletedRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::ChannelDeleted::decode(&data[..])
        .map_err(|_| AudioError::PacketParseError)?;
    let reason = match proto.reason() {
        aura_protocol::ChannelRemovalReason::Deleted => ChannelRemovalReason::Deleted,
        aura_protocol::ChannelRemovalReason::NotFound => ChannelRemovalReason::NotFound,
        aura_protocol::ChannelRemovalReason::Kicked => ChannelRemovalReason::Kicked,
    };
    Ok(ChannelDeletedRecord {
        channel_id: proto.channel_id,
        fallback_channel_id: proto.fallback_channel_id,
        reason,
        detail: proto.detail,
    })
}

#[uniffi::export]
pub fn decode_create_channel_response(
    data: Vec<u8>,
) -> Result<CreateChannelResponseRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::CreateChannelResponse::decode(&data[..])
        .map_err(|_| AudioError::PacketParseError)?;
    Ok(CreateChannelResponseRecord {
        success: proto.success,
        channel_id: proto.channel_id,
        error_message: proto.error_message,
    })
}

#[uniffi::export]
pub fn decode_metadata_update_response(
    data: Vec<u8>,
) -> Result<MetadataUpdateResponseRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::MetadataUpdateResponse::decode(&data[..])
        .map_err(|_| AudioError::PacketParseError)?;
    Ok(MetadataUpdateResponseRecord {
        success: proto.success,
        error_message: proto.error_message,
    })
}

#[uniffi::export]
pub fn decode_admin_response(data: Vec<u8>) -> Result<AdminResponseRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::AdminResponse::decode(&data[..])
        .map_err(|_| AudioError::PacketParseError)?;
    Ok(AdminResponseRecord {
        success: proto.success,
        error_message: proto.error_message,
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
pub fn decode_encrypted_text_packet(
    data: Vec<u8>,
) -> Result<EncryptedTextPacketRecord, AudioError> {
    use prost::Message;
    let proto = aura_protocol::EncryptedTextPacket::decode(&data[..])
        .map_err(|_| AudioError::PacketParseError)?;
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
        ciphertext: packet.ciphertext,
        nonce: packet.nonce,
        tag: packet.tag,
        reply_to_id: packet.reply_to_id,
    };
    proto.encode_to_vec()
}

#[uniffi::export]
pub fn decode_mls_envelope(data: Vec<u8>) -> Result<MlsEnvelopeRecord, AudioError> {
    use prost::Message;
    let proto =
        aura_protocol::MlsEnvelope::decode(&data[..]).map_err(|_| AudioError::PacketParseError)?;

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
        Some(aura_protocol::mls_envelope::Content::CommitWelcome(
            aura_protocol::MlsCommitWelcome {
                commit: cw.commit,
                welcome: cw.welcome,
                new_member_session_id: cw.new_member_session_id,
            },
        ))
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
    /// The message was encrypted under an epoch whose keys are no longer
    /// retained. Callers should drop the message rather than retry.
    #[error("No retained key for epoch {epoch} (current epoch is {current})")]
    EpochUnavailable { epoch: u64, current: u64 },
}

impl From<InternalMlsError> for MlsError {
    fn from(e: InternalMlsError) -> Self {
        match e {
            InternalMlsError::GroupNotFound(_msg) => MlsError::GroupNotFound,
            InternalMlsError::EpochUnavailable { epoch, current } => {
                MlsError::EpochUnavailable { epoch, current }
            }
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
    /// Create the MLS client for `user_id`, the account's server-assigned id
    /// (`user_uuid` from the login response).
    ///
    /// It signs with the MLS key derived from `identity_secret` (64 bytes; see
    /// `generate_identity_secret`), the same key on every connection, and
    /// presents a credential in which that identity certifies the key.
    #[uniffi::constructor]
    pub fn new(user_id: String, identity_secret: Vec<u8>) -> Result<Self, MlsError> {
        let identity = aura_identity::IdentitySecret::from_bytes(&identity_secret)
            .map_err(|e| MlsError::OperationFailed(format!("identity secret: {e}")))?;
        let client = MlsClient::new(&user_id, &identity)?;
        Ok(Self {
            inner: Mutex::new(client),
        })
    }

    /// Generate a key package to send to the server
    ///
    /// Returns serialized KeyPackage bytes that can be sent to server
    pub fn create_key_package(&self) -> Result<Vec<u8>, MlsError> {
        let client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        client.get_key_package_bytes().map_err(Into::into)
    }

    /// Create a new MLS group (as the first member)
    ///
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `is_voice` - true for voice group, false for text group
    pub fn create_group(&self, channel_id: String, is_voice: bool) -> Result<(), MlsError> {
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
        client.create_group(&group_id).map_err(Into::into)
    }

    /// Join a group via a Welcome message from the server
    ///
    /// # Arguments
    /// * `welcome_bytes` - Serialized Welcome message
    pub fn join_group(&self, welcome_bytes: Vec<u8>) -> Result<(), MlsError> {
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
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
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
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
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
        client
            .process_commit(&group_id, &commit_bytes)
            .map_err(Into::into)
    }

    /// Issue an empty commit to advance a group's epoch (PFS ratchet).
    ///
    /// Returns the serialized Commit for the caller to hand to the server for
    /// distribution to the rest of the group.
    pub fn self_update(&self, channel_id: String, is_voice: bool) -> Result<Vec<u8>, MlsError> {
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
        client.self_update(&group_id).map_err(Into::into)
    }

    /// Answer the server's `MlsSyncMembers` (0x58): commit the removal of every
    /// member whose user id is not in `keep_user_ids`.
    ///
    /// Returns the Commit to send back as `MSG_MLS_RATCHET_COMMIT` (0x57), or
    /// `None` when the group already matches and there is nothing to send.
    pub fn remove_members_except(
        &self,
        channel_id: String,
        is_voice: bool,
        keep_user_ids: Vec<String>,
    ) -> Result<Option<Vec<u8>>, MlsError> {
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
        client
            .remove_members_except(&group_id, &keep_user_ids)
            .map_err(Into::into)
    }

    /// Drop our local state for a channel's group when we leave the channel.
    /// Sends nothing. A no-op if we were never in the group.
    pub fn forget_group(&self, channel_id: String, is_voice: bool) -> Result<(), MlsError> {
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();
        client.forget_group(&group_id);
        Ok(())
    }

    /// Export encryption key for audio (voice group)
    ///
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `sender_session_id` - Session ID of the audio sender
    ///
    /// # Returns
    /// 32-byte encryption key for this sender
    pub fn export_audio_key(
        &self,
        channel_id: String,
        sender_session_id: u32,
    ) -> Result<Vec<u8>, MlsError> {
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, true).into_bytes();
        let (key, _epoch) = client.export_sender_key(&group_id, sender_session_id)?;
        Ok(key.to_vec())
    }

    /// Export encryption key for text (text group)
    ///
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `sender_session_id` - Session ID of the text sender
    /// * `epoch` - Epoch the message was encrypted under. Pass the value from
    ///   the received packet; `None` uses the group's current epoch (correct
    ///   for the send path only).
    ///
    /// # Returns
    /// 32-byte encryption key for this sender
    pub fn export_text_key(
        &self,
        channel_id: String,
        sender_session_id: u32,
        epoch: Option<u64>,
    ) -> Result<Vec<u8>, MlsError> {
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, false).into_bytes();

        match epoch {
            Some(epoch) => client
                .export_sender_key_at_epoch(&group_id, sender_session_id, epoch)
                .map(|key| key.to_vec())
                .map_err(Into::into),
            None => {
                let (key, _epoch) = client.export_sender_key(&group_id, sender_session_id)?;
                Ok(key.to_vec())
            }
        }
    }

    /// Pre-derive and retain a member's text key at the current epoch.
    ///
    /// Call this when a member becomes visible in a channel. It guarantees a
    /// retained key exists for them, so their first message stays decryptable
    /// even if it races a membership change.
    pub fn register_text_sender(
        &self,
        channel_id: String,
        sender_session_id: u32,
    ) -> Result<(), MlsError> {
        let mut client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, false).into_bytes();
        client
            .register_sender(&group_id, sender_session_id)
            .map_err(Into::into)
    }

    /// Get current epoch for a group
    ///
    /// # Arguments
    /// * `channel_id` - Numeric channel ID
    /// * `is_voice` - true for voice group, false for text group
    pub fn current_epoch(&self, channel_id: String, is_voice: bool) -> Result<u64, MlsError> {
        let client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
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

    /// Every member of a group with their MLS-vouched signature key.
    ///
    /// The only trustworthy source of other users' identity keys: they come
    /// from the authenticated ratchet tree, not from a server-supplied list.
    /// Feed these into `derive_pairwise_verification` to build a trust sheet.
    pub fn group_members(
        &self,
        channel_id: String,
        is_voice: bool,
    ) -> Result<Vec<MlsGroupMemberRecord>, MlsError> {
        let client = self
            .inner
            .lock()
            .map_err(|_| MlsError::OperationFailed("Lock poisoned".into()))?;
        let group_id = aura_protocol::make_mls_group_id(&channel_id, is_voice).into_bytes();

        Ok(client
            .group_members(&group_id)?
            .into_iter()
            .map(|m| MlsGroupMemberRecord {
                identity: m.identity,
                identity_key: m.identity_key,
                is_self: m.is_self,
            })
            .collect())
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
        receiver
            .add_sender(session_id, &key, 0)
            .expect("Add sender");

        let pcm = vec![1000i16; 960];
        let packet = sender
            .process(&pcm)
            .expect("Process")
            .expect("VAD off; expected packet");

        receiver.on_packet(&packet).expect("On packet");

        let decoded = receiver.pop_decoded();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].session_id, session_id);
    }

    #[test]
    fn moderation_requests_encode_their_fields() {
        let req = aura_protocol::ListUsersRequest::decode(&encode_list_users_request(20, 10)[..])
            .unwrap();
        assert_eq!((req.offset, req.limit), (20, 10));
        assert!(req.session_token.is_empty());

        let req =
            aura_protocol::VerifyUserRequest::decode(&encode_verify_user_request("U_a".into())[..])
                .unwrap();
        assert_eq!(req.target_user_uuid, "U_a");

        let req = aura_protocol::BanUserRequest::decode(
            &encode_ban_user_request("U_b".into(), true, "spam".into())[..],
        )
        .unwrap();
        assert_eq!(
            (
                req.target_user_uuid.as_str(),
                req.banned,
                req.reason.as_str()
            ),
            ("U_b", true, "spam")
        );

        let req = aura_protocol::GrantAdminRequest::decode(
            &encode_grant_admin_request(
                "U_c".into(),
                AdminPermissionsRecord {
                    verify_users: true,
                    ban_users: false,
                    grant_admin: false,
                    manage_channels: true,
                },
            )[..],
        )
        .unwrap();
        assert_eq!(req.target_user_uuid, "U_c");
        assert!(req.verify_users && req.manage_channels);
        assert!(!req.ban_users && !req.grant_admin);

        let req = aura_protocol::KickUserRequest::decode(
            &encode_kick_user_request("U_d".into(), "be nice".into())[..],
        )
        .unwrap();
        assert_eq!(
            (req.target_user_uuid.as_str(), req.reason.as_str()),
            ("U_d", "be nice")
        );
    }

    #[test]
    fn list_users_response_decodes() {
        let bytes = aura_protocol::ListUsersResponse {
            users: vec![aura_protocol::UserInfo {
                user_uuid: "U_a".into(),
                display_name: "Alice".into(),
                verified: true,
                banned: false,
                is_admin: true,
                created_at: 1_700_000_000,
            }],
            total_count: 7,
            success: true,
            error_message: String::new(),
        }
        .encode_to_vec();

        let resp = decode_list_users_response(bytes).unwrap();
        assert!(resp.success);
        assert_eq!(resp.total_count, 7);
        assert_eq!(resp.users.len(), 1);
        let alice = &resp.users[0];
        assert_eq!(
            (alice.user_uuid.as_str(), alice.display_name.as_str()),
            ("U_a", "Alice")
        );
        assert!(alice.verified && alice.is_admin && !alice.banned);
        assert_eq!(alice.created_at, 1_700_000_000);
    }

    #[test]
    fn channel_deleted_carries_the_removal_reason() {
        let kicked = aura_protocol::ChannelDeleted {
            channel_id: "C_1".into(),
            fallback_channel_id: "C_lobby".into(),
            reason: aura_protocol::ChannelRemovalReason::Kicked as i32,
            detail: "be nice".into(),
        }
        .encode_to_vec();
        let notice = decode_channel_deleted(kicked).unwrap();
        assert_eq!(notice.reason, ChannelRemovalReason::Kicked);
        assert_eq!(notice.detail, "be nice");
        assert_eq!(notice.fallback_channel_id, "C_lobby");

        // A notice from a server that predates `reason` is a deletion.
        let legacy = aura_protocol::ChannelDeleted {
            channel_id: "C_1".into(),
            fallback_channel_id: String::new(),
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            decode_channel_deleted(legacy).unwrap().reason,
            ChannelRemovalReason::Deleted
        );
    }
}
#[uniffi::export]
pub fn encode_create_channel(
    name: String,
    comment: String,
    icon: Option<ChannelIconRecord>,
) -> Vec<u8> {
    let proto_icon = icon.map(|i| ProtoIcon {
        icon: i
            .emoji
            .map(channel_icon::Icon::Emoji)
            .or_else(|| i.preset_id.map(channel_icon::Icon::PresetId))
            .or_else(|| i.custom_data.map(channel_icon::Icon::CustomData)),
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
        icon: i
            .emoji
            .map(channel_icon::Icon::Emoji)
            .or_else(|| i.preset_id.map(channel_icon::Icon::PresetId))
            .or_else(|| i.custom_data.map(channel_icon::Icon::CustomData)),
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

/// Payload of `DeleteUserRequest` (0x44). The server honours it for the
/// caller's own account, or any account when the caller is an admin, and
/// answers with an `AdminResponse` (see [`decode_admin_response`]).
#[uniffi::export]
pub fn encode_delete_user_request(user_uuid: String) -> Vec<u8> {
    aura_protocol::DeleteUserRequest { user_uuid }.encode_to_vec()
}

// ============================================================================
// Moderation (MSG 0x48-0x4C)
//
// The server authorises these with the account the connection logged in as;
// the requests' legacy `session_token` fields stay empty. Every request but
// ListUsers is answered with an `AdminResponse` (see
// [`decode_admin_response`]) on the request's own message type.
// ============================================================================

/// Payload of `ListUsersRequest` (0x48), answered with a `ListUsersResponse`
/// (see [`decode_list_users_response`]). Any admin may list users. A `limit`
/// of 0 asks for the server's default page size (100); the server caps it
/// at 500.
#[uniffi::export]
pub fn encode_list_users_request(offset: u32, limit: u32) -> Vec<u8> {
    aura_protocol::ListUsersRequest {
        session_token: String::new(),
        offset,
        limit,
    }
    .encode_to_vec()
}

/// Decode the server's reply to `ListUsersRequest` (0x48).
#[uniffi::export]
pub fn decode_list_users_response(data: Vec<u8>) -> Result<ListUsersResponseRecord, AudioError> {
    let proto = aura_protocol::ListUsersResponse::decode(&data[..])
        .map_err(|_| AudioError::PacketParseError)?;
    Ok(ListUsersResponseRecord {
        success: proto.success,
        error_message: proto.error_message,
        users: proto
            .users
            .into_iter()
            .map(|u| UserInfoRecord {
                user_uuid: u.user_uuid,
                display_name: u.display_name,
                verified: u.verified,
                banned: u.banned,
                is_admin: u.is_admin,
                created_at: u.created_at,
            })
            .collect(),
        total_count: proto.total_count,
    })
}

/// Payload of `VerifyUserRequest` (0x49). Needs `verify_users`.
#[uniffi::export]
pub fn encode_verify_user_request(target_user_uuid: String) -> Vec<u8> {
    aura_protocol::VerifyUserRequest {
        session_token: String::new(),
        target_user_uuid,
    }
    .encode_to_vec()
}

/// Payload of `BanUserRequest` (0x4A): `banned` true bans, false unbans.
/// Needs `ban_users`. A ban closes the target's connections at once.
#[uniffi::export]
pub fn encode_ban_user_request(target_user_uuid: String, banned: bool, reason: String) -> Vec<u8> {
    aura_protocol::BanUserRequest {
        session_token: String::new(),
        target_user_uuid,
        banned,
        reason,
    }
    .encode_to_vec()
}

/// Payload of `GrantAdminRequest` (0x4B). Needs `grant_admin`. The
/// permissions replace the target's; granting none revokes admin status.
#[uniffi::export]
pub fn encode_grant_admin_request(
    target_user_uuid: String,
    permissions: AdminPermissionsRecord,
) -> Vec<u8> {
    aura_protocol::GrantAdminRequest {
        session_token: String::new(),
        target_user_uuid,
        verify_users: permissions.verify_users,
        ban_users: permissions.ban_users,
        grant_admin: permissions.grant_admin,
        manage_channels: permissions.manage_channels,
    }
    .encode_to_vec()
}

/// Payload of `KickUserRequest` (0x4C). Needs `ban_users`. The target is
/// moved out of their channel and sent a `ChannelDeleted` notice with reason
/// [`ChannelRemovalReason::Kicked`] and `reason` as its detail.
#[uniffi::export]
pub fn encode_kick_user_request(target_user_uuid: String, reason: String) -> Vec<u8> {
    aura_protocol::KickUserRequest {
        target_user_uuid,
        reason,
    }
    .encode_to_vec()
}

// ============================================================================
// Identity verification — safety words and fingerprints
//
// See `crate::verification` for the construction and `docs/protocol.md`
// § Verification Fingerprint for the spec it follows.
// ============================================================================

/// Verification error surfaced to the clients.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum VerificationError {
    #[error("identity public key must be 1984 bytes, got {got}")]
    BadPublicKeyLength { got: u32 },
    #[error("user identifier must not be empty")]
    EmptyIdentifier,
    #[error("key derivation failed: {message}")]
    Kdf { message: String },
}

impl From<crate::verification::VerificationError> for VerificationError {
    fn from(e: crate::verification::VerificationError) -> Self {
        use crate::verification::VerificationError as Inner;
        match e {
            Inner::BadPublicKeyLength { got, .. } => {
                VerificationError::BadPublicKeyLength { got: got as u32 }
            }
            Inner::EmptyIdentifier => VerificationError::EmptyIdentifier,
            Inner::Kdf(message) => VerificationError::Kdf { message },
        }
    }
}

/// One member of an MLS group, with the key material MLS itself vouches for.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MlsGroupMemberRecord {
    /// The member's user id (`U_…`), from their verified credential.
    pub identity: String,
    /// The member's long-term identity public key (hybrid Ed25519 +
    /// ML-DSA-65, 1,984 bytes), verified to have certified the MLS key they
    /// sign with. Stable across connections; trust decisions key on it.
    pub identity_key: Vec<u8>,
    /// True for the local user's own leaf.
    pub is_self: bool,
}

/// Everything the trust sheet renders for one pair of identities.
///
/// Returned as a unit because deriving it costs an scrypt run (~100 ms); the
/// UI should make this call once when the sheet opens, not once per field.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PairwiseVerificationRecord {
    /// Six BIP-39 words the two users read to each other.
    pub safety_words: Vec<String>,
    /// Local identity fingerprint, grouped hex.
    pub local_fingerprint: String,
    /// Remote identity fingerprint, grouped hex.
    pub remote_fingerprint: String,
}

/// Derive the safety words and fingerprint pair for two identities.
///
/// Both devices produce identical output regardless of which side calls it —
/// that symmetry is what makes comparing the words meaningful.
///
/// Expensive (scrypt, N=16384). Call on a background thread.
#[uniffi::export]
pub fn derive_pairwise_verification(
    local_public_key: Vec<u8>,
    local_id: String,
    remote_public_key: Vec<u8>,
    remote_id: String,
) -> Result<PairwiseVerificationRecord, VerificationError> {
    use crate::verification as v;

    let pairwise =
        v::pairwise_fingerprint(&local_public_key, &local_id, &remote_public_key, &remote_id)?;

    Ok(PairwiseVerificationRecord {
        safety_words: v::safety_words(&pairwise).to_vec(),
        local_fingerprint: v::format_fingerprint(&v::key_fingerprint(&local_public_key)?),
        remote_fingerprint: v::format_fingerprint(&v::key_fingerprint(&remote_public_key)?),
    })
}

/// Per-key identity fingerprint as grouped hex: `3f8a 91c2 be04 77dd …`.
///
/// Cheap — a single SHA-256. Safe to call per roster row. This identifies a
/// key; it does not prove anything on its own. Comparison out of band must use
/// [`derive_pairwise_verification`].
#[uniffi::export]
pub fn identity_fingerprint(public_key: Vec<u8>) -> Result<String, VerificationError> {
    let fp = crate::verification::key_fingerprint(&public_key)?;
    Ok(crate::verification::format_fingerprint(&fp))
}

/// Head-and-tail identity fingerprint for tight UI: `3f8a…5503`.
///
/// A display truncation, never a trust decision.
#[uniffi::export]
pub fn identity_fingerprint_short(public_key: Vec<u8>) -> Result<String, VerificationError> {
    let fp = crate::verification::key_fingerprint(&public_key)?;
    Ok(crate::verification::format_fingerprint_short(&fp))
}
