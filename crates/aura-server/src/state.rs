//! Server state module.
//!
//! Extracts `ServerState` from main.rs and adds persistence layer.

use crate::auth::AuthService;
use crate::config::{Config, VerificationMode};
use crate::db::{Database, User};
use aura_protocol::{
    FastAudioPacket, UserProfile, MlsEnvelope, MlsGroupType, mls_envelope,
};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use dashmap::{DashMap, DashSet};
use std::net::SocketAddr;
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
    pub user_id: u32,
    pub voice_group_id: Option<u32>,
    pub text_group_id: Option<u32>,
    pub socket_addr: SocketAddr,
    pub sender: tokio::sync::mpsc::UnboundedSender<ServiceMessage>,
}

/// Internal messages sent to client connection loops
#[derive(Debug)]
pub enum ServiceMessage {
    RelayAudio(Bytes),
}

/// Voice MLS Group - LOW CHURN
#[derive(Debug)]
pub struct VoiceGroup {
    pub id: u32,
    pub current_epoch: u64,
    pub members: DashSet<u32>, // Session IDs
}

/// Text MLS Group - HIGH CHURN
#[derive(Debug)]
pub struct TextGroup {
    pub id: u32,
    pub current_epoch: u64,
    pub members: DashSet<u32>, // Session IDs
}

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
    pub fn register_session(&self, user_id: u32, socket_addr: SocketAddr, sender: tokio::sync::mpsc::UnboundedSender<ServiceMessage>) -> u32 {
        // Use user_id as session_id for simplicity (client alignment)
        let session_id = user_id;
        let session = ClientSession {
            session_id,
            user_id,
            voice_group_id: None,
            text_group_id: None,
            socket_addr,
            sender,
        };
        self.sessions.insert(session_id, session);
        info!("Registered session {} for user {}", session_id, user_id);
        session_id
    }

    /// Remove a client session.
    pub fn remove_session(&self, session_id: u32) {
        if let Some((_, session)) = self.sessions.remove(&session_id) {
            // Remove from groups
            if let Some(voice_id) = session.voice_group_id {
                if let Some(group) = self.voice_groups.get(&voice_id) {
                    group.value().blocking_write().members.remove(&session_id);
                }
            }
            if let Some(text_id) = session.text_group_id {
                if let Some(group) = self.text_groups.get(&text_id) {
                    group.value().blocking_write().members.remove(&session_id);
                }
            }
            info!("Removed session {}", session_id);
        }
    }

    /// Check if a user can join a voice channel based on verification policy.
    pub fn can_join_channel(&self, user_id: u32) -> Result<bool> {
        match self.config.verification.mode {
            VerificationMode::None | VerificationMode::Optional => Ok(true),
            VerificationMode::Required => {
                let user = self.db.find_user_by_id(user_id)?;
                match user {
                    Some(u) => Ok(u.verified),
                    None => Ok(false),
                }
            }
        }
    }

    /// Create a channel with both voice and text groups.
    pub fn create_channel(&self, channel_id: u32) {
        let voice_group = VoiceGroup {
            id: channel_id,
            current_epoch: 0,
            members: DashSet::new(),
        };
        self.voice_groups
            .insert(channel_id, Arc::new(RwLock::new(voice_group)));

        let text_group = TextGroup {
            id: channel_id,
            current_epoch: 0,
            members: DashSet::new(),
        };
        self.text_groups
            .insert(channel_id, Arc::new(RwLock::new(text_group)));

        info!(
            "Created channel {} with Voice and Text MLS groups",
            channel_id
        );
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
    pub fn route_audio_packet(&self, raw_bytes: Bytes) {
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
                let group = g.value().blocking_read();
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

    #[test]
    fn test_session_management() {
        let state = create_test_state();
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let session_id = state.register_session(1, addr, tx);
        assert!(state.sessions.contains_key(&session_id));

        state.remove_session(session_id);
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
        let user_id = db.create_user(&key, "TestUser").unwrap();

        // Test with Optional mode (default)
        let mut config = Config::default();
        let state = ServerState::new(Arc::clone(&db), config.clone());
        assert!(state.can_join_channel(user_id).unwrap());

        // Test with Required mode - unverified user
        config.verification.mode = VerificationMode::Required;
        let state = ServerState::new(Arc::clone(&db), config.clone());
        assert!(!state.can_join_channel(user_id).unwrap());

        // Verify user
        db.set_user_verified(user_id, true).unwrap();
        assert!(state.can_join_channel(user_id).unwrap());
    }
}
