//! Server state module.
//!
//! Extracts `ServerState` from main.rs and adds persistence layer.

use crate::auth::AuthService;
use crate::config::{Config, VerificationMode};
use crate::db::Database;
use aura_protocol::{
    FastAudioPacket, UserProfile, MlsEnvelope, MlsGroupType, mls_envelope, EncryptedTextPacket,
    ServerState as ProtoServerState, ChannelInfo as ProtoChannelInfo, ChannelIcon as ProtoChannelIcon,
    channel_icon,
};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use dashmap::{DashMap, DashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
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
    pub voice_group_id: Option<String>,
    pub text_group_id: Option<String>,
    pub is_muted: bool,
    pub is_deafened: bool,
    pub socket_addr: SocketAddr,
    pub sender: tokio::sync::mpsc::UnboundedSender<ServiceMessage>,
    /// Bytes received from this client (audio + control)
    pub bytes_in: Arc<AtomicU64>,
    /// Bytes sent to this client (relayed audio)
    pub bytes_out: Arc<AtomicU64>,
}

/// Internal messages sent to client connection loops
#[derive(Debug, Clone)]
pub enum ServiceMessage {
    RelayAudio(Bytes),
    /// User joined a channel - broadcast to ALL connected users
    UserJoined {
        channel_id: String,
        session_id: u32,
        display_name: String,
    },
    /// User left a channel - broadcast to ALL connected users
    UserLeft {
        channel_id: String,
        session_id: u32,
    },
    /// Full snapshot of server state - sent to new joiners
    ServerSnapshot(ProtoServerState),
    /// Relay encrypted text message to channel members
    RelayText(EncryptedTextPacket),
    
    // --- MLS Protocol Messages ---
    
    /// Tell client to create a new MLS group (they are the first joiner)
    MlsCreateGroup {
        channel_id: String,
        is_voice: bool,
    },
    /// Forward a key package to the group founder for addition
    MlsAddMemberRequest {
        channel_id: String,
        is_voice: bool,
        joiner_session_id: u32,
        joiner_uuid: String,
        key_package: Vec<u8>,
    },
    /// Distribute a Commit to existing group members
    MlsCommit {
        channel_id: String,
        is_voice: bool,
        commit: Vec<u8>,
    },
    /// Send Welcome to a new member joining via add
    MlsWelcome {
        channel_id: String,
        is_voice: bool,
        welcome: Vec<u8>,
    },
    /// Status update (mute/deafen) - broadcast to channel members
    UserStatusUpdate {
        session_id: u32,
        is_muted: bool,
        is_deafened: bool,
    },
}

/// Static metadata for a channel (persisted in DB)
#[derive(Debug, Clone)]
pub struct ChannelMetadata {
    pub id: String,
    pub name: String,
    pub comment: String,
    pub icon_type: i32,
    pub icon_data: Vec<u8>,
    pub position: i32,
}

/// Information about a user in a channel.
#[derive(Debug, Clone)]
pub struct ChannelUser {
    pub session_id: u32,
    pub display_name: String,
}

/// Pending key package waiting for founder to process
#[derive(Debug, Clone)]
pub struct PendingMlsJoin {
    pub joiner_session_id: u32,
    pub joiner_uuid: String,
    pub key_package: Vec<u8>,
}

/// Voice MLS Group - LOW CHURN
#[derive(Debug)]
pub struct VoiceGroup {
    pub id: String,
    pub current_epoch: u64,
    pub members: DashSet<u32>, // Session IDs
    /// Session ID of the group founder (first joiner who created the MLS group)
    pub founder_session_id: Option<u32>,
    /// Pending key packages from joiners waiting to be added
    pub pending_joins: Vec<PendingMlsJoin>,
}

/// Text MLS Group - HIGH CHURN with batched ratcheting
pub struct TextGroup {
    pub id: String,
    pub current_epoch: u64,
    pub members: DashSet<u32>, // Session IDs
    /// Message count since last ratchet (for batched ratcheting)
    pub message_count: AtomicU32,
    /// Last ratchet time (for time-based ratcheting)
    pub last_ratchet: Instant,
    /// Session ID of the group founder
    pub founder_session_id: Option<u32>,
    /// Pending key packages from joiners waiting to be added
    pub pending_joins: Vec<PendingMlsJoin>,
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

/// TTL for seen message IDs (replay protection)
const SEEN_MESSAGE_TTL_SECS: u64 = 300; // 5 minutes

/// Seen message entry with expiration timestamp
#[derive(Debug, Clone)]
pub struct SeenMessageEntry {
    pub message_id: String,
    pub expires_at: Instant,
}

/// Replay protection: track seen message IDs per channel
#[derive(Debug)]
pub struct SeenMessages {
    /// Map of channel_id -> list of (message_id, expires_at)
    messages: DashMap<String, Vec<SeenMessageEntry>>,
}

impl SeenMessages {
    pub fn new() -> Self {
        Self {
            messages: DashMap::new(),
        }
    }
    
