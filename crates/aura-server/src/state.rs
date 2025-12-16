//! Server state module.
//!
//! Extracts `ServerState` from main.rs and adds persistence layer.

use crate::auth::AuthService;
use crate::config::{Config, VerificationMode};
use crate::db::{Database, User};
use aura_protocol::{
    FastAudioPacket, UserProfile, MlsEnvelope, MlsGroupType, mls_envelope, EncryptedTextPacket,
};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use dashmap::{DashMap, DashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

// --- Constants ---

/// Voice group type identifier (matches proto enum)
pub const MLS_GROUP_TYPE_VOICE: i32 = MlsGroupType::Voice as i32;
/// Text group type identifier (matches proto enum)
pub const MLS_GROUP_TYPE_TEXT: i32 = MlsGroupType::Text as i32;

/// Response to MLS signaling operations
#[derive(Debug, Clone)]
pub struct MlsSignalResponse {
    pub success: bool,
    pub error_message: String,
    pub current_epoch: u64,
}

// --- Data Structures ---

/// Represents a connected client's session.
#[derive(Debug, Clone)]
pub struct ClientSession {
    pub session_id: u32,
    pub user_uuid: String,
    pub voice_group_id: Option<u32>,
    pub text_group_id: Option<u32>,
    pub socket_addr: SocketAddr,
    pub sender: tokio::sync::mpsc::UnboundedSender<ServiceMessage>,
}

/// Internal messages sent to client connection loops
#[derive(Debug, Clone)]
pub enum ServiceMessage {
    RelayAudio(Bytes),
    /// User joined a channel - broadcast to ALL connected users
    UserJoined {
        channel_id: u32,
        session_id: u32,
        display_name: String,
    },
    /// User left a channel - broadcast to ALL connected users
    UserLeft {
        channel_id: u32,
        session_id: u32,
    },
    /// Full channel state - sent to new joiners
    ChannelState {
        channel_id: u32,
        users: Vec<ChannelUser>,
    },
    /// Relay encrypted text message to channel members
    RelayText(EncryptedTextPacket),
}

/// Information about a user in a channel
#[derive(Debug, Clone)]
pub struct ChannelUser {
    pub session_id: u32,
    pub display_name: String,
}

/// Voice MLS Group - LOW CHURN
#[derive(Debug)]
pub struct VoiceGroup {
    pub id: u32,
    pub current_epoch: u64,
    pub members: DashSet<u32>, // Session IDs
}

/// Text MLS Group - HIGH CHURN with batched ratcheting
pub struct TextGroup {
    pub id: u32,
    pub current_epoch: u64,
    pub members: DashSet<u32>, // Session IDs
    /// Message count since last ratchet (for batched ratcheting)
    pub message_count: AtomicU32,
    /// Last ratchet time (for time-based ratcheting)
    pub last_ratchet: Instant,
}

impl std::fmt::Debug for TextGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextGroup")
            .field("id", &self.id)
            .field("current_epoch", &self.current_epoch)
            .field("members", &self.members)
            .field("message_count", &self.message_count.load(Ordering::Relaxed))
            .finish()
    }
}

/// Constants for batched ratcheting
const TEXT_RATCHET_MESSAGE_THRESHOLD: u32 = 50;
const TEXT_RATCHET_TIME_THRESHOLD_SECS: u64 = 300; // 5 minutes

/// The Zero-Trust Server State with persistence.
pub struct ServerState {
    // MLS groups
    pub voice_groups: Arc<DashMap<u32, Arc<RwLock<VoiceGroup>>>>,
    pub text_groups: Arc<DashMap<u32, Arc<RwLock<TextGroup>>>>,

    // User profiles (runtime, synced from DB)
    pub profiles: Arc<DashMap<u32, UserProfile>>,

    // Active sessions (in-memory)
    pub sessions: Arc<DashMap<u32, ClientSession>>,

    // Session ID counter
    session_counter: Arc<std::sync::atomic::AtomicU32>,

