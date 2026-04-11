//! Voice Session - Ties together MLS, Audio Pipeline, and Encryption
//!
//! Manages the complete E2EE voice session for a channel, handling:
//! - MLS group membership and key derivation
//! - Audio send/receive with automatic key rotation
//! - Epoch advancement when members join/leave

use bytes::Bytes;
use std::sync::RwLock;

use crate::audio_pipeline::{AudioSender, AudioReceiver, AudioPipelineError};
use crate::mls::{MlsClient, MlsError};

/// Voice session combining MLS E2EE with audio pipeline
pub struct VoiceSession {
    /// MLS client for key management
    mls: RwLock<MlsClient>,
    /// Audio sender (our outgoing audio)
    sender: AudioSender,
    /// Audio receiver (incoming audio from others)
    receiver: AudioReceiver,
    /// Current channel/group ID
    group_id: RwLock<Option<Vec<u8>>>,
    /// Our session ID (assigned by server)
    session_id: u32,
    /// Our identity name
    _identity: String,
}

/// Voice session errors
#[derive(Debug, thiserror::Error)]
pub enum VoiceSessionError {
    #[error("MLS error: {0}")]
    Mls(#[from] MlsError),
    
    #[error("Audio pipeline error: {0}")]
    Audio(#[from] AudioPipelineError),
    
    #[error("Not in a channel")]
    NotInChannel,
    
    #[error("Already in a channel")]
    AlreadyInChannel,
}

impl VoiceSession {
    /// Create a new voice session
    /// 
    /// # Arguments
    /// * `identity` - User identity string (e.g., display name)
    /// * `session_id` - Session ID assigned by server
    pub fn new(identity: &str, session_id: u32) -> Result<Self, VoiceSessionError> {
        let mls = MlsClient::new(identity)?;
        
        // Initialize sender with a placeholder key (will be set on join)
        let placeholder_key = [0u8; 32];
        let sender = AudioSender::new(session_id, &placeholder_key)?;
        let receiver = AudioReceiver::new();
        
        Ok(Self {
            mls: RwLock::new(mls),
            sender,
            receiver,
            group_id: RwLock::new(None),
            session_id,
            _identity: identity.to_string(),
        })
    }
    
    /// Create a new channel (we become the first member)
    /// 
    /// Returns a serialized KeyPackage to share with the server
    pub fn create_channel(&self, channel_id: &[u8]) -> Result<Vec<u8>, VoiceSessionError> {
        let mut mls = self.mls.write().unwrap();
        
        // Check we're not already in a channel
        if self.group_id.read().unwrap().is_some() {
            return Err(VoiceSessionError::AlreadyInChannel);
        }
        
        // Create the MLS group
        mls.create_group(channel_id)?;
        
        // Get the DAVE key for audio encryption (per-sender derivation with our session_id)
        let (dave_key, epoch) = mls.export_sender_key(channel_id, self.session_id)?;
        
        // Set up audio encryption
        self.sender.update_key(&dave_key, epoch);
        
        // Store group ID
        *self.group_id.write().unwrap() = Some(channel_id.to_vec());
        
        // Return our KeyPackage for the server
        Ok(mls.get_key_package_bytes()?)
    }
    
    /// Join an existing channel via Welcome message
    /// 
    /// # Arguments
    /// * `welcome_bytes` - Serialized MLS Welcome from channel creator
    pub fn join_channel(&self, welcome_bytes: &[u8]) -> Result<(), VoiceSessionError> {
        let mut mls = self.mls.write().unwrap();
        
        // Check we're not already in a channel
        if self.group_id.read().unwrap().is_some() {
            return Err(VoiceSessionError::AlreadyInChannel);
        }
        
        // Process the Welcome and join the group
        let group_id = mls.process_welcome(welcome_bytes)?;
        
        // Get the DAVE key for audio encryption (per-sender derivation with our session_id)
        let (dave_key, epoch) = mls.export_sender_key(&group_id, self.session_id)?;
        
        // Set up audio encryption
        self.sender.update_key(&dave_key, epoch);
        
        // Store group ID
        *self.group_id.write().unwrap() = Some(group_id);
        
        Ok(())
    }
    
    /// Add a member to the channel (we must be in the channel)
    /// 
    /// # Arguments
    /// * `key_package_bytes` - Serialized KeyPackage from the new member
    /// 
    /// # Returns
    /// * `(commit_bytes, welcome_bytes)` - Send commit to group, welcome to new member
    pub fn add_member(&self, key_package_bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>), VoiceSessionError> {
        let mut mls = self.mls.write().unwrap();
        let group_id = self.group_id.read().unwrap()
            .clone()
            .ok_or(VoiceSessionError::NotInChannel)?;
        
        // Add the member
        let (commit, welcome) = mls.add_member(&group_id, key_package_bytes)?;
        
        // Re-key our sender with the new epoch's key (per-sender derivation)
        let (dave_key, epoch) = mls.export_sender_key(&group_id, self.session_id)?;
        self.sender.update_key(&dave_key, epoch);
        
        Ok((commit, welcome))
    }
    
    /// Process a Commit message (epoch advancement)
    /// 
    /// Called when another member joins/leaves
    pub fn process_commit(&self, commit_bytes: &[u8]) -> Result<u64, VoiceSessionError> {
        let mut mls = self.mls.write().unwrap();
        let group_id = self.group_id.read().unwrap()
            .clone()
            .ok_or(VoiceSessionError::NotInChannel)?;
        
        // Process the commit
        let new_epoch = mls.process_commit(&group_id, commit_bytes)?;
        
        // Re-key our sender with the new epoch's key (per-sender derivation)
        let (dave_key, _) = mls.export_sender_key(&group_id, self.session_id)?;
        self.sender.update_key(&dave_key, new_epoch);
        
        Ok(new_epoch)
    }
    
    /// Leave the current channel
    pub fn leave_channel(&self) -> Result<Vec<u8>, VoiceSessionError> {
        let mut mls = self.mls.write().unwrap();
        let group_id = self.group_id.write().unwrap()
            .take()
            .ok_or(VoiceSessionError::NotInChannel)?;
        
        // Leave the MLS group
        let leave_proposal = mls.leave_group(&group_id)?;
        
        Ok(leave_proposal)
    }
    
    /// Register a remote sender for audio reception
    /// 
    /// Called when another member joins the channel
    pub fn add_remote_sender(&self, remote_session_id: u32) -> Result<(), VoiceSessionError> {
        let mls = self.mls.read().unwrap();
        let group_id = self.group_id.read().unwrap()
            .clone()
            .ok_or(VoiceSessionError::NotInChannel)?;
        
        // Per-sender key derivation using session_id as context
        let (dave_key, epoch) = mls.export_sender_key(&group_id, remote_session_id)?;
        
        self.receiver.add_sender(remote_session_id, &dave_key, (epoch & 0xFFFF) as u16)?;
        Ok(())
    }
    
    /// Update all remote senders' keys (after epoch advance)
    pub fn update_remote_keys(&self, remote_session_ids: &[u32]) -> Result<(), VoiceSessionError> {
        let mls = self.mls.read().unwrap();
        let group_id = self.group_id.read().unwrap()
            .clone()
            .ok_or(VoiceSessionError::NotInChannel)?;
        
        for &session_id in remote_session_ids {
            // Per-sender key derivation
            let (dave_key, epoch) = mls.export_sender_key(&group_id, session_id)?;
            self.receiver.update_sender_key(session_id, &dave_key, (epoch & 0xFFFF) as u16);
        }
        
        Ok(())
    }
    
    /// Remove a remote sender
    pub fn remove_remote_sender(&self, remote_session_id: u32) {
        self.receiver.remove_sender(remote_session_id);
    }
    
    // =========================================================================
    // Audio Processing
    // =========================================================================
    
    /// Process outgoing audio (PCM → encrypted packet)
    /// 
    /// # Arguments
    /// * `pcm` - 960 samples of 16-bit mono PCM (20ms at 48kHz)
    /// 
    /// # Returns
    /// Serialized FastAudioPacket ready for QUIC datagram
    pub fn process_audio(&self, pcm: &[i16]) -> Result<Bytes, VoiceSessionError> {
        if self.group_id.read().unwrap().is_none() {
            return Err(VoiceSessionError::NotInChannel);
        }
        
        Ok(self.sender.process(pcm)?)
    }
    
    /// Process outgoing audio (f32 PCM → encrypted packet)
    pub fn process_audio_float(&self, pcm: &[f32]) -> Result<Bytes, VoiceSessionError> {
        if self.group_id.read().unwrap().is_none() {
            return Err(VoiceSessionError::NotInChannel);
        }
        
        Ok(self.sender.process_float_with_reference(pcm, None)?)
    }
    
    /// Receive incoming audio packet
    /// 
    /// Packet is decrypted and added to the jitter buffer
    pub fn receive_audio(&self, packet: Bytes) -> Result<(), VoiceSessionError> {
        Ok(self.receiver.on_packet(packet)?)
    }
    
    /// Pop mixed audio for playback
    /// 
    /// Returns mixed PCM from all active senders
    pub fn pop_playback(&self) -> Option<Vec<i16>> {
        self.receiver.pop_mixed().map(|mixed| mixed.pcm)
    }
    
    /// Pop individual decoded frames
    /// 
    /// Returns Vec of (session_id, PCM samples) for each active sender
    pub fn pop_decoded(&self) -> Vec<(u32, Vec<i16>)> {
        self.receiver.pop_decoded()
    }
    
    // =========================================================================
    // Getters
    // =========================================================================
    
    /// Get our session ID
    pub fn session_id(&self) -> u32 {
        self.session_id
    }
    
    /// Check if we're in a channel
    pub fn is_in_channel(&self) -> bool {
        self.group_id.read().unwrap().is_some()
    }
    
    /// Get current MLS epoch
    pub fn current_epoch(&self) -> Option<u64> {
        let mls = self.mls.read().unwrap();
        let group_id = self.group_id.read().unwrap();
        group_id.as_ref().and_then(|gid| mls.epoch(gid).ok())
    }
    
    /// Get serialized KeyPackage for sharing
    pub fn get_key_package(&self) -> Result<Vec<u8>, VoiceSessionError> {
        let mls = self.mls.read().unwrap();
        Ok(mls.get_key_package_bytes()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_create_voice_session() {
        let session = VoiceSession::new("alice", 1).expect("Create session");
        assert_eq!(session.session_id(), 1);
        assert!(!session.is_in_channel());
    }
    
    #[test]
    fn test_create_and_join_channel() {
        // Alice creates channel
        let alice = VoiceSession::new("alice", 1).expect("Create alice");
        let _alice_kp = alice.create_channel(b"test-channel").expect("Create channel");
        assert!(alice.is_in_channel());
        assert_eq!(alice.current_epoch(), Some(0));
        
        // Bob prepares to join
        let bob = VoiceSession::new("bob", 2).expect("Create bob");
        let bob_kp = bob.get_key_package().expect("Get bob KP");
        
        // Alice adds Bob
        let (commit, welcome) = alice.add_member(&bob_kp).expect("Add bob");
        assert!(!commit.is_empty());
        assert!(!welcome.is_empty());
        
        // Alice's epoch advanced
        assert_eq!(alice.current_epoch(), Some(1));
        
        // Bob joins via Welcome
        bob.join_channel(&welcome).expect("Bob join");
        assert!(bob.is_in_channel());
        assert_eq!(bob.current_epoch(), Some(1));
    }
    
    #[test]
    fn test_audio_roundtrip() {
        // Setup: Alice and Bob in same channel
        let alice = VoiceSession::new("alice", 1).expect("Create alice");
        alice.create_channel(b"audio-test").expect("Create channel");
        
        let bob = VoiceSession::new("bob", 2).expect("Create bob");
        let bob_kp = bob.get_key_package().expect("Get bob KP");
        let (_, welcome) = alice.add_member(&bob_kp).expect("Add bob");
        bob.join_channel(&welcome).expect("Bob join");
        
        // Register each other as remote senders
        alice.add_remote_sender(2).expect("Alice add bob");
        bob.add_remote_sender(1).expect("Bob add alice");
        
        // Alice sends audio
        let pcm = vec![1000i16; 960];
        let packet = alice.process_audio(&pcm).expect("Alice send");
        
        // Bob receives
        bob.receive_audio(packet).expect("Bob receive");
        
        // Bob pops playback
        let playback = bob.pop_playback();
        assert!(playback.is_some());
        assert_eq!(playback.unwrap().len(), 960);
    }
    
    #[test]
    fn test_epoch_key_rotation() {
        // Setup: Alice creates channel
        let alice = VoiceSession::new("alice", 1).expect("Create alice");
        alice.create_channel(b"rotation-test").expect("Create channel");
        assert_eq!(alice.current_epoch(), Some(0));
        
        // Add Bob - epoch advances to 1
        let bob = VoiceSession::new("bob", 2).expect("Create bob");
        let bob_kp = bob.get_key_package().expect("Get bob KP");
        let (_commit1, welcome1) = alice.add_member(&bob_kp).expect("Add bob");
        bob.join_channel(&welcome1).expect("Bob join");
        
        // Both at epoch 1
        assert_eq!(alice.current_epoch(), Some(1));
        assert_eq!(bob.current_epoch(), Some(1));
        
        // Add Charlie - epoch advances to 2
        let charlie = VoiceSession::new("charlie", 3).expect("Create charlie");
        let charlie_kp = charlie.get_key_package().expect("Get charlie KP");
        let (commit2, welcome2) = alice.add_member(&charlie_kp).expect("Add charlie");
        charlie.join_channel(&welcome2).expect("Charlie join");
        
        // Bob processes commit
        bob.process_commit(&commit2).expect("Bob process commit");
        
        // All at epoch 2
        assert_eq!(alice.current_epoch(), Some(2));
        assert_eq!(bob.current_epoch(), Some(2));
        assert_eq!(charlie.current_epoch(), Some(2));
    }
}