    /// Check if a message ID has been seen. If not, mark it as seen.
    /// Returns true if the message is NEW (not a replay).
    /// Returns false if the message is a REPLAY (already seen).
    pub fn check_and_mark(&self, channel_id: String, message_id: &str) -> bool {
        let expires_at = Instant::now() + std::time::Duration::from_secs(SEEN_MESSAGE_TTL_SECS);
        
        let mut entries = self.messages.entry(channel_id).or_insert_with(Vec::new);
        
        // Check if already seen
        for entry in entries.iter() {
            if entry.message_id == message_id {
                return false; // Replay detected!
            }
        }
        
        // Not seen - add to list
        entries.push(SeenMessageEntry {
            message_id: message_id.to_string(),
            expires_at,
        });
        
        true // New message
    }
    
    /// Cleanup expired entries. Call periodically to prevent memory bloat.
    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        
        for mut entries in self.messages.iter_mut() {
            entries.value_mut().retain(|e| e.expires_at > now);
        }
        
        // Remove empty channels
        self.messages.retain(|_, v| !v.is_empty());
    }
    
    /// Get count of tracked messages (for metrics)
    pub fn message_count(&self) -> usize {
        self.messages.iter().map(|e| e.value().len()).sum()
    }
}

/// The Zero-Trust Server State with persistence.
pub struct ServerState {
    // MLS groups
    pub voice_groups: Arc<DashMap<String, Arc<RwLock<VoiceGroup>>>>,
    pub text_groups: Arc<DashMap<String, Arc<RwLock<TextGroup>>>>,

    // Channel metadata (synced from DB)
    pub channel_metadata: Arc<DashMap<String, ChannelMetadata>>,

    // User profiles (runtime, synced from DB)
    pub profiles: Arc<DashMap<String, UserProfile>>,

    // Active sessions (in-memory)
    pub sessions: Arc<DashMap<u32, ClientSession>>,

    // Session ID counter
    session_counter: Arc<std::sync::atomic::AtomicU32>,
    
    // Replay protection: track seen text message IDs
    pub seen_messages: Arc<SeenMessages>,

    // Persistence layer
    pub db: Arc<Database>,
    pub config: Config,
    pub auth: Arc<AuthService>,
}

impl ServerState {
    /// Create new server state with persistence.
    pub fn new(db: Arc<Database>, config: Config) -> Self {
        let auth = Arc::new(AuthService::new(Arc::clone(&db), config.clone()));

        let state = Self {
            voice_groups: Arc::new(DashMap::new()),
            text_groups: Arc::new(DashMap::new()),
            channel_metadata: Arc::new(DashMap::new()),
            profiles: Arc::new(DashMap::new()),
            sessions: Arc::new(DashMap::new()),
            session_counter: Arc::new(std::sync::atomic::AtomicU32::new(1)),
            seen_messages: Arc::new(SeenMessages::new()),
            db: db.clone(),
            config: config.clone(),
            auth,
        };

        // Load channels from DB
        match db.get_all_channels() {
            Ok(channels) => {
                for (id, name, comment, i_type, i_data, pos) in channels {
                    state.channel_metadata.insert(id.clone(), ChannelMetadata {
                        id: id.clone(), name, comment, icon_type: i_type, icon_data: i_data, position: pos
                    });
                    state.create_channel(id);
                }
            }
            Err(e) => warn!("Failed to load channels from DB: {}", e),
        }

        state
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
            is_muted: false,
            is_deafened: false,
            socket_addr,
            sender,
            bytes_in: Arc::new(AtomicU64::new(0)),
            bytes_out: Arc::new(AtomicU64::new(0)),
        };
        self.sessions.insert(session_id, session);
        
        // Populate profile cache immediately
        match self.db.find_user_by_uuid(&user_uuid) {
            Ok(Some(user)) => {
                let mut bio = String::new();
                let mut avatar_data = vec![];
                let mut signature = vec![];
                let mut signing_key = user.ed25519_public_key.to_vec();

                if let Ok(Some((b, a, s, sk))) = self.db.get_user_profile(&user_uuid) {
                    bio = b;
                    avatar_data = a;
                    signature = s;
                    signing_key = sk;
                }

                let profile = UserProfile {
                    user_id: user_uuid.clone(),
                    display_name: user.display_name.clone(),
                    bio,
                    avatar_data,
                    signature,
                    signing_key,
                };
                self.profiles.insert(user_uuid.clone(), profile);
                info!("Registered session {} for user {} (Profile cached)", session_id, user.display_name);
            }
            Ok(None) => warn!("Registered session {} for unknown user {}", session_id, user_uuid),
            Err(e) => warn!("Failed to fetch profile for user {}: {}", user_uuid, e),
        }