    // Persistence layer
    pub db: Arc<Database>,
    pub config: Config,
    pub auth: Arc<AuthService>,
}

impl ServerState {
    /// Create new server state with persistence.
    pub fn new(db: Arc<Database>, config: Config) -> Self {
        let auth = Arc::new(AuthService::new(Arc::clone(&db), config.clone()));

        Self {
            voice_groups: Arc::new(DashMap::new()),
            text_groups: Arc::new(DashMap::new()),
            profiles: Arc::new(DashMap::new()),
            sessions: Arc::new(DashMap::new()),
            session_counter: Arc::new(std::sync::atomic::AtomicU32::new(1)),
            db,
            config,
            auth,
        }
    }

    /// Allocate a new session ID.
    pub fn allocate_session_id(&self) -> u32 {
        self.session_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Register a new client session.
    pub fn register_session(&self, user_uuid: String, socket_addr: SocketAddr, sender: tokio::sync::mpsc::UnboundedSender<ServiceMessage>) -> u32 {
        // Allocate a new session ID
        let session_id = self.allocate_session_id();
        let session = ClientSession {
            session_id,
            user_uuid: user_uuid.clone(),
            voice_group_id: None,
            text_group_id: None,
            socket_addr,
            sender,
        };
        self.sessions.insert(session_id, session);
        info!("Registered session {} for user {}", session_id, user_uuid);
        session_id
    }

    /// Remove a client session.
    pub async fn remove_session(&self, session_id: u32) {
        if let Some((_, session)) = self.sessions.remove(&session_id) {
            // Broadcast user left before removing from groups
            if let Some(voice_id) = session.voice_group_id {
                self.broadcast_user_left(voice_id, session_id).await;
                
                if let Some(group) = self.voice_groups.get(&voice_id) {
                    group.value().write().await.members.remove(&session_id);
                }
            }
            if let Some(text_id) = session.text_group_id {
                if let Some(group) = self.text_groups.get(&text_id) {
                    group.value().write().await.members.remove(&session_id);
                }
            }
            info!("Removed session {}", session_id);
        }
    }

    /// Check if a user can join a voice channel based on verification policy.
    pub fn can_join_channel(&self, user_uuid: &str) -> Result<bool> {
        match self.config.verification.mode {
            VerificationMode::None | VerificationMode::Optional => Ok(true),
            VerificationMode::Required => {
                let user = self.db.find_user_by_uuid(user_uuid)?;
                match user {
                    Some(u) => Ok(u.verified),
                    None => Ok(false),
                }
            }
        }
    }

    /// Create a channel with both voice and text groups.
    pub fn create_channel(&self, channel_id: u32) {
        // Only create if not exists - don't overwrite existing groups!
        self.voice_groups
            .entry(channel_id)
            .or_insert_with(|| {
                Arc::new(RwLock::new(VoiceGroup {
                    id: channel_id,
                    current_epoch: 0,
                    members: DashSet::new(),
                }))
            });

        self.text_groups
            .entry(channel_id)
            .or_insert_with(|| {
                Arc::new(RwLock::new(TextGroup {
                    id: channel_id,
                    current_epoch: 0,
                    members: DashSet::new(),
                    message_count: AtomicU32::new(0),
                    last_ratchet: Instant::now(),
                }))
            });

        info!(
            "Created channel {} with Voice and Text MLS groups",
            channel_id
        );
    }

    /// Broadcast that a user joined a channel to ALL connected users.
    /// Also sends the full channel state to the new joiner.
    pub async fn broadcast_user_joined(&self, channel_id: u32, session_id: u32, display_name: String) {
        // Get all members of the voice group
        if let Some(group_lock) = self.voice_groups.get(&channel_id) {
            let group = group_lock.read().await;
            
            // Collect current users for the new joiner (excluding themselves)
            let mut users = Vec::new();
            for member_id in group.members.iter() {
                if *member_id != session_id {  // Exclude the joiner
                    if let Some(sess) = self.sessions.get(&*member_id) {
                        // Look up display name from DB
                        let name = self.db.find_user_by_uuid(&sess.user_uuid)
                            .ok()
                            .flatten()
                            .map(|u| u.display_name)
                            .unwrap_or_else(|| format!("User {}", *member_id));
                        
                        users.push(ChannelUser {
                            session_id: *member_id,
                            display_name: name,
                        });
                    }
                }
            }
            
            // Broadcast UserJoined to ALL connected users (not just in this channel)
            for sess in self.sessions.iter() {
                if *sess.key() != session_id {
                    let _ = sess.sender.send(ServiceMessage::UserJoined {
                        channel_id,
                        session_id,
                        display_name: display_name.clone(),
                    });
                }
            }
            
            // Send full channel state to the new joiner
            if let Some(new_sess) = self.sessions.get(&session_id) {
                let _ = new_sess.sender.send(ServiceMessage::ChannelState {
                    channel_id,
                    users,
                });
            }
        }
    }

    /// Broadcast that a user left a channel to ALL connected users.
    pub async fn broadcast_user_left(&self, channel_id: u32, session_id: u32) {
        // Broadcast to ALL connected users (not just in this channel)
        for sess in self.sessions.iter() {
            if *sess.key() != session_id {
                let _ = sess.sender.send(ServiceMessage::UserLeft {
                    channel_id,
                    session_id,
                });
            }
        }
    }

    /// Send the state of all channels to a newly connected user
    pub async fn send_all_channel_states(&self, session_id: u32) {
        if let Some(sess) = self.sessions.get(&session_id) {
            // Iterate through all voice groups
            for group_entry in self.voice_groups.iter() {
                let channel_id = *group_entry.key();
                let group = group_entry.value().read().await;
                
                // Collect users in this channel
                let mut users = Vec::new();
                for member_id in group.members.iter() {
                    if let Some(member_sess) = self.sessions.get(&*member_id) {
                        let name = self.db.find_user_by_uuid(&member_sess.user_uuid)
                            .ok()
                            .flatten()
                            .map(|u| u.display_name)
                            .unwrap_or_else(|| format!("User {}", *member_id));
                        
                        users.push(ChannelUser {
                            session_id: *member_id,
                            display_name: name,
                        });
                    }
                }
                
                // Send channel state if there are users
                if !users.is_empty() {
                    let _ = sess.sender.send(ServiceMessage::ChannelState {
                        channel_id,
                        users,
                    });
                }
            }
        }
    }


    // --- Text Message Routing (Zero-Knowledge) ---

    /// Broadcast an encrypted text message to all members of the text group.
    /// Server never decrypts - just routes opaque packets.
    /// Returns true if a ratchet is needed (for batched ratcheting).
    pub async fn broadcast_text_message(&self, sender_session_id: u32, packet: EncryptedTextPacket) -> bool {
        let channel_id = packet.channel_id;
        
        // Verify sender is a member of this text group
        let group_lock = match self.text_groups.get(&channel_id) {
            Some(g) => g.clone(),
            None => {
                warn!("Text group not found for channel {}", channel_id);
                return false;
            }
        };
        
        let group = group_lock.read().await;
        
        if !group.members.contains(&sender_session_id) {
            warn!("Session {} not a member of text group {}", sender_session_id, channel_id);
            return false;
        }
        
        // Increment message counter
        let msg_count = group.message_count.fetch_add(1, Ordering::Relaxed) + 1;
        
        // Collect members to send to (excluding sender)
        let members: Vec<u32> = group.members.iter().map(|id| *id).collect();
        drop(group); // Release lock before sending
        
        // Fan-out to all members except sender
        for member_id in members {
            if member_id == sender_session_id {
                continue; // Don't echo back to sender
            }
            
            if let Some(session) = self.sessions.get(&member_id) {
                let _ = session.sender.send(ServiceMessage::RelayText(packet.clone()));
            }
        }
        
        info!("Relayed text message from {} to channel {} (msg #{})", 
              sender_session_id, channel_id, msg_count);
        
        // Check if we need to ratchet
        self.should_ratchet_text_group(channel_id).await
    }
    
    /// Check if a text group should ratchet based on message count or time.
    /// Batched ratcheting: every 50 messages OR every 5 minutes.
    pub async fn should_ratchet_text_group(&self, channel_id: u32) -> bool {
        let group_lock = match self.text_groups.get(&channel_id) {
            Some(g) => g.clone(),
            None => return false,
        };
        
        let group = group_lock.read().await;
        let msg_count = group.message_count.load(Ordering::Relaxed);
        let elapsed = group.last_ratchet.elapsed().as_secs();
        
        msg_count >= TEXT_RATCHET_MESSAGE_THRESHOLD || elapsed >= TEXT_RATCHET_TIME_THRESHOLD_SECS
    }
    
    /// Reset ratchet counters after a successful epoch advance.
    pub async fn reset_text_ratchet_counters(&self, channel_id: u32) {
        let group_lock = match self.text_groups.get(&channel_id) {
            Some(g) => g.clone(),
            None => return,
        };
        
        let mut group = group_lock.write().await;
        group.message_count.store(0, Ordering::Relaxed);
        group.last_ratchet = Instant::now();
        info!("Reset ratchet counters for text group {}", channel_id);
    }


    // --- Group Membership Helpers ---

    pub async fn add_to_voice_group(&self, channel_id: u32, session_id: u32) {
        if let Some(group) = self.voice_groups.get(&channel_id) {
            group.value().write().await.members.insert(session_id);
        }
    }

    pub async fn remove_from_voice_group(&self, channel_id: u32, session_id: u32) {
        if let Some(group) = self.voice_groups.get(&channel_id) {
            group.value().write().await.members.remove(&session_id);
        }
    }

    pub async fn add_to_text_group(&self, channel_id: u32, session_id: u32) {
        if let Some(group) = self.text_groups.get(&channel_id) {
            group.value().write().await.members.insert(session_id);
        }
    }

    pub async fn remove_from_text_group(&self, channel_id: u32, session_id: u32) {
        if let Some(group) = self.text_groups.get(&channel_id) {
            group.value().write().await.members.remove(&session_id);
        }
    }

    // --- MLS Delivery Service (Reliable Signaling) ---

    /// Process an incoming MLS Message.
    pub async fn handle_mls_message(&self, msg: MlsEnvelope) -> Result<MlsSignalResponse> {
        let group_id = msg.group_id;
        let group_type = msg.group_type;

        match group_type {
            x if x == MLS_GROUP_TYPE_VOICE => self.handle_voice_mls(group_id, msg).await,
            x if x == MLS_GROUP_TYPE_TEXT => self.handle_text_mls(group_id, msg).await,
            _ => Err(anyhow!("Unknown group type: {}", group_type)),
        }
    }

    /// Handle Voice MLS message - LOW CHURN rules
    async fn handle_voice_mls(&self, group_id: u32, msg: MlsEnvelope) -> Result<MlsSignalResponse> {
        let group_lock = match self.voice_groups.get(&group_id) {
            Some(g) => g.clone(),
            None => return Err(anyhow!("Voice group not found")),
        };

        let mut group = group_lock.write().await;

        if msg.epoch != group.current_epoch {
            return Ok(MlsSignalResponse {
                success: false,
                error_message: "Error::StaleEpoch".into(),
                current_epoch: group.current_epoch,
            });
        }

        match msg.content {
            Some(mls_envelope::Content::Commit(_)) => {
                group.current_epoch += 1;
                info!(
                    "Voice Group {} advanced to Epoch {}",
                    group_id, group.current_epoch
                );
                Ok(MlsSignalResponse {
                    success: true,
                    error_message: String::new(),
                    current_epoch: group.current_epoch,
                })
            }
            _ => Ok(MlsSignalResponse {
                success: true,
                error_message: String::new(),
                current_epoch: group.current_epoch,
            }),
        }
    }

    /// Handle Text MLS message - HIGH CHURN allowed
    async fn handle_text_mls(&self, group_id: u32, msg: MlsEnvelope) -> Result<MlsSignalResponse> {
        let group_lock = match self.text_groups.get(&group_id) {
            Some(g) => g.clone(),
            None => return Err(anyhow!("Text group not found")),
        };

        let mut group = group_lock.write().await;

        if msg.epoch != group.current_epoch {
            return Ok(MlsSignalResponse {
                success: false,
                error_message: "Error::StaleEpoch".into(),
                current_epoch: group.current_epoch,
            });
        }

        match msg.content {
            Some(mls_envelope::Content::Commit(_)) => {
                group.current_epoch += 1;
                info!(
                    "Text Group {} advanced to Epoch {}",
                    group_id, group.current_epoch
                );
                Ok(MlsSignalResponse {
                    success: true,
                    error_message: String::new(),
                    current_epoch: group.current_epoch,
                })
            }
            _ => Ok(MlsSignalResponse {
                success: true,
                error_message: String::new(),
                current_epoch: group.current_epoch,
            }),
        }
    }

    // --- Profile Management ---

    /// Store a user profile (signed but plaintext).
    pub fn store_profile(&self, profile: UserProfile) -> Result<()> {
        let user_id = profile.user_id;
        self.profiles.insert(user_id, profile);
        info!("Stored profile for user {}", user_id);
        Ok(())
    }

    /// Get a user profile by ID.
    pub fn get_profile(&self, user_id: u32) -> Option<UserProfile> {
        self.profiles.get(&user_id).map(|p| p.clone())
    }

    // --- Hot Path Media Relay ---

    /// Route audio packet to voice group members.
    pub async fn route_audio_packet(&self, raw_bytes: Bytes) {
        let packet = match FastAudioPacket::parse(raw_bytes.clone()) {
            Ok(p) => p,
            Err(e) => {
                warn!("Bad Packet: {}", e);
                return;
            }
        };

        let sender_session = match self.sessions.get(&packet.session_id) {
            Some(s) => s.clone(),
            None => {
                // Sender not found
                return;
            }
        };

        let voice_group_id = match sender_session.voice_group_id {
            Some(id) => id,
            None => return, // Not in a voice channel
        };

        let members: Vec<u32> = match self.voice_groups.get(&voice_group_id) {
            Some(g) => {
                let group = g.value().read().await;
                group.members.iter().map(|id| *id).collect()
            },
            None => return,
        };

        // Fan-out to all other members
        for member_id in members {
            if member_id == sender_session.session_id {
                continue; // Don't echo back
            }

            if let Some(session) = self.sessions.get(&member_id) {
                let _ = session.sender.send(ServiceMessage::RelayAudio(raw_bytes.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_state() -> ServerState {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let config = Config::default();
        ServerState::new(db, config)
    }

    #[tokio::test]
    async fn test_session_management() {
        let state = create_test_state();
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let session_id = state.register_session("test-uuid-123".to_string(), addr, tx);
        assert!(state.sessions.contains_key(&session_id));

        state.remove_session(session_id).await;
        assert!(!state.sessions.contains_key(&session_id));
    }

    #[test]
    fn test_channel_creation() {
        let state = create_test_state();
        state.create_channel(1);

        assert!(state.voice_groups.contains_key(&1));
        assert!(state.text_groups.contains_key(&1));
    }

    #[tokio::test]
    async fn test_verification_policy() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        
        // Create a user
        let key = [0x42u8; 32];
        let user_uuid = db.create_user(&key, "TestUser").unwrap();

        // Test with Optional mode (default)
        let mut config = Config::default();
        let state = ServerState::new(Arc::clone(&db), config.clone());
        assert!(state.can_join_channel(&user_uuid).unwrap());

        // Test with Required mode - unverified user
        config.verification.mode = VerificationMode::Required;
        let state = ServerState::new(Arc::clone(&db), config.clone());
        assert!(!state.can_join_channel(&user_uuid).unwrap());

        // Verify user
        db.set_user_verified(&user_uuid, true).unwrap();
        assert!(state.can_join_channel(&user_uuid).unwrap());
    }

    #[tokio::test]
    async fn test_group_membership() {
        let state = create_test_state();
        let channel_id = 100;
        let session_id = 50;

        // Ensure channel exists
        state.create_channel(channel_id);

        // Test Voice Group
        state.add_to_voice_group(channel_id, session_id).await;
        {
            let group = state.voice_groups.get(&channel_id).unwrap();
            assert!(group.read().await.members.contains(&session_id));
        }

        state.remove_from_voice_group(channel_id, session_id).await;
        {
            let group = state.voice_groups.get(&channel_id).unwrap();
            assert!(!group.read().await.members.contains(&session_id));
        }

        // Test Text Group
        state.add_to_text_group(channel_id, session_id).await;
        {
            let group = state.text_groups.get(&channel_id).unwrap();
            assert!(group.read().await.members.contains(&session_id));
        }

        state.remove_from_text_group(channel_id, session_id).await;
        {
            let group = state.text_groups.get(&channel_id).unwrap();
            assert!(!group.read().await.members.contains(&session_id));
        }
    }

    #[tokio::test]
    async fn test_broadcast_logic() {
        let state = create_test_state();
        let channel_id = 200;
        
        // Setup two sessions
        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        
        let addr: SocketAddr = "127.0.0.1:1111".parse().unwrap();
        let s1 = state.register_session("uuid-1".into(), addr, tx1);
        let s2 = state.register_session("uuid-2".into(), addr, tx2);

        // Create channel
        state.create_channel(channel_id);
        
        // Add s1 to voice group first
        state.add_to_voice_group(channel_id, s1).await;
        
        // Broadcast joined for s2
        // We need to add s2 to voice group first for logic to work usually? 
        // broadcast_user_joined logic iterates voice group members to send state, 
        // and sends UserJoined to ALL connected sessions.
        state.broadcast_user_joined(channel_id, s2, "User 2".into()).await;

        // Check s1 received UserJoined
        if let Some(ServiceMessage::UserJoined { channel_id: c, session_id: s, display_name: n }) = rx1.recv().await {
            assert_eq!(c, channel_id);
            assert_eq!(s, s2);
            assert_eq!(n, "User 2");
        } else {
            panic!("s1 did not receive UserJoined");
        }

        // Check s2 received ChannelState (even if empty/just s1)
        // Since s1 is in voice group, s2 should see s1 in the state list if s2 is the joiner.
        // Wait, broadcast_user_joined sends ChannelState to the *joiner* (session_id arg).
        if let Some(ServiceMessage::ChannelState { channel_id: c, users }) = rx2.recv().await {
            assert_eq!(c, channel_id);
            // s1 is in the group, so it should be listed
            assert_eq!(users.len(), 1);
            assert_eq!(users[0].session_id, s1);
        } else {
            panic!("s2 did not receive ChannelState");
        }
    }

    #[tokio::test]
    async fn test_text_message_routing() {
        use aura_protocol::EncryptedTextPacket;
        let state = create_test_state();
        let channel_id = 300;
        state.create_channel(channel_id);

        // Setup two sessions
        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        
        let addr: SocketAddr = "127.0.0.1:2222".parse().unwrap();
        let s1 = state.register_session("uuid-1".into(), addr, tx1);
        let s2 = state.register_session("uuid-2".into(), addr, tx2);

        // Add both to text group
        state.add_to_text_group(channel_id, s1).await;
        state.add_to_text_group(channel_id, s2).await;

        // Create packet
        let packet = EncryptedTextPacket {
            sender_session_id: s1,
            channel_id,
            epoch: 1,
            message_id: "msg-123".into(),
            ciphertext: vec![1, 2, 3],
            nonce: vec![0; 24],
            tag: vec![0; 16],
            reply_to_id: "".into(),
        };

        // Broadcast from s1
        let _ = state.broadcast_text_message(s1, packet.clone()).await;

        // s2 should receive it
        if let Some(ServiceMessage::RelayText(recvd)) = rx2.recv().await {
            assert_eq!(recvd.sender_session_id, s1);
            assert_eq!(recvd.message_id, "msg-123");
            assert_eq!(recvd.ciphertext, vec![1, 2, 3]);
        } else {
            panic!("s2 did not receive text packet");
        }

        // s1 should NOT receive it (echo check)
        // We use try_recv or timeout to check absence
        assert!(rx1.try_recv().is_err(), "s1 received its own message (echo should be disabled)");
    }

    #[tokio::test]
    async fn test_ratchet_logic() {
        use aura_protocol::EncryptedTextPacket;
        let state = create_test_state();
        let channel_id = 400;
        state.create_channel(channel_id);
        
        // Add fake sender
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let addr: SocketAddr = "127.0.0.1:3333".parse().unwrap();
        let s1 = state.register_session("uuid-ratchet".into(), addr, tx);
        state.add_to_text_group(channel_id, s1).await;

        let packet = EncryptedTextPacket {
            sender_session_id: s1,
            channel_id,
            epoch: 1,
            message_id: "msg-x".into(),
            ciphertext: vec![],
            nonce: vec![],
            tag: vec![],
            reply_to_id: "".into(),
        };

        // Send 49 messages (threshold is 50)
        for _ in 0..49 {
            let ratchet = state.broadcast_text_message(s1, packet.clone()).await;
            assert!(!ratchet, "Should not ratchet yet");
        }

        // 50th message
        let ratchet = state.broadcast_text_message(s1, packet.clone()).await;
        assert!(ratchet, "Should trigger ratchet on 50th message");

        // Reset
        state.reset_text_ratchet_counters(channel_id).await;
        
        // Next message should not ratchet
        let ratchet = state.broadcast_text_message(s1, packet.clone()).await;
        assert!(!ratchet, "Counter should be reset");
    }
    #[tokio::test]
    async fn test_mls_signaling() {
        use aura_protocol::{MlsEnvelope, mls_envelope, MlsGroupType};
        let state = create_test_state();
        let channel_id = 500;
        state.create_channel(channel_id); // Creates epoch 0

        // 1. Submit valid COMMIT for Voice Group
        let mut commit_msg = MlsEnvelope {
            group_id: channel_id,
            group_type: MlsGroupType::Voice as i32,
            epoch: 0, // Current epoch
            sender_id: 12345,
            content: Some(mls_envelope::Content::Commit(vec![1, 2, 3])),
        };

        let res = state.handle_mls_message(commit_msg.clone()).await.unwrap();
        assert!(res.success);
        assert_eq!(res.current_epoch, 1); // Epoch should advance

        // 2. Submit STALE commit (replaying epoch 0)
        let res_stale = state.handle_mls_message(commit_msg.clone()).await.unwrap();
        assert!(!res_stale.success);
        assert_eq!(res_stale.error_message, "Error::StaleEpoch");
        assert_eq!(res_stale.current_epoch, 1);

        // 3. Submit FUTURE commit (epoch 2) - Should catch up or accept if policy allows?
        // Current logic strictly checks `msg.epoch != group.current_epoch`.
        // So sending epoch 2 when current is 1 will fail as Stale/Mismatch.
        // Let's verify that behavior.
        let future_msg = MlsEnvelope {
            group_id: channel_id,
            group_type: MlsGroupType::Voice as i32,
            epoch: 2, 
            sender_id: 12345,
            content: Some(mls_envelope::Content::Commit(vec![4,5,6])),
        };
        let res_future = state.handle_mls_message(future_msg).await.unwrap();
        assert!(!res_future.success); // Should fail strict check
        assert_eq!(res_future.current_epoch, 1);
        
        // 4. Unknown Group
        let unknown_msg = MlsEnvelope {
            group_id: 99999,
            group_type: MlsGroupType::Voice as i32,
            epoch: 0,
            sender_id: 12345,
            content: Some(mls_envelope::Content::Commit(vec![])),
        };
        assert!(state.handle_mls_message(unknown_msg).await.is_err());
    }
}