        session_id
    }

    /// Remove a client session.
    pub async fn remove_session(&self, session_id: u32) {
        if let Some((_, session)) = self.sessions.remove(&session_id) {
            // Broadcast user left before removing from groups
            if let Some(voice_id) = session.voice_group_id {
                self.broadcast_user_left(voice_id.clone(), session_id).await;
                
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
            
            // Only remove the profile cache entry if no other active session
            // shares this user_uuid. A fast reconnect can register a new session
            // before the old one is fully torn down — we must not wipe the new
            // session's profile in that case.
            let still_active = self.sessions.iter()
                .any(|s| s.value().user_uuid == session.user_uuid);
            if !still_active {
                self.profiles.remove(&session.user_uuid);
            }
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
    pub fn create_channel(&self, channel_id: String) {
        // Only create if not exists - don't overwrite existing groups!
        self.voice_groups
            .entry(channel_id.clone())
            .or_insert_with(|| {
                Arc::new(RwLock::new(VoiceGroup {
                    id: channel_id.clone(),
                    current_epoch: 0,
                    members: DashSet::new(),
                    founder_session_id: None,
                    pending_joins: Vec::new(),
                }))
            });

        self.text_groups
            .entry(channel_id.clone())
            .or_insert_with(|| {
                Arc::new(RwLock::new(TextGroup {
                    id: channel_id.clone(),
                    current_epoch: 0,
                    members: DashSet::new(),
                    message_count: AtomicU32::new(0),
                    last_ratchet: Instant::now(),
                    founder_session_id: None,
                    pending_joins: Vec::new(),
                }))
            });

        info!(
            "Created channel {} with Voice and Text MLS groups",
            channel_id
        );
    }

    /// Broadcast that a user joined a channel to ALL connected users.
    /// Also sends the full channel state to the new joiner.
    /// Generate a full snapshot of the server state for a new connection.
    pub async fn get_server_snapshot(&self) -> ProtoServerState {
        let mut channels = Vec::new();
        for meta_entry in self.channel_metadata.iter() {
            let meta = meta_entry.value();
            let mut users = Vec::new();
            
            if let Some(group_lock) = self.voice_groups.get(&meta.id) {
                let group = group_lock.read().await;
                for id in group.members.iter() {
                    if let Some(sess) = self.sessions.get(&id) {
                        users.push(aura_protocol::ChannelUserStatus {
                            session_id: *id,
                            is_muted: sess.is_muted,
                            is_deafened: sess.is_deafened,
                        });
                    } else {
                        users.push(aura_protocol::ChannelUserStatus {
                            session_id: *id,
                            is_muted: false,
                            is_deafened: false,
                        });
                    }
                }
            }

            let icon = match meta.icon_type {
                1 => Some(ProtoChannelIcon {
                    icon: Some(channel_icon::Icon::Emoji(String::from_utf8_lossy(&meta.icon_data).into())),
                }),
                2 => Some(ProtoChannelIcon {
                    icon: Some(channel_icon::Icon::PresetId(String::from_utf8_lossy(&meta.icon_data).into())),
                }),
                3 => Some(ProtoChannelIcon {
                    icon: Some(channel_icon::Icon::CustomData(meta.icon_data.clone().into())),
                }),
                _ => None,
            };

            channels.push(ProtoChannelInfo {
                channel_id: meta.id.clone(),
                name: meta.name.clone(),
                comment: meta.comment.clone(),
                icon,
                position: meta.position,
                user_ids: users.iter().map(|u| u.session_id).collect(),
                users,
            });
        }

        let profiles: Vec<UserProfile> = self.profiles.iter().map(|p| p.value().clone()).collect();
        
        info!("[Snapshot] Sending {} channels and {} profiles", channels.len(), profiles.len());

        ProtoServerState { channels, profiles }
    }

    /// Broadcast that a user joined a channel to ALL connected users.
    pub async fn broadcast_user_joined(&self, channel_id: String, session_id: u32, display_name: String) {
        // Broadcast UserJoined to ALL connected users
        for sess in self.sessions.iter() {
            if *sess.key() != session_id {
                let _ = sess.sender.send(ServiceMessage::UserJoined {
                    channel_id: channel_id.clone(),
                    session_id,
                    display_name: display_name.clone(),
                });
            }
        }
        
        // Send full server snapshot to the new joiner
        if let Some(new_sess) = self.sessions.get(&session_id) {
            let snapshot = self.get_server_snapshot().await;
            let _ = new_sess.sender.send(ServiceMessage::ServerSnapshot(snapshot));
        }
    }

    /// Broadcast that a user left a channel to ALL connected users.
    pub async fn broadcast_user_left(&self, channel_id: String, session_id: u32) {
        // Broadcast to ALL connected users (not just in this channel)
        for sess in self.sessions.iter() {
            if *sess.key() != session_id {
                let _ = sess.sender.send(ServiceMessage::UserLeft {
                    channel_id: channel_id.clone(),
                    session_id,
                });
            }
        }
    }

    /// Broadcast user status update (mute/deafen) to everyone.
    pub async fn broadcast_user_status(&self, session_id: u32, is_muted: bool, is_deafened: bool) {
        // Force muted true if deafened is true
        let (is_muted, is_deafened) = if is_deafened { (true, true) } else { (is_muted, is_deafened) };

        // Update in-memory session state
        if let Some(mut sess) = self.sessions.get_mut(&session_id) {
            sess.is_muted = is_muted;
            sess.is_deafened = is_deafened;
        }

        // Broadcast to ALL connected users
        for sess in self.sessions.iter() {
            let _ = sess.sender.send(ServiceMessage::UserStatusUpdate {
                session_id,
                is_muted,
                is_deafened,
            });
        }
    }

    /// Send the full server state snapshot to a newly connected user
    pub async fn send_server_snapshot(&self, session_id: u32) {
        if let Some(sess) = self.sessions.get(&session_id) {
            let snapshot = self.get_server_snapshot().await;
            let _ = sess.sender.send(ServiceMessage::ServerSnapshot(snapshot));
        }
    }


    // --- Text Message Routing (Zero-Knowledge) ---

    /// Broadcast an encrypted text message to all members of the text group.
    /// Server never decrypts - just routes opaque packets.
    /// Returns true if a ratchet is needed (for batched ratcheting).
    pub async fn broadcast_text_message(&self, sender_session_id: u32, packet: EncryptedTextPacket) -> bool {
        let channel_id = packet.channel_id.clone();
        let message_id = &packet.message_id;
        
        // Replay protection: Check if we've seen this message ID before
        if !self.seen_messages.check_and_mark(channel_id.clone(), message_id) {
            warn!("Replay attack detected! Duplicate message ID '{}' from session {} in channel {}", 
                  message_id, sender_session_id, channel_id);
            return false;
        }
        
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
    pub async fn should_ratchet_text_group(&self, channel_id: String) -> bool {
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
    pub async fn reset_text_ratchet_counters(&self, channel_id: String) {
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

    pub async fn add_to_voice_group(&self, channel_id: String, session_id: u32) {
        if let Some(group) = self.voice_groups.get(&channel_id) {
            group.value().write().await.members.insert(session_id);
        }
        
        // Set voice_group_id on session so audio routing works
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.voice_group_id = Some(channel_id);
        }
    }

    pub async fn remove_from_voice_group(&self, channel_id: String, session_id: u32) {
        if let Some(group) = self.voice_groups.get(&channel_id) {
            group.value().write().await.members.remove(&session_id);
        }
        
        // Clear voice_group_id on session
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.voice_group_id = None;
        }
    }

    pub async fn add_to_text_group(&self, channel_id: String, session_id: u32) {
        if let Some(group) = self.text_groups.get(&channel_id) {
            group.value().write().await.members.insert(session_id);
        }

        // Set text_group_id on session
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.text_group_id = Some(channel_id);
        }
    }

    pub async fn remove_from_text_group(&self, channel_id: String, session_id: u32) {
        if let Some(group) = self.text_groups.get(&channel_id) {
            group.value().write().await.members.remove(&session_id);
        }

        // Clear text_group_id on session
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.text_group_id = None;
        }
    }

    // --- MLS First-Joiner Protocol ---

    /// Handle a client joining a channel with their MLS key package.
    /// Implements the first-joiner protocol:
    /// - If no founder exists: This client becomes founder, told to create group
    /// - If founder exists: Key package is forwarded to founder for addition
    pub async fn handle_mls_join(
        &self,
        channel_id: String,
        is_voice: bool,
        session_id: u32,
        user_uuid: String,
        key_package: Vec<u8>,
    ) {
        let session = match self.sessions.get(&session_id) {
            Some(s) => s.clone(),
            None => return,
        };

        if is_voice {
            self.handle_voice_mls_join(channel_id, session_id, user_uuid, key_package, &session.sender).await;
        } else {
            self.handle_text_mls_join(channel_id, session_id, user_uuid, key_package, &session.sender).await;
        }
    }

    async fn handle_voice_mls_join(
        &self,
        channel_id: String,
        session_id: u32,
        user_uuid: String,
        key_package: Vec<u8>,
        sender: &tokio::sync::mpsc::UnboundedSender<ServiceMessage>,
    ) {
        let group_lock = match self.voice_groups.get(&channel_id) {
            Some(g) => g.clone(),
            None => {
                warn!("Voice group {} not found for MLS join", channel_id);
                return;
            }
        };

        let mut group = group_lock.write().await;

        if group.founder_session_id.is_none() {
            // First joiner becomes founder - tell them to create the group
            group.founder_session_id = Some(session_id);
            group.members.insert(session_id);
            info!("[MLS] Session {} is founder of voice group {}", session_id, channel_id);

            let _ = sender.send(ServiceMessage::MlsCreateGroup {
                channel_id: channel_id.clone(),
                is_voice: true,
            });
        } else {
            // Not the first - queue key package and notify founder
            let pending = PendingMlsJoin {
                joiner_session_id: session_id,
                joiner_uuid: user_uuid.clone(),
                key_package: key_package.clone(),
            };
            group.pending_joins.push(pending);

            let founder_id = group.founder_session_id.unwrap();
            drop(group); // Release lock before sending

            if let Some(founder_session) = self.sessions.get(&founder_id) {
                info!("[MLS] Forwarding key package from {} to founder {} for voice group {}",
                      session_id, founder_id, channel_id);
                let _ = founder_session.sender.send(ServiceMessage::MlsAddMemberRequest {
                    channel_id: channel_id.clone(),
                    is_voice: true,
                    joiner_session_id: session_id,
                    joiner_uuid: user_uuid,
                    key_package,
                });
            }
        }
    }

    async fn handle_text_mls_join(
        &self,
        channel_id: String,
        session_id: u32,
        user_uuid: String,
        key_package: Vec<u8>,
        sender: &tokio::sync::mpsc::UnboundedSender<ServiceMessage>,
    ) {
        let group_lock = match self.text_groups.get(&channel_id) {
            Some(g) => g.clone(),
            None => return,
        };

        let mut group = group_lock.write().await;

        if group.founder_session_id.is_none() {
            group.founder_session_id = Some(session_id);
            group.members.insert(session_id);
            info!("[MLS] Session {} is founder of text group {}", session_id, channel_id);

            let _ = sender.send(ServiceMessage::MlsCreateGroup {
                channel_id: channel_id.clone(),
                is_voice: false,
            });
        } else {
            let pending = PendingMlsJoin {
                joiner_session_id: session_id,
                joiner_uuid: user_uuid.clone(),
                key_package: key_package.clone(),
            };
            group.pending_joins.push(pending);

            let founder_id = group.founder_session_id.unwrap();
            drop(group);

            if let Some(founder_session) = self.sessions.get(&founder_id) {
                let _ = founder_session.sender.send(ServiceMessage::MlsAddMemberRequest {
                    channel_id: channel_id.clone(),
                    is_voice: false,
                    joiner_session_id: session_id,
                    joiner_uuid: user_uuid,
                    key_package,
                });
            }
        }
    }

    /// Handle commit/welcome from a member who added someone.
    /// Broadcasts commit to existing members, sends welcome to new member.
    pub async fn handle_mls_commit_welcome(
        &self,
        channel_id: String,
        is_voice: bool,
        committer_session_id: u32,
        new_member_session_id: u32,
        commit: Vec<u8>,
        welcome: Vec<u8>,
    ) {
        // Send Welcome to new member
        if let Some(new_session) = self.sessions.get(&new_member_session_id) {
            info!("[MLS] Sending Welcome to session {} for {} group {}",
                  new_member_session_id, if is_voice { "voice" } else { "text" }, channel_id);
            let _ = new_session.sender.send(ServiceMessage::MlsWelcome {
                channel_id: channel_id.clone(),
                is_voice,
                welcome,
            });
        }

        // Get members list and advance epoch
        let members: Vec<u32> = if is_voice {
            if let Some(group_lock) = self.voice_groups.get(&channel_id) {
                let mut group = group_lock.write().await;
                group.current_epoch += 1;
                group.members.insert(new_member_session_id); // Add new member
                // Remove from pending
                group.pending_joins.retain(|p| p.joiner_session_id != new_member_session_id);
                group.members.iter().map(|id| *id).collect()
            } else {
                return;
            }
        } else {
            if let Some(group_lock) = self.text_groups.get(&channel_id) {
                let mut group = group_lock.write().await;
                group.current_epoch += 1;
                group.members.insert(new_member_session_id);
                group.pending_joins.retain(|p| p.joiner_session_id != new_member_session_id);
                group.members.iter().map(|id| *id).collect()
            } else {
                return;
            }
        };

        // Broadcast Commit to all existing members (except new member and committer)
        for member_id in members {
            if member_id == new_member_session_id || member_id == committer_session_id {
                continue;
            }
            if let Some(session) = self.sessions.get(&member_id) {
                let _ = session.sender.send(ServiceMessage::MlsCommit {
                    channel_id: channel_id.clone(),
                    is_voice,
                    commit: commit.clone(),
                });
            }
        }

        info!("[MLS] Distributed commit for {} group {} (new member: {})",
              if is_voice { "voice" } else { "text" }, channel_id, new_member_session_id);
    }

    // --- MLS Delivery Service (Reliable Signaling) ---

    /// Process an incoming MLS Message.
    pub async fn handle_mls_message(&self, msg: MlsEnvelope) -> Result<MlsSignalResponse> {
        let group_id = msg.group_id.clone();
        let group_type = msg.group_type;

        match group_type {
            x if x == MLS_GROUP_TYPE_VOICE => self.handle_voice_mls(group_id, msg).await,
            x if x == MLS_GROUP_TYPE_TEXT => self.handle_text_mls(group_id, msg).await,
            _ => Err(anyhow!("Unknown group type: {}", group_type)),
        }
    }

    /// Handle Voice MLS message - LOW CHURN rules
    async fn handle_voice_mls(&self, group_id: String, msg: MlsEnvelope) -> Result<MlsSignalResponse> {
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
    async fn handle_text_mls(&self, group_id: String, msg: MlsEnvelope) -> Result<MlsSignalResponse> {
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
        let user_id = profile.user_id.clone();
        self.profiles.insert(user_id.clone(), profile);
        info!("Stored profile for user {}", user_id);
        Ok(())
    }

    /// Get a user profile by ID.
    pub fn get_profile(&self, user_uuid: &str) -> Option<UserProfile> {
        self.profiles.get(user_uuid).map(|p| p.clone())
    }

    // --- Channel & Profile Management ---

    /// Create a new channel persistently.
    pub async fn create_channel_persistent(&self, name: String, comment: String, icon: Option<ProtoChannelIcon>) -> Result<String> {
        let (icon_type, icon_data) = self.convert_proto_icon(icon);
        
        let channel_id = self.db.upsert_channel(None, &name, &comment, icon_type, &icon_data, 0)?;
        
        // Update in-memory metadata
        self.channel_metadata.insert(channel_id.clone(), ChannelMetadata {
            id: channel_id.clone(),
            name,
            comment,
            icon_type,
            icon_data,
            position: 0,
        });

        // Initialize MLS groups
        self.create_channel(channel_id.clone());

        // Broadcast full state update to everyone
        let snapshot = self.get_server_snapshot().await;
        for sess in self.sessions.iter() {
            let _ = sess.sender.send(ServiceMessage::ServerSnapshot(snapshot.clone()));
        }

        Ok(channel_id)
    }

    /// Update channel metadata persistently.
    pub async fn update_channel_persistent(&self, channel_id: String, name: Option<String>, comment: Option<String>, icon: Option<ProtoChannelIcon>, position: Option<i32>) -> Result<()> {
        let mut meta = self.channel_metadata.get_mut(&channel_id).ok_or_else(|| anyhow!("Channel not found"))?;
        
        if let Some(n) = name { meta.name = n; }
        if let Some(c) = comment { meta.comment = c; }
        if let Some(i) = icon {
            let (t, d) = self.convert_proto_icon(Some(i));
            meta.icon_type = t;
            meta.icon_data = d;
        }
        if let Some(p) = position { meta.position = p; }

        // Persist to DB
        self.db.upsert_channel(Some(channel_id.clone()), &meta.name, &meta.comment, meta.icon_type, &meta.icon_data, meta.position)?;
        
        drop(meta); // Release lock

        // Broadcast full state update
        let snapshot = self.get_server_snapshot().await;
        for sess in self.sessions.iter() {
            let _ = sess.sender.send(ServiceMessage::ServerSnapshot(snapshot.clone()));
        }

        Ok(())
    }

    /// Update user profile persistently.
    pub async fn update_profile_persistent(&self, session_id: u32, profile: UserProfile) -> Result<()> {
        let session = self.sessions.get(&session_id).ok_or_else(|| anyhow!("Session not found"))?;
        let user_uuid = session.user_uuid.clone();
        
        // Update DB
        self.db.upsert_user_profile(
            &session.user_uuid, 
            &profile.bio, 
            &profile.avatar_data, 
            &profile.signature, 
            &profile.signing_key
        )?;

        // Update in-memory cache
        self.profiles.insert(user_uuid, profile);

        Ok(())
    }

    /// Delete a channel persistently.
    pub async fn delete_channel_persistent(&self, channel_id: &str) -> Result<()> {
        // Update DB
        self.db.delete_channel(channel_id)?;
        
        // Update in-memory metadata
        self.channel_metadata.remove(channel_id);
        
        // Force everyone out of the channel groups in-memory
        if let Some(voice_group) = self.voice_groups.get(channel_id) {
            let members = { voice_group.read().await.members.clone() };
            for session_id in members {
                self.remove_from_voice_group(channel_id.to_string(), session_id).await;
            }
        }
        if let Some(text_group) = self.text_groups.get(channel_id) {
            let members = { text_group.read().await.members.clone() };
            for session_id in members {
                self.remove_from_text_group(channel_id.to_string(), session_id).await;
            }
        }

        self.voice_groups.remove(channel_id);
        self.text_groups.remove(channel_id);

        // Broadcast full state update to everyone
        let snapshot = self.get_server_snapshot().await;
        for sess in self.sessions.iter() {
            let _ = sess.sender.send(ServiceMessage::ServerSnapshot(snapshot.clone()));
        }

        Ok(())
    }

    /// Delete a user profile and all associated data persistently.
    pub async fn delete_user_persistent(&self, user_uuid: &str) -> Result<()> {
        // Update DB
        self.db.delete_user(user_uuid)?;
        
        // Update in-memory
        self.profiles.remove(user_uuid);
        
        // Invalidate any active sessions for this user
        let mut sessions_to_remove = Vec::new();
        for sess in self.sessions.iter() {
            if sess.user_uuid == user_uuid {
                sessions_to_remove.push(*sess.key());
            }
        }
        
        for session_id in sessions_to_remove {
            self.remove_session(session_id).await;
        }

        // Broadcast full state update
        let snapshot = self.get_server_snapshot().await;
        for sess in self.sessions.iter() {
            let _ = sess.sender.send(ServiceMessage::ServerSnapshot(snapshot.clone()));
        }

        Ok(())
    }

    /// Helper to convert ProtoChannelIcon to (type, data)
    fn convert_proto_icon(&self, icon: Option<ProtoChannelIcon>) -> (i32, Vec<u8>) {
        match icon.and_then(|i| i.icon) {
            Some(channel_icon::Icon::Emoji(e)) => (1, e.into_bytes()),
            Some(channel_icon::Icon::PresetId(p)) => (2, p.into_bytes()),
            Some(channel_icon::Icon::CustomData(d)) => (3, d.to_vec()),
            None => (0, vec![]),
        }
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

        // Don't relay if sender is muted
        if sender_session.is_muted {
            return;
        }

        // Track inbound bytes for this sender
        sender_session.bytes_in.fetch_add(raw_bytes.len() as u64, Ordering::Relaxed);

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
        let relay_len = raw_bytes.len() as u64;
        for member_id in members {
            if member_id == sender_session.session_id {
                continue; // Don't echo back
            }

            if let Some(session) = self.sessions.get(&member_id) {
                // Don't relay if receiver is deafened
                if session.is_deafened {
                    continue;
                }
                // Track outbound bytes for this receiver
                session.bytes_out.fetch_add(relay_len, Ordering::Relaxed);
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
        let channel_id = "C_1".to_string();
        state.create_channel(channel_id.clone());

        assert!(state.voice_groups.contains_key(&channel_id));
        assert!(state.text_groups.contains_key(&channel_id));
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
        let channel_id = "C_100".to_string();
        let session_id = 50;

        // Ensure channel exists
        state.create_channel(channel_id.clone());

        // Test Voice Group
        state.add_to_voice_group(channel_id.clone(), session_id).await;
        {
            let group = state.voice_groups.get(&channel_id).unwrap();
            assert!(group.read().await.members.contains(&session_id));
        }

        state.remove_from_voice_group(channel_id.clone(), session_id).await;
        {
            let group = state.voice_groups.get(&channel_id).unwrap();
            assert!(!group.read().await.members.contains(&session_id));
        }

        // Test Text Group
        state.add_to_text_group(channel_id.clone(), session_id).await;
        {
            let group = state.text_groups.get(&channel_id).unwrap();
            assert!(group.read().await.members.contains(&session_id));
        }

        state.remove_from_text_group(channel_id.clone(), session_id).await;
        {
            let group = state.text_groups.get(&channel_id).unwrap();
            assert!(!group.read().await.members.contains(&session_id));
        }
    }

    #[tokio::test]
    async fn test_broadcast_logic() {
        let state = create_test_state();
        let channel_id = "C_200".to_string();
        
        // Setup two sessions
        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        
        let addr: SocketAddr = "127.0.0.1:1111".parse().unwrap();
        let s1 = state.register_session("uuid-1".into(), addr, tx1);
        let s2 = state.register_session("uuid-2".into(), addr, tx2);

        // Create channel
        state.create_channel(channel_id.clone());
        
        // Add s1 to voice group first
        state.add_to_voice_group(channel_id.clone(), s1).await;
        
        // Broadcast joined for s2
        // We need to add s2 to voice group first for logic to work usually? 
        // broadcast_user_joined logic iterates voice group members to send state, 
        // and sends UserJoined to ALL connected sessions.
        state.broadcast_user_joined(channel_id.clone(), s2, "User 2".into()).await;

        // Check s1 received UserJoined
        if let Some(ServiceMessage::UserJoined { channel_id: c, session_id: s, display_name: n }) = rx1.recv().await {
            assert_eq!(c, channel_id);
            assert_eq!(s, s2);
            assert_eq!(n, "User 2");
        } else {
            panic!("s1 did not receive UserJoined");
        }

        // Check s2 received ServerSnapshot (sent when a user joins)
        // broadcast_user_joined sends UserJoined to others, not the joiner
        // The joiner gets ServerSnapshot via send_server_snapshot called separately
        // For this test, s2 should receive a UserJoined about s1 if we called broadcast for both
        // But since we only broadcast s2 joining, s2 won't receive that notification
        // Let's verify s2's channel is empty or just verify s1 got the join
    }

    #[tokio::test]
    async fn test_text_message_routing() {
        use aura_protocol::EncryptedTextPacket;
        let state = create_test_state();
        let channel_id = "C_300".to_string();
        state.create_channel(channel_id.clone());

        // Setup two sessions
        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        
        let addr: SocketAddr = "127.0.0.1:2222".parse().unwrap();
        let s1 = state.register_session("uuid-1".into(), addr, tx1);
        let s2 = state.register_session("uuid-2".into(), addr, tx2);

        // Add both to text group
        state.add_to_text_group(channel_id.clone(), s1).await;
        state.add_to_text_group(channel_id.clone(), s2).await;

        // Create packet
        let packet = EncryptedTextPacket {
            sender_session_id: s1,
            channel_id: channel_id.clone(),
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
        let channel_id = "C_400".to_string();
        state.create_channel(channel_id.clone());
        
        // Add fake sender
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let addr: SocketAddr = "127.0.0.1:3333".parse().unwrap();
        let s1 = state.register_session("uuid-ratchet".into(), addr, tx);
        state.add_to_text_group(channel_id.clone(), s1).await;

        // Send 49 messages (threshold is 50) - use unique message IDs to avoid replay protection
        for i in 0..49 {
            let packet = EncryptedTextPacket {
                sender_session_id: s1,
                channel_id: channel_id.clone(),
                epoch: 1,
                message_id: format!("msg-{}", i),
                ciphertext: vec![],
                nonce: vec![],
                tag: vec![],
                reply_to_id: "".into(),
            };
            let ratchet = state.broadcast_text_message(s1, packet).await;
            assert!(!ratchet, "Should not ratchet yet");
        }

        // 50th message
        let packet50 = EncryptedTextPacket {
            sender_session_id: s1,
            channel_id: channel_id.clone(),
            epoch: 1,
            message_id: "msg-49".into(),
            ciphertext: vec![],
            nonce: vec![],
            tag: vec![],
            reply_to_id: "".into(),
        };
        let ratchet = state.broadcast_text_message(s1, packet50).await;
        assert!(ratchet, "Should trigger ratchet on 50th message");

        // Reset
        state.reset_text_ratchet_counters(channel_id.clone()).await;
        
        // Next message should not ratchet (use new unique ID)
        let packet51 = EncryptedTextPacket {
            sender_session_id: s1,
            channel_id: channel_id.clone(),
            epoch: 1,
            message_id: "msg-50".into(),
            ciphertext: vec![],
            nonce: vec![],
            tag: vec![],
            reply_to_id: "".into(),
        };
        let ratchet = state.broadcast_text_message(s1, packet51).await;
        assert!(!ratchet, "Counter should be reset");
    }
    #[tokio::test]
    async fn test_mls_signaling() {
        use aura_protocol::{MlsEnvelope, mls_envelope, MlsGroupType};
        let state = create_test_state();
        let channel_id = "C_500".to_string();
        state.create_channel(channel_id.clone()); // Creates epoch 0

        // 1. Submit valid COMMIT for Voice Group
        let mut commit_msg = MlsEnvelope {
            group_id: channel_id.clone(),
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
            group_id: channel_id.clone(),
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
            group_id: "C_UNKNOWN".to_string(),
            group_type: MlsGroupType::Voice as i32,
            epoch: 0,
            sender_id: 12345,
            content: Some(mls_envelope::Content::Commit(vec![])),
        };
        assert!(state.handle_mls_message(unknown_msg).await.is_err());
    }
    
    #[test]
    fn test_seen_messages_uniqueness() {
        let seen = SeenMessages::new();
        let channel_id = "C_1".to_string();
        
        // First check should return true (new message)
        assert!(seen.check_and_mark(channel_id.clone(), "msg-1"));
        
        // Second check should return false (replay)
        assert!(!seen.check_and_mark(channel_id.clone(), "msg-1"));
        
        // Different message ID should return true
        assert!(seen.check_and_mark(channel_id.clone(), "msg-2"));
        
        // Same message ID in different channel should return true
        assert!(seen.check_and_mark("C_2".to_string(), "msg-1"));
    }
    
    #[test]
    fn test_seen_messages_count() {
        let seen = SeenMessages::new();
        
        seen.check_and_mark("C_1".to_string(), "msg-001");
        seen.check_and_mark("C_1".to_string(), "msg-002");
        seen.check_and_mark("C_2".to_string(), "msg-003");
        
        assert_eq!(seen.message_count(), 3);
    }
    
    #[test]
    fn test_seen_messages_cleanup() {
        use std::thread;
        use std::time::Duration;
        
        // Note: This test uses a very short delay to simulate expiry
        // In production, SEEN_MESSAGE_TTL_SECS is 300 (5 min)
        
        let seen = SeenMessages::new();
        
        // Add some messages
        seen.check_and_mark("C_1".to_string(), "msg-001");
        seen.check_and_mark("C_1".to_string(), "msg-002");
        
        assert_eq!(seen.message_count(), 2);
        
        // Cleanup should not remove them immediately (TTL not expired)
        seen.cleanup_expired();
        assert_eq!(seen.message_count(), 2);
        
        // Replays should still be detected
        assert!(!seen.check_and_mark("C_1".to_string(), "msg-001"));
    }
}
