//! Server state module.
//!
//! Extracts `ServerState` from main.rs and adds persistence layer.

use crate::auth::{AuthError, AuthService};
use crate::config::{Config, VerificationMode};
use crate::db::Database;
use anyhow::{anyhow, Result};
use aura_protocol::{
    channel_icon, mls_envelope, ChannelIcon as ProtoChannelIcon, ChannelInfo as ProtoChannelInfo,
    EncryptedTextPacket, FastAudioPacket, ListUsersResponse, MlsEnvelope, MlsGroupType,
    ServerState as ProtoServerState, UserInfo, UserProfile,
};
use bytes::Bytes;
use dashmap::{DashMap, DashSet};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    /// Audio in the current format (header authenticated).
    RelayAudio(Bytes),
    /// Audio in the version 1 format. Relayed under its own tag so receivers
    /// can tell the sender to update; no one can decrypt it any more.
    RelayLegacyAudio(Bytes),
    /// User joined a channel - broadcast to ALL connected users
    UserJoined {
        channel_id: String,
        session_id: u32,
        display_name: String,
        user_uuid: String,
    },
    /// User left a channel - broadcast to ALL connected users
    UserLeft { channel_id: String, session_id: u32 },
    /// Full snapshot of server state - sent to new joiners
    ServerSnapshot(ProtoServerState),
    /// Relay encrypted text message to channel members
    RelayText(EncryptedTextPacket),

    // --- MLS Protocol Messages ---
    /// Tell client to create a new MLS group (they are the first joiner)
    MlsCreateGroup { channel_id: String, is_voice: bool },
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
    /// A user's profile (bio, avatar, display name) changed - broadcast to everyone
    /// so other clients can refresh their local caches without a full snapshot.
    ProfileUpdated(UserProfile),
    /// The channel this session is in was deleted. Sent only to its occupants
    /// so they can clear their channel state and move somewhere else.
    ChannelDeleted {
        channel_id: String,
        fallback_channel_id: String,
    },
    /// Ask the group founder to issue an empty commit, advancing the epoch for
    /// forward secrecy. Sent when a text group crosses its ratchet threshold,
    /// or when a voice group has been on one epoch for 15 minutes.
    MlsRatchetRequest { channel_id: String, is_voice: bool },
    /// Tell the group founder which members should still be in the group, so
    /// it commits a Remove for everyone else. Sent after a member leaves the
    /// channel or disconnects. It can only ever shrink the group.
    MlsSyncMembers {
        channel_id: String,
        is_voice: bool,
        member_uuids: Vec<String>,
    },
    /// An admin kicked this session out of `channel_id`. The connection
    /// leaves the channel, if it is still in it, and tells its client.
    Kicked {
        channel_id: String,
        fallback_channel_id: String,
        reason: String,
    },
    /// The account was banned: close the connection.
    Banned,
}

/// Why a moderation request failed.
#[derive(Debug, thiserror::Error)]
pub enum ModerationError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("User is not online")]
    NotOnline,
    #[error("User is not in a channel they can be kicked from")]
    NotInChannel,
}

impl ModerationError {
    /// The error as told to the client (see [`AuthError::client_message`]).
    pub fn client_message(&self) -> String {
        match self {
            ModerationError::Auth(e) => e.client_message(),
            other => other.to_string(),
        }
    }
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
    pub channel_type: i32,
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

/// Who the server believes is in a group's MLS tree, in the order they got
/// there.
///
/// A group's `members` is the *routing* set: everyone in the channel,
/// including sessions still waiting for their Welcome. Only the roster says who
/// actually holds the group's keys. Its first entry is the founder, the one
/// member asked to commit (adds, ratchets, removals), so there is never more
/// than one committer and the group cannot fork. When the founder goes, the
/// next-longest member takes over; when the roster empties, the next joiner
/// starts a fresh group.
#[derive(Debug, Default)]
pub struct MlsRoster {
    members: Vec<(u32, String)>, // (session_id, user_uuid)
}

/// Maximum avatar size accepted in a profile update.
pub const MAX_AVATAR_BYTES: usize = 128 * 1024;
/// Maximum bio size (UTF-8 bytes) accepted in a profile update.
pub const MAX_BIO_BYTES: usize = 16 * 1024;

const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const JPEG_MAGIC: [u8; 3] = [0xFF, 0xD8, 0xFF];

/// Check a profile update's bio and avatar. Errors carry the message shown to
/// the user. An empty avatar means "no avatar".
pub fn validate_profile_update(profile: &UserProfile) -> Result<()> {
    if profile.avatar_data.len() > MAX_AVATAR_BYTES {
        return Err(anyhow!("Avatar is larger than 128 KB"));
    }
    if profile.bio.len() > MAX_BIO_BYTES {
        return Err(anyhow!("Bio is longer than 16 KB"));
    }
    let a = &profile.avatar_data;
    if !a.is_empty() && !a.starts_with(&PNG_MAGIC) && !a.starts_with(&JPEG_MAGIC) {
        return Err(anyhow!("Avatar must be a PNG or JPEG image"));
    }
    Ok(())
}

impl MlsRoster {
    /// The member that commits, if anyone holds the group.
    pub fn founder(&self) -> Option<u32> {
        self.members.first().map(|(session_id, _)| *session_id)
    }

    pub fn contains(&self, session_id: u32) -> bool {
        self.members.iter().any(|(s, _)| *s == session_id)
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    fn insert(&mut self, session_id: u32, user_uuid: String) {
        if !self.contains(session_id) {
            self.members.push((session_id, user_uuid));
        }
    }

    /// Remove a session; true if it was a member.
    fn remove(&mut self, session_id: u32) -> bool {
        let before = self.members.len();
        self.members.retain(|(s, _)| *s != session_id);
        self.members.len() != before
    }

    fn retain_live(&mut self, is_live: impl Fn(u32) -> bool) {
        self.members.retain(|(s, _)| is_live(*s));
    }

    /// The user ids that should remain in the MLS group, without repeats.
    fn user_uuids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::with_capacity(self.members.len());
        for (_, uuid) in &self.members {
            if !ids.contains(uuid) {
                ids.push(uuid.clone());
            }
        }
        ids
    }
}

/// Messages to send once a group's lock is released: `(recipient, message)`.
type MlsActions = Vec<(u32, ServiceMessage)>;

fn add_member_request(channel_id: &str, is_voice: bool, join: &PendingMlsJoin) -> ServiceMessage {
    ServiceMessage::MlsAddMemberRequest {
        channel_id: channel_id.to_string(),
        is_voice,
        joiner_session_id: join.joiner_session_id,
        joiner_uuid: join.joiner_uuid.clone(),
        key_package: join.key_package.clone(),
    }
}

/// A session offered its KeyPackage for a group. Either it starts a fresh
/// group, because nobody holds this one, and then adds everyone already
/// waiting; or its KeyPackage goes to the founder. Returns the messages to
/// send and whether a fresh group was started.
#[allow(clippy::too_many_arguments)]
fn mls_offer_key_package(
    roster: &mut MlsRoster,
    pending: &mut Vec<PendingMlsJoin>,
    routing: &DashSet<u32>,
    channel_id: &str,
    is_voice: bool,
    join: PendingMlsJoin,
    is_live: impl Fn(u32) -> bool,
) -> (MlsActions, bool) {
    roster.retain_live(&is_live);
    pending
        .retain(|p| is_live(p.joiner_session_id) && p.joiner_session_id != join.joiner_session_id);

    match roster.founder() {
        None => {
            let founder = join.joiner_session_id;
            roster.insert(founder, join.joiner_uuid);
            routing.insert(founder);
            let mut actions: MlsActions = vec![(
                founder,
                ServiceMessage::MlsCreateGroup {
                    channel_id: channel_id.to_string(),
                    is_voice,
                },
            )];
            actions.extend(
                pending
                    .iter()
                    .map(|p| (founder, add_member_request(channel_id, is_voice, p))),
            );
            (actions, true)
        }
        Some(founder) => {
            let request = add_member_request(channel_id, is_voice, &join);
            pending.push(join);
            (vec![(founder, request)], false)
        }
    }
}

/// A session left a group's channel (or disconnected). If it was in the MLS
/// tree: hand the founder role on if it held it, and ask the founder to remove
/// it; if nobody is left holding the group, the first session still waiting
/// starts a fresh one. Returns the messages to send and whether a fresh group
/// was started.
fn mls_member_departed(
    roster: &mut MlsRoster,
    pending: &mut Vec<PendingMlsJoin>,
    channel_id: &str,
    is_voice: bool,
    session_id: u32,
    is_live: impl Fn(u32) -> bool,
) -> (MlsActions, bool) {
    pending.retain(|p| p.joiner_session_id != session_id && is_live(p.joiner_session_id));
    let was_founder = roster.founder() == Some(session_id);
    if !roster.remove(session_id) {
        return (Vec::new(), false);
    }
    roster.retain_live(&is_live);

    match roster.founder() {
        None if pending.is_empty() => (Vec::new(), false),
        None => {
            let first = pending.remove(0);
            let founder = first.joiner_session_id;
            roster.insert(founder, first.joiner_uuid);
            let mut actions: MlsActions = vec![(
                founder,
                ServiceMessage::MlsCreateGroup {
                    channel_id: channel_id.to_string(),
                    is_voice,
                },
            )];
            actions.extend(
                pending
                    .iter()
                    .map(|p| (founder, add_member_request(channel_id, is_voice, p))),
            );
            (actions, true)
        }
        Some(founder) => {
            let mut actions: MlsActions = Vec::new();
            if was_founder {
                // The old founder may never have handled these.
                actions.extend(
                    pending
                        .iter()
                        .map(|p| (founder, add_member_request(channel_id, is_voice, p))),
                );
            }
            actions.push((
                founder,
                ServiceMessage::MlsSyncMembers {
                    channel_id: channel_id.to_string(),
                    is_voice,
                    member_uuids: roster.user_uuids(),
                },
            ));
            (actions, false)
        }
    }
}

/// The founder added someone (Commit + Welcome). Returns the sessions the
/// Commit goes to, plus a follow-up for the founder when the newcomer has
/// already gone (their fresh leaf must be removed again); or `None` when
/// `committer` is not the founder. A commit from anyone else would fork the
/// group's epoch, so it is dropped whole, Welcome included.
#[allow(clippy::too_many_arguments)]
fn mls_member_added(
    roster: &mut MlsRoster,
    pending: &mut Vec<PendingMlsJoin>,
    routing: &DashSet<u32>,
    channel_id: &str,
    is_voice: bool,
    committer: u32,
    new_member: u32,
    new_member_uuid: Option<String>,
) -> Option<(Vec<u32>, Option<ServiceMessage>)> {
    if roster.founder() != Some(committer) {
        return None;
    }
    pending.retain(|p| p.joiner_session_id != new_member);

    // `new_member_uuid` is `None` when the session is gone: it left before the
    // founder's commit arrived, so its leaf needs removing straight away.
    let follow_up = match new_member_uuid {
        Some(uuid) => {
            routing.insert(new_member);
            roster.insert(new_member, uuid);
            None
        }
        None => Some(ServiceMessage::MlsSyncMembers {
            channel_id: channel_id.to_string(),
            is_voice,
            member_uuids: roster.user_uuids(),
        }),
    };

    let recipients = routing
        .iter()
        .map(|id| *id)
        .filter(|id| *id != committer && *id != new_member)
        .collect();
    Some((recipients, follow_up))
}

/// The channel a group id (`voice-v1-…` / `text-v1-…`) belongs to.
fn channel_of_group(group_id: &str, is_voice: bool) -> String {
    let prefix = if is_voice { "voice-v1-" } else { "text-v1-" };
    group_id
        .strip_prefix(prefix)
        .unwrap_or(group_id)
        .to_string()
}

/// Voice MLS Group - LOW CHURN
#[derive(Debug)]
pub struct VoiceGroup {
    pub id: String,
    pub channel_id: String,
    pub current_epoch: u64,
    pub members: DashSet<u32>, // Session IDs
    /// Who is in the MLS tree; the first is the founder.
    pub mls: MlsRoster,
    /// Pending key packages from joiners waiting to be added
    pub pending_joins: Vec<PendingMlsJoin>,
    /// When the epoch last advanced by a ratchet commit (for the periodic
    /// forward-secrecy timer)
    pub last_ratchet: Instant,
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
    /// Who is in the MLS tree; the first is the founder.
    pub mls: MlsRoster,
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
/// Voice groups rarely change membership, so without a timer they could keep
/// one epoch (and one set of media keys) for hours. Ask for a ratchet when a
/// group has been on the same epoch this long.
const VOICE_RATCHET_TIME_THRESHOLD_SECS: u64 = 900; // 15 minutes
/// How often the server checks voice groups against that threshold.
pub const VOICE_RATCHET_CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// TTL for seen message IDs (replay protection)
const SEEN_MESSAGE_TTL_SECS: u64 = 300; // 5 minutes

/// How often the server sweeps expired replay-protection entries.
pub const SEEN_MESSAGE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Replay protection: track seen message IDs per channel
#[derive(Debug)]
pub struct SeenMessages {
    /// How long a message ID stays blocked after it was first seen.
    ttl: Duration,
    /// channel_id -> (message_id -> expires_at). A hash map keeps the check
    /// O(1) however many messages a channel has seen within the TTL.
    messages: DashMap<String, HashMap<String, Instant>>,
}

impl Default for SeenMessages {
    fn default() -> Self {
        Self::new()
    }
}

impl SeenMessages {
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(SEEN_MESSAGE_TTL_SECS))
    }

    fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            messages: DashMap::new(),
        }
    }

    /// Check if a message ID has been seen. If not, mark it as seen.
    /// Returns true if the message is NEW (not a replay).
    /// Returns false if the message is a REPLAY (seen within the TTL).
    pub fn check_and_mark(&self, channel_id: String, message_id: &str) -> bool {
        let now = Instant::now();
        let mut entries = self.messages.entry(channel_id).or_default();

        match entries.get_mut(message_id) {
            // Seen within the TTL: replay.
            Some(expires_at) if *expires_at > now => false,
            // Expired but not swept yet: same as never seen.
            Some(expires_at) => {
                *expires_at = now + self.ttl;
                true
            }
            None => {
                entries.insert(message_id.to_owned(), now + self.ttl);
                true
            }
        }
    }

    /// Drop expired entries and channels left empty.
    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        self.messages.retain(|_, entries| {
            entries.retain(|_, expires_at| *expires_at > now);
            !entries.is_empty()
        });
    }

    /// Run [`Self::cleanup_expired`] every `every` until the task is aborted,
    /// so the set holds at most one TTL's worth of message IDs.
    pub fn spawn_cleanup(self: Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(every);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                self.cleanup_expired();
            }
        })
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

    /// Connection registry and bookkeeping for encrypted file transfers.
    pub file_relay: crate::file_relay::FileRelay,
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
            file_relay: crate::file_relay::FileRelay::default(),
        };

        // Load channels from DB
        match db.get_all_channels() {
            Ok(channels) => {
                for (id, name, comment, i_type, i_data, pos, c_type) in channels {
                    state.channel_metadata.insert(
                        id.clone(),
                        ChannelMetadata {
                            id: id.clone(),
                            name,
                            comment,
                            icon_type: i_type,
                            icon_data: i_data,
                            position: pos,
                            channel_type: c_type,
                        },
                    );
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
    pub fn register_session(
        &self,
        user_uuid: String,
        socket_addr: SocketAddr,
        sender: tokio::sync::mpsc::UnboundedSender<ServiceMessage>,
    ) -> u32 {
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
                info!(
                    "Registered session {} for user {} (Profile cached)",
                    session_id, user.display_name
                );
            }
            Ok(None) => warn!(
                "Registered session {} for unknown user {}",
                session_id, user_uuid
            ),
            Err(e) => warn!("Failed to fetch profile for user {}: {}", user_uuid, e),
        }

        session_id
    }

    /// Remove a client session.
    pub async fn remove_session(&self, session_id: u32) {
        if let Some((_, session)) = self.sessions.remove(&session_id) {
            // Still in a channel: the connection's own cleanup normally leaves
            // first, so this is for sessions removed from elsewhere (a deleted
            // account). The session fields hold *group* ids; UserLeft wants
            // the channel id.
            if let Some(voice_id) = session.voice_group_id {
                let channel_id = channel_of_group(&voice_id, true);
                self.broadcast_user_left(channel_id.clone(), session_id)
                    .await;
                self.leave_mls_group(&channel_id, true, session_id).await;
            }
            if let Some(text_id) = session.text_group_id {
                let channel_id = channel_of_group(&text_id, false);
                self.leave_mls_group(&channel_id, false, session_id).await;
            }
            info!("Removed session {}", session_id);

            // Only remove the profile cache entry if no other active session
            // shares this user_uuid. A fast reconnect can register a new session
            // before the old one is fully torn down — we must not wipe the new
            // session's profile in that case.
            let still_active = self
                .sessions
                .iter()
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

    /// Whether `channel_id` names a channel that exists.
    pub fn channel_exists(&self, channel_id: &str) -> bool {
        self.channel_metadata.contains_key(channel_id)
    }

    /// The lobby's channel id, if the server has a lobby. This is where
    /// clients are sent when they lose their channel.
    pub fn lobby_channel_id(&self) -> Option<String> {
        self.channel_metadata
            .iter()
            .find(|entry| entry.channel_type == aura_protocol::ChannelType::Lobby as i32)
            .map(|entry| entry.id.clone())
    }

    /// The channel a session is in, found from its voice group. `None` when
    /// it isn't in a channel that exists.
    fn session_channel_id(&self, session: &ClientSession) -> Option<String> {
        let voice_group_id = session.voice_group_id.as_deref()?;
        self.channel_metadata
            .iter()
            .find(|entry| aura_protocol::make_mls_group_id(&entry.id, true) == voice_group_id)
            .map(|entry| entry.id.clone())
    }

    /// Create a channel with both voice and text groups.
    pub fn create_channel(&self, channel_id: String) {
        let voice_group_id = aura_protocol::make_mls_group_id(&channel_id, true);
        let text_group_id = aura_protocol::make_mls_group_id(&channel_id, false);

        // Only create if not exists - don't overwrite existing groups!
        self.voice_groups
            .entry(voice_group_id.clone())
            .or_insert_with(|| {
                Arc::new(RwLock::new(VoiceGroup {
                    id: voice_group_id,
                    channel_id: channel_id.clone(),
                    current_epoch: 0,
                    members: DashSet::new(),
                    mls: MlsRoster::default(),
                    pending_joins: Vec::new(),
                    last_ratchet: Instant::now(),
                }))
            });

        self.text_groups
            .entry(text_group_id.clone())
            .or_insert_with(|| {
                Arc::new(RwLock::new(TextGroup {
                    id: text_group_id,
                    current_epoch: 0,
                    members: DashSet::new(),
                    message_count: AtomicU32::new(0),
                    last_ratchet: Instant::now(),
                    mls: MlsRoster::default(),
                    pending_joins: Vec::new(),
                }))
            });

        info!(
            "Created channel {} with Voice ({}) and Text ({}) MLS groups",
            channel_id,
            aura_protocol::make_mls_group_id(&channel_id, true),
            aura_protocol::make_mls_group_id(&channel_id, false)
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

            if let Some(group_lock) = self
                .voice_groups
                .get(&aura_protocol::make_mls_group_id(&meta.id, true))
            {
                let group = group_lock.read().await;
                for id in group.members.iter() {
                    if let Some(sess) = self.sessions.get(&id) {
                        let display_name = self
                            .profiles
                            .get(&sess.user_uuid)
                            .map(|p| p.display_name.clone())
                            .unwrap_or_else(|| format!("User {}", *id));
                        users.push(aura_protocol::ChannelUserStatus {
                            session_id: *id,
                            is_muted: sess.is_muted,
                            is_deafened: sess.is_deafened,
                            display_name,
                            user_uuid: sess.user_uuid.clone(),
                        });
                    } else {
                        users.push(aura_protocol::ChannelUserStatus {
                            session_id: *id,
                            is_muted: false,
                            is_deafened: false,
                            display_name: format!("User {}", *id),
                            user_uuid: String::new(),
                        });
                    }
                }
            }

            let icon = match meta.icon_type {
                1 => Some(ProtoChannelIcon {
                    icon: Some(channel_icon::Icon::Emoji(
                        String::from_utf8_lossy(&meta.icon_data).into(),
                    )),
                }),
                2 => Some(ProtoChannelIcon {
                    icon: Some(channel_icon::Icon::PresetId(
                        String::from_utf8_lossy(&meta.icon_data).into(),
                    )),
                }),
                3 => Some(ProtoChannelIcon {
                    icon: Some(channel_icon::Icon::CustomData(meta.icon_data.clone())),
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
                r#type: meta.channel_type,
            });
        }

        // DashMap iteration order is unspecified; sort so clients see a
        // stable, position-respecting order (mirrors the DB's
        // `ORDER BY position, channel_id` in `Database::get_all_channels`).
        channels.sort_by(|a, b| {
            a.position
                .cmp(&b.position)
                .then_with(|| a.channel_id.cmp(&b.channel_id))
        });

        let profiles: Vec<UserProfile> = self.profiles.iter().map(|p| p.value().clone()).collect();

        info!(
            "[Snapshot] Sending {} channels and {} profiles",
            channels.len(),
            profiles.len()
        );

        ProtoServerState { channels, profiles }
    }

    /// Broadcast that a user joined a channel to ALL connected users.
    pub async fn broadcast_user_joined(
        &self,
        channel_id: String,
        session_id: u32,
        display_name: String,
    ) {
        let user_uuid = self
            .sessions
            .get(&session_id)
            .map(|s| s.user_uuid.clone())
            .unwrap_or_default();

        // Broadcast UserJoined to ALL connected users
        for sess in self.sessions.iter() {
            if *sess.key() != session_id {
                let _ = sess.sender.send(ServiceMessage::UserJoined {
                    channel_id: channel_id.clone(),
                    session_id,
                    display_name: display_name.clone(),
                    user_uuid: user_uuid.clone(),
                });
            }
        }

        // Send full server snapshot to the new joiner
        if let Some(new_sess) = self.sessions.get(&session_id) {
            let snapshot = self.get_server_snapshot().await;
            let _ = new_sess
                .sender
                .send(ServiceMessage::ServerSnapshot(snapshot));
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
        let (is_muted, is_deafened) = if is_deafened {
            (true, true)
        } else {
            (is_muted, is_deafened)
        };

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
    pub async fn broadcast_text_message(
        &self,
        sender_session_id: u32,
        packet: EncryptedTextPacket,
    ) -> bool {
        let channel_id = packet.channel_id.clone();
        let message_id = &packet.message_id;

        // Replay protection: Check if we've seen this message ID before
        if !self
            .seen_messages
            .check_and_mark(channel_id.clone(), message_id)
        {
            crate::metrics::METRICS.text_replay_rejected();
            warn!(
                "Replay attack detected! Duplicate message ID '{}' from session {} in channel {}",
                message_id, sender_session_id, channel_id
            );
            return false;
        }

        // Verify sender is a member of this text group
        let text_group_id = aura_protocol::make_mls_group_id(&channel_id, false);
        let group_lock = match self.text_groups.get(&text_group_id) {
            Some(g) => g.clone(),
            None => {
                warn!("Text group not found for channel {}", channel_id);
                return false;
            }
        };

        let group = group_lock.read().await;

        if !group.members.contains(&sender_session_id) {
            warn!(
                "Session {} not a member of text group {}",
                sender_session_id, channel_id
            );
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
                let _ = session
                    .sender
                    .send(ServiceMessage::RelayText(packet.clone()));
            }
        }

        info!(
            "Relayed text message from {} to channel {} (msg #{})",
            sender_session_id, channel_id, msg_count
        );

        // Check if we need to ratchet
        self.should_ratchet_text_group(channel_id).await
    }

    /// Check if a text group should ratchet based on message count or time.
    /// Batched ratcheting: every 50 messages OR every 5 minutes.
    pub async fn should_ratchet_text_group(&self, channel_id: String) -> bool {
        let group_lock = match self
            .text_groups
            .get(&aura_protocol::make_mls_group_id(&channel_id, false))
        {
            Some(g) => g.clone(),
            None => return false,
        };

        let group = group_lock.read().await;
        let msg_count = group.message_count.load(Ordering::Relaxed);
        let elapsed = group.last_ratchet.elapsed().as_secs();

        msg_count >= TEXT_RATCHET_MESSAGE_THRESHOLD || elapsed >= TEXT_RATCHET_TIME_THRESHOLD_SECS
    }

    /// Ask a text group's founder to advance the epoch for forward secrecy.
    ///
    /// The server is the MLS Delivery Service and cannot commit on its own — it
    /// can only nudge the member that owns the group. The ratchet counters are
    /// reset up front so a burst of messages doesn't queue up a request per
    /// message while the first one is still in flight.
    pub async fn request_text_ratchet(&self, channel_id: String) {
        let group_id = aura_protocol::make_mls_group_id(&channel_id, false);
        let Some(group_lock) = self.text_groups.get(&group_id).map(|g| g.clone()) else {
            return;
        };

        let founder_session_id = {
            let mut group = group_lock.write().await;
            // Reset before dispatching so concurrent senders don't each trigger
            // their own request for the same threshold crossing.
            group.message_count.store(0, Ordering::Relaxed);
            group.last_ratchet = Instant::now();
            group.mls.founder()
        };

        let Some(founder_session_id) = founder_session_id else {
            return;
        };

        if let Some(founder) = self.sessions.get(&founder_session_id) {
            info!(
                "Asking founder {} to ratchet text group {}",
                founder_session_id, channel_id
            );
            let _ = founder.sender.send(ServiceMessage::MlsRatchetRequest {
                channel_id,
                is_voice: false,
            });
        }
    }

    /// Ask the founder of each voice group that has been on the same epoch
    /// for `VOICE_RATCHET_TIME_THRESHOLD_SECS` to issue an empty commit.
    /// Called every `VOICE_RATCHET_CHECK_INTERVAL` from `main`.
    pub async fn request_due_voice_ratchets(&self) {
        self.request_due_voice_ratchets_after(Duration::from_secs(
            VOICE_RATCHET_TIME_THRESHOLD_SECS,
        ))
        .await;
    }

    /// [`request_due_voice_ratchets`](Self::request_due_voice_ratchets) with
    /// the threshold as a parameter, so tests don't wait 15 minutes.
    pub async fn request_due_voice_ratchets_after(&self, threshold: Duration) {
        let group_locks: Vec<Arc<RwLock<VoiceGroup>>> = self
            .voice_groups
            .iter()
            .map(|e| e.value().clone())
            .collect();

        for group_lock in group_locks {
            let due = {
                let mut group = group_lock.write().await;
                if group.members.is_empty() || group.last_ratchet.elapsed() < threshold {
                    None
                } else if let Some(founder_session_id) = group.mls.founder() {
                    // Reset up front so a slow or disconnected founder isn't
                    // asked again on every check.
                    group.last_ratchet = Instant::now();
                    Some((founder_session_id, group.channel_id.clone()))
                } else {
                    None
                }
            };

            if let Some((founder_session_id, channel_id)) = due {
                if let Some(founder) = self.sessions.get(&founder_session_id) {
                    info!(
                        "Asking founder {} to ratchet voice group {}",
                        founder_session_id, channel_id
                    );
                    let _ = founder.sender.send(ServiceMessage::MlsRatchetRequest {
                        channel_id,
                        is_voice: true,
                    });
                }
            }
        }
    }

    /// Distribute a commit that carries no Welcome (a ratchet, or the removals
    /// answering `MlsSyncMembers`) to every group member but the committer,
    /// and advance the server's epoch counter. Only the founder's is accepted.
    ///
    /// Distinct from [`handle_mls_commit_welcome`](Self::handle_mls_commit_welcome):
    /// there is no new member and no Welcome, so nothing must be added to the
    /// member set.
    pub async fn handle_mls_ratchet_commit(
        &self,
        channel_id: String,
        is_voice: bool,
        committer_session_id: u32,
        commit: Vec<u8>,
    ) {
        // Only the founder commits; anyone else's commit would fork the epoch.
        let members: Option<Vec<u32>> = if is_voice {
            let group_id = aura_protocol::make_mls_group_id(&channel_id, true);
            let Some(group_lock) = self.voice_groups.get(&group_id).map(|g| g.clone()) else {
                return;
            };
            let mut group = group_lock.write().await;
            if group.mls.founder() == Some(committer_session_id) {
                group.current_epoch += 1;
                group.last_ratchet = Instant::now();
                Some(group.members.iter().map(|id| *id).collect())
            } else {
                None
            }
        } else {
            let group_id = aura_protocol::make_mls_group_id(&channel_id, false);
            let Some(group_lock) = self.text_groups.get(&group_id).map(|g| g.clone()) else {
                return;
            };
            let mut group = group_lock.write().await;
            if group.mls.founder() == Some(committer_session_id) {
                group.current_epoch += 1;
                group.last_ratchet = Instant::now();
                Some(group.members.iter().map(|id| *id).collect())
            } else {
                None
            }
        };

        let Some(members) = members else {
            warn!(
                "[MLS] Dropping commit for {} group {} from session {}: not the founder",
                if is_voice { "voice" } else { "text" },
                channel_id,
                committer_session_id
            );
            return;
        };

        let mut delivered = 0;
        for member_id in members {
            if member_id == committer_session_id {
                continue;
            }
            if let Some(session) = self.sessions.get(&member_id) {
                let _ = session.sender.send(ServiceMessage::MlsCommit {
                    channel_id: channel_id.clone(),
                    is_voice,
                    commit: commit.clone(),
                });
                delivered += 1;
            }
        }

        info!(
            "[MLS] Distributed ratchet commit for {} group {} to {} member(s)",
            if is_voice { "voice" } else { "text" },
            channel_id,
            delivered
        );
    }

    /// Reset ratchet counters after a successful epoch advance.
    pub async fn reset_text_ratchet_counters(&self, channel_id: String) {
        let group_id = aura_protocol::make_mls_group_id(&channel_id, false);
        let group_lock = match self.text_groups.get(&group_id) {
            Some(g) => g.clone(),
            None => return,
        };

        let mut group = group_lock.write().await;
        group.message_count.store(0, Ordering::Relaxed);
        group.last_ratchet = Instant::now();
        info!("Reset ratchet counters for text group {}", group_id);
    }

    // --- Group Membership Helpers ---

    pub async fn add_to_voice_group(&self, channel_id: String, session_id: u32) {
        let voice_group_id = aura_protocol::make_mls_group_id(&channel_id, true);
        if let Some(group) = self.voice_groups.get(&voice_group_id) {
            group.value().write().await.members.insert(session_id);
        }

        // Set voice_group_id on session so audio routing works
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.voice_group_id = Some(voice_group_id);
        }
    }

    pub async fn remove_from_voice_group(&self, channel_id: String, session_id: u32) {
        self.leave_mls_group(&channel_id, true, session_id).await;

        // Clear voice_group_id on session
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.voice_group_id = None;
        }
    }

    /// A session leaves a channel's voice or text group: stop routing to it,
    /// and if it was in the MLS tree, have the founder remove it (handing the
    /// founder role on first if it held it). Without the removal it would stay
    /// a member and could keep following the group's keys from any commits it
    /// still saw.
    async fn leave_mls_group(&self, channel_id: &str, is_voice: bool, session_id: u32) {
        let group_id = aura_protocol::make_mls_group_id(channel_id, is_voice);
        let is_live = |s: u32| s != session_id && self.sessions.contains_key(&s);

        let (actions, fresh) = if is_voice {
            let Some(group_lock) = self.voice_groups.get(&group_id).map(|g| g.clone()) else {
                return;
            };
            let mut guard = group_lock.write().await;
            let group = &mut *guard;
            group.members.remove(&session_id);
            let (actions, fresh) = mls_member_departed(
                &mut group.mls,
                &mut group.pending_joins,
                channel_id,
                true,
                session_id,
                is_live,
            );
            if fresh {
                group.current_epoch = 0;
                group.last_ratchet = Instant::now();
            }
            (actions, fresh)
        } else {
            let Some(group_lock) = self.text_groups.get(&group_id).map(|g| g.clone()) else {
                return;
            };
            let mut guard = group_lock.write().await;
            let group = &mut *guard;
            group.members.remove(&session_id);
            let (actions, fresh) = mls_member_departed(
                &mut group.mls,
                &mut group.pending_joins,
                channel_id,
                false,
                session_id,
                is_live,
            );
            if fresh {
                group.current_epoch = 0;
                group.message_count.store(0, Ordering::Relaxed);
                group.last_ratchet = Instant::now();
            }
            (actions, fresh)
        };

        if fresh {
            info!(
                "[MLS] {} group {} emptied; a waiting session starts it afresh",
                if is_voice { "Voice" } else { "Text" },
                channel_id
            );
        } else if !actions.is_empty() {
            info!(
                "[MLS] Session {} left {} group {}; asking the founder to remove it",
                session_id,
                if is_voice { "voice" } else { "text" },
                channel_id
            );
        }
        self.dispatch_mls(actions);
    }

    pub async fn add_to_text_group(&self, channel_id: String, session_id: u32) {
        let text_group_id = aura_protocol::make_mls_group_id(&channel_id, false);
        if let Some(group) = self.text_groups.get(&text_group_id) {
            group.value().write().await.members.insert(session_id);
        }

        // Set text_group_id on session
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.text_group_id = Some(text_group_id);
        }
    }

    pub async fn remove_from_text_group(&self, channel_id: String, session_id: u32) {
        self.leave_mls_group(&channel_id, false, session_id).await;

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
        if !self.sessions.contains_key(&session_id) {
            return;
        }

        if is_voice {
            self.handle_voice_mls_join(channel_id, session_id, user_uuid, key_package)
                .await;
        } else {
            self.handle_text_mls_join(channel_id, session_id, user_uuid, key_package)
                .await;
        }
    }

    async fn handle_voice_mls_join(
        &self,
        channel_id: String,
        session_id: u32,
        user_uuid: String,
        key_package: Vec<u8>,
    ) {
        let group_id = aura_protocol::make_mls_group_id(&channel_id, true);
        let group_lock = self
            .voice_groups
            .entry(group_id.clone())
            .or_insert_with(|| {
                info!(
                    "[MLS] Lazily initializing Voice group for channel {}",
                    channel_id
                );
                Arc::new(RwLock::new(VoiceGroup {
                    id: group_id.clone(),
                    channel_id: channel_id.clone(),
                    current_epoch: 0,
                    members: DashSet::new(),
                    mls: MlsRoster::default(),
                    pending_joins: Vec::new(),
                    last_ratchet: Instant::now(),
                }))
            })
            .clone();

        let (actions, fresh) = {
            let mut guard = group_lock.write().await;
            let group = &mut *guard;
            let (actions, fresh) = mls_offer_key_package(
                &mut group.mls,
                &mut group.pending_joins,
                &group.members,
                &channel_id,
                true,
                PendingMlsJoin {
                    joiner_session_id: session_id,
                    joiner_uuid: user_uuid,
                    key_package,
                },
                |s| self.sessions.contains_key(&s),
            );
            if fresh {
                group.current_epoch = 0;
                group.last_ratchet = Instant::now();
            }
            (actions, fresh)
        };

        if fresh {
            info!(
                "[MLS] Session {} is the new founder of voice group {}",
                session_id, channel_id
            );
        } else {
            info!(
                "[MLS] Forwarding key package from {} to the founder of voice group {}",
                session_id, channel_id
            );
        }
        self.dispatch_mls(actions);
    }

    async fn handle_text_mls_join(
        &self,
        channel_id: String,
        session_id: u32,
        user_uuid: String,
        key_package: Vec<u8>,
    ) {
        let group_id = aura_protocol::make_mls_group_id(&channel_id, false);
        let group_lock = self
            .text_groups
            .entry(group_id.clone())
            .or_insert_with(|| {
                info!(
                    "[MLS] Lazily initializing Text group for channel {}",
                    channel_id
                );
                Arc::new(RwLock::new(TextGroup {
                    id: group_id.clone(),
                    current_epoch: 0,
                    members: DashSet::new(),
                    message_count: AtomicU32::new(0),
                    last_ratchet: Instant::now(),
                    mls: MlsRoster::default(),
                    pending_joins: Vec::new(),
                }))
            })
            .clone();

        let (actions, fresh) = {
            let mut guard = group_lock.write().await;
            let group = &mut *guard;
            let (actions, fresh) = mls_offer_key_package(
                &mut group.mls,
                &mut group.pending_joins,
                &group.members,
                &channel_id,
                false,
                PendingMlsJoin {
                    joiner_session_id: session_id,
                    joiner_uuid: user_uuid,
                    key_package,
                },
                |s| self.sessions.contains_key(&s),
            );
            if fresh {
                group.current_epoch = 0;
                group.message_count.store(0, Ordering::Relaxed);
                group.last_ratchet = Instant::now();
            }
            (actions, fresh)
        };

        if fresh {
            info!(
                "[MLS] Session {} is the new founder of text group {}",
                session_id, channel_id
            );
        }
        self.dispatch_mls(actions);
    }

    /// Send what an MLS membership change decided, once the group's lock is
    /// released.
    fn dispatch_mls(&self, actions: MlsActions) {
        for (recipient, message) in actions {
            if let Some(session) = self.sessions.get(&recipient) {
                let _ = session.sender.send(message);
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
        let new_member_uuid = self
            .sessions
            .get(&new_member_session_id)
            .map(|s| s.user_uuid.clone());

        // Bookkeeping first, under the group's lock: only the founder's commit
        // is accepted, and only then does anything go out.
        let outcome = if is_voice {
            let group_id = aura_protocol::make_mls_group_id(&channel_id, true);
            let Some(group_lock) = self.voice_groups.get(&group_id).map(|g| g.clone()) else {
                return;
            };
            let mut guard = group_lock.write().await;
            let group = &mut *guard;
            let outcome = mls_member_added(
                &mut group.mls,
                &mut group.pending_joins,
                &group.members,
                &channel_id,
                true,
                committer_session_id,
                new_member_session_id,
                new_member_uuid,
            );
            if outcome.is_some() {
                group.current_epoch += 1;
            }
            outcome
        } else {
            let group_id = aura_protocol::make_mls_group_id(&channel_id, false);
            let Some(group_lock) = self.text_groups.get(&group_id).map(|g| g.clone()) else {
                return;
            };
            let mut guard = group_lock.write().await;
            let group = &mut *guard;
            let outcome = mls_member_added(
                &mut group.mls,
                &mut group.pending_joins,
                &group.members,
                &channel_id,
                false,
                committer_session_id,
                new_member_session_id,
                new_member_uuid,
            );
            if outcome.is_some() {
                group.current_epoch += 1;
            }
            outcome
        };

        let Some((recipients, follow_up)) = outcome else {
            warn!(
                "[MLS] Dropping commit for {} group {} from session {}: not the founder",
                if is_voice { "voice" } else { "text" },
                channel_id,
                committer_session_id
            );
            return;
        };

        if let Some(new_session) = self.sessions.get(&new_member_session_id) {
            info!(
                "[MLS] Sending Welcome to session {} for {} group {}",
                new_member_session_id,
                if is_voice { "voice" } else { "text" },
                channel_id
            );
            let _ = new_session.sender.send(ServiceMessage::MlsWelcome {
                channel_id: channel_id.clone(),
                is_voice,
                welcome,
            });
        }

        for member_id in recipients {
            if let Some(session) = self.sessions.get(&member_id) {
                let _ = session.sender.send(ServiceMessage::MlsCommit {
                    channel_id: channel_id.clone(),
                    is_voice,
                    commit: commit.clone(),
                });
            }
        }

        if let Some(sync) = follow_up {
            self.dispatch_mls(vec![(committer_session_id, sync)]);
        }

        info!(
            "[MLS] Distributed commit for {} group {} (new member: {})",
            if is_voice { "voice" } else { "text" },
            channel_id,
            new_member_session_id
        );
    }

    // --- MLS Delivery Service (Reliable Signaling) ---

    /// Process an incoming MLS Message.
    pub async fn handle_mls_message(&self, msg: MlsEnvelope) -> Result<MlsSignalResponse> {
        let channel_id = msg.channel_id.clone();
        let group_type = msg.group_type();

        match group_type {
            MlsGroupType::Voice => self.handle_voice_mls(channel_id, msg).await,
            MlsGroupType::Text => self.handle_text_mls(channel_id, msg).await,
            MlsGroupType::Invalid => Err(anyhow!("Invalid MLS group type")),
        }
    }

    /// Handle Voice MLS message - LOW CHURN rules
    async fn handle_voice_mls(
        &self,
        channel_id: String,
        msg: MlsEnvelope,
    ) -> Result<MlsSignalResponse> {
        let group_lock = match self.voice_groups.get(&channel_id) {
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
                    channel_id, group.current_epoch
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
    async fn handle_text_mls(
        &self,
        channel_id: String,
        msg: MlsEnvelope,
    ) -> Result<MlsSignalResponse> {
        let group_lock = match self.text_groups.get(&channel_id) {
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

                // Reset ratchet counters on epoch advance
                group.message_count.store(0, Ordering::Relaxed);
                group.last_ratchet = Instant::now();

                info!(
                    "Text Group {} advanced to Epoch {}",
                    channel_id, group.current_epoch
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
    pub async fn create_channel_persistent(
        &self,
        name: String,
        comment: String,
        icon: Option<ProtoChannelIcon>,
    ) -> Result<String> {
        let (icon_type, icon_data) = self.convert_proto_icon(icon);
        let channel_type = 0; // Default to Regular for manually created channels
                              // Append after existing channels instead of colliding at 0 — new
                              // channels previously all landed at position 0, leaving their
                              // relative order to DashMap iteration (see get_server_snapshot).
        let position = self.channel_metadata.len() as i32;

        let channel_id = self.db.upsert_channel(
            None,
            &name,
            &comment,
            icon_type,
            &icon_data,
            position,
            channel_type,
        )?;

        // Update in-memory metadata
        self.channel_metadata.insert(
            channel_id.clone(),
            ChannelMetadata {
                id: channel_id.clone(),
                name,
                comment,
                icon_type,
                icon_data,
                position,
                channel_type,
            },
        );

        // Initialize MLS groups
        self.create_channel(channel_id.clone());

        // Broadcast full state update to everyone
        let snapshot = self.get_server_snapshot().await;
        for sess in self.sessions.iter() {
            let _ = sess
                .sender
                .send(ServiceMessage::ServerSnapshot(snapshot.clone()));
        }

        Ok(channel_id)
    }

    /// Update channel metadata persistently.
    pub async fn update_channel_persistent(
        &self,
        channel_id: String,
        name: Option<String>,
        comment: Option<String>,
        icon: Option<ProtoChannelIcon>,
        position: Option<i32>,
    ) -> Result<()> {
        let mut meta = self
            .channel_metadata
            .get_mut(&channel_id)
            .ok_or_else(|| anyhow!("Channel not found"))?;

        if let Some(n) = name {
            meta.name = n;
        }
        if let Some(c) = comment {
            meta.comment = c;
        }
        if let Some(i) = icon {
            let (t, d) = self.convert_proto_icon(Some(i));
            meta.icon_type = t;
            meta.icon_data = d;
        }
        if let Some(p) = position {
            meta.position = p;
        }

        // Persist to DB
        self.db.upsert_channel(
            Some(channel_id.clone()),
            &meta.name,
            &meta.comment,
            meta.icon_type,
            &meta.icon_data,
            meta.position,
            meta.channel_type,
        )?;

        drop(meta); // Release lock

        // Broadcast full state update
        let snapshot = self.get_server_snapshot().await;
        for sess in self.sessions.iter() {
            let _ = sess
                .sender
                .send(ServiceMessage::ServerSnapshot(snapshot.clone()));
        }

        Ok(())
    }

    /// Update user profile persistently.
    ///
    /// The client-supplied `display_name` is ignored: the account's
    /// authenticated name (validated at registration) is what gets stored,
    /// cached and broadcast. Bio and avatar are checked by
    /// [`validate_profile_update`].
    pub async fn update_profile_persistent(
        &self,
        session_id: u32,
        mut profile: UserProfile,
    ) -> Result<()> {
        let session = self
            .sessions
            .get(&session_id)
            .ok_or_else(|| anyhow!("Session not found"))?;
        let user_uuid = session.user_uuid.clone();
        drop(session);

        validate_profile_update(&profile)?;

        let user = self
            .db
            .find_user_by_uuid(&user_uuid)?
            .ok_or_else(|| anyhow!("Account not found"))?;
        profile.user_id = user_uuid.clone();
        profile.display_name = user.display_name;

        // Update DB
        self.db.upsert_user_profile(
            &user_uuid,
            &profile.bio,
            &profile.avatar_data,
            &profile.signature,
            &profile.signing_key,
        )?;

        // Update in-memory cache
        self.profiles.insert(user_uuid, profile.clone());

        // Broadcast the delta to every connected session (including the
        // originator, so their own in-memory view reflects what the server
        // persisted). Cheap compared to shipping a full ServerSnapshot.
        for sess in self.sessions.iter() {
            let _ = sess
                .sender
                .send(ServiceMessage::ProfileUpdated(profile.clone()));
        }

        Ok(())
    }

    /// Delete a channel persistently.
    ///
    /// Occupants are evicted explicitly: they get a `UserLeft` broadcast (so
    /// every client drops them from the roster) and a `ChannelDeleted` notice
    /// (so their own client stops treating the dead channel as current).
    /// Without both, occupants linger in a channel that no longer exists.
    pub async fn delete_channel_persistent(&self, channel_id: &str) -> Result<()> {
        // Update DB
        self.db.delete_channel(channel_id)?;

        // Update in-memory metadata
        self.channel_metadata.remove(channel_id);

        // Collect occupants before tearing the groups down, so they can be
        // notified afterwards.
        let voice_group_id = aura_protocol::make_mls_group_id(channel_id, true);
        let mut occupants: Vec<u32> = Vec::new();

        if let Some(voice_group) = self.voice_groups.get(&voice_group_id) {
            let members = { voice_group.read().await.members.clone() };
            for session_id in members {
                occupants.push(session_id);
                self.remove_from_voice_group(channel_id.to_string(), session_id)
                    .await;
            }
        }
        self.voice_groups.remove(&voice_group_id);

        let text_group_id = aura_protocol::make_mls_group_id(channel_id, false);
        if let Some(text_group) = self.text_groups.get(&text_group_id) {
            let members = { text_group.read().await.members.clone() };
            for session_id in members {
                if !occupants.contains(&session_id) {
                    occupants.push(session_id);
                }
                self.remove_from_text_group(channel_id.to_string(), session_id)
                    .await;
            }
        }
        self.text_groups.remove(&text_group_id);

        // Tell everyone the occupants are gone from this channel.
        for session_id in &occupants {
            self.broadcast_user_left(channel_id.to_string(), *session_id)
                .await;
        }

        // Suggest the lobby as a landing spot, if the server has one.
        let fallback_channel_id = self.lobby_channel_id().unwrap_or_default();

        // Tell the occupants themselves that their channel is gone.
        for session_id in &occupants {
            if let Some(session) = self.sessions.get(session_id) {
                let _ = session.sender.send(ServiceMessage::ChannelDeleted {
                    channel_id: channel_id.to_string(),
                    fallback_channel_id: fallback_channel_id.clone(),
                });
            }
        }

        if !occupants.is_empty() {
            info!(
                "Evicted {} session(s) from deleted channel {}",
                occupants.len(),
                channel_id
            );
        }

        // Broadcast full state update to everyone
        let snapshot = self.get_server_snapshot().await;
        for sess in self.sessions.iter() {
            let _ = sess
                .sender
                .send(ServiceMessage::ServerSnapshot(snapshot.clone()));
        }

        Ok(())
    }

    // --- Moderation ---

    /// List users for `actor_uuid` (any admin; see [`AuthService::list_users`]).
    pub fn list_users(
        &self,
        actor_uuid: &str,
        offset: u32,
        limit: u32,
    ) -> Result<ListUsersResponse, AuthError> {
        let users = self.auth.list_users(actor_uuid, offset, limit)?;
        let admins = self.db.admin_user_uuids()?;
        let total_count = self.db.user_count()?;

        let users = users
            .into_iter()
            .map(|user| UserInfo {
                is_admin: admins.contains(&user.user_uuid),
                user_uuid: user.user_uuid,
                display_name: user.display_name,
                verified: user.verified,
                banned: user.banned,
                created_at: user.created_at,
            })
            .collect();

        Ok(ListUsersResponse {
            users,
            total_count,
            success: true,
            error_message: String::new(),
        })
    }

    /// Ban or unban `target_uuid` on behalf of `actor_uuid` (see
    /// [`AuthService::ban_user`] for who may). A ban also closes every
    /// connection the target has open: the database flag alone only refuses
    /// their next login.
    pub fn set_user_banned(
        &self,
        actor_uuid: &str,
        target_uuid: &str,
        banned: bool,
    ) -> Result<(), AuthError> {
        if !banned {
            return self.auth.unban_user(actor_uuid, target_uuid);
        }

        self.auth.ban_user(actor_uuid, target_uuid)?;
        let mut dropped = 0;
        for session in self.sessions.iter() {
            if session.user_uuid == target_uuid {
                let _ = session.sender.send(ServiceMessage::Banned);
                dropped += 1;
            }
        }
        info!(
            "User {} banned by {}; closing {} connection(s)",
            target_uuid, actor_uuid, dropped
        );
        Ok(())
    }

    /// Kick `target_uuid` out of the channel they are in, on behalf of
    /// `actor_uuid` (see [`AuthService::authorize_kick`] for who may).
    ///
    /// Each of the target's sessions that is in a channel other than the
    /// lobby is told to leave it, with the lobby as the place to go. The
    /// target's connection does the leaving (as for any channel switch) and
    /// sends the client a `ChannelDeleted` notice with reason `KICKED`.
    /// Returns how many sessions were kicked.
    pub fn kick_user(
        &self,
        actor_uuid: &str,
        target_uuid: &str,
        reason: &str,
    ) -> Result<usize, ModerationError> {
        self.auth.authorize_kick(actor_uuid, target_uuid)?;

        let lobby = self.lobby_channel_id();
        let mut online = false;
        let mut kicked = 0;
        for session in self.sessions.iter() {
            if session.user_uuid != target_uuid {
                continue;
            }
            online = true;
            let Some(channel_id) = self.session_channel_id(&session) else {
                continue;
            };
            if lobby.as_deref() == Some(channel_id.as_str()) {
                continue;
            }
            let _ = session.sender.send(ServiceMessage::Kicked {
                channel_id,
                fallback_channel_id: lobby.clone().unwrap_or_default(),
                reason: reason.to_string(),
            });
            kicked += 1;
        }

        if !online {
            return Err(ModerationError::NotOnline);
        }
        if kicked == 0 {
            return Err(ModerationError::NotInChannel);
        }
        info!(
            "User {} kicked by {} ({} session(s))",
            target_uuid, actor_uuid, kicked
        );
        Ok(kicked)
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
            let _ = sess
                .sender
                .send(ServiceMessage::ServerSnapshot(snapshot.clone()));
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

    /// Route an audio packet (current format) to voice group members.
    pub async fn route_audio_packet(&self, raw_bytes: Bytes) {
        self.fan_out_audio(raw_bytes, ServiceMessage::RelayAudio)
            .await;
    }

    /// Route a version 1 audio packet. The server cannot read audio either
    /// way; it only keeps the old tag so receivers see why they hear nothing.
    pub async fn route_legacy_audio_packet(&self, raw_bytes: Bytes) {
        self.fan_out_audio(raw_bytes, ServiceMessage::RelayLegacyAudio)
            .await;
    }

    async fn fan_out_audio(&self, raw_bytes: Bytes, wrap: fn(Bytes) -> ServiceMessage) {
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
        sender_session
            .bytes_in
            .fetch_add(raw_bytes.len() as u64, Ordering::Relaxed);

        let voice_group_id = match sender_session.voice_group_id {
            Some(id) => id,
            None => return, // Not in a voice channel
        };

        let members: Vec<u32> = match self.voice_groups.get(&voice_group_id) {
            Some(g) => {
                let group = g.value().read().await;
                group.members.iter().map(|id| *id).collect()
            }
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
                let _ = session.sender.send(wrap(raw_bytes.clone()));
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

        assert!(state
            .voice_groups
            .contains_key(&aura_protocol::make_mls_group_id(&channel_id, true)));
        assert!(state
            .text_groups
            .contains_key(&aura_protocol::make_mls_group_id(&channel_id, false)));
    }

    #[tokio::test]
    async fn test_verification_policy() {
        let db = Arc::new(Database::open_in_memory().unwrap());

        // Create a user
        let key = [0x42u8; 32];
        let user_uuid = db.create_user(&key, None, "TestUser").unwrap();

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
        state
            .add_to_voice_group(channel_id.clone(), session_id)
            .await;
        {
            let group_id = aura_protocol::make_mls_group_id(&channel_id, true);
            let group = state.voice_groups.get(&group_id).unwrap();
            assert!(group.read().await.members.contains(&session_id));
        }

        state
            .remove_from_voice_group(channel_id.clone(), session_id)
            .await;
        {
            let group_id = aura_protocol::make_mls_group_id(&channel_id, true);
            let group = state.voice_groups.get(&group_id).unwrap();
            assert!(!group.read().await.members.contains(&session_id));
        }

        // Test Text Group
        state
            .add_to_text_group(channel_id.clone(), session_id)
            .await;
        {
            let group_id = aura_protocol::make_mls_group_id(&channel_id, false);
            let group = state.text_groups.get(&group_id).unwrap();
            assert!(group.read().await.members.contains(&session_id));
        }

        state
            .remove_from_text_group(channel_id.clone(), session_id)
            .await;
        {
            let group_id = aura_protocol::make_mls_group_id(&channel_id, false);
            let group = state.text_groups.get(&group_id).unwrap();
            assert!(!group.read().await.members.contains(&session_id));
        }
    }

    #[tokio::test]
    async fn test_broadcast_logic() {
        let state = create_test_state();
        let channel_id = "C_200".to_string();

        // Setup two sessions
        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();

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
        state
            .broadcast_user_joined(channel_id.clone(), s2, "User 2".into())
            .await;

        // Check s1 received UserJoined
        if let Some(ServiceMessage::UserJoined {
            channel_id: c,
            session_id: s,
            display_name: n,
            user_uuid: _,
        }) = rx1.recv().await
        {
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
            channel_id: aura_protocol::make_mls_group_id(&channel_id, false),
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
        assert!(
            rx1.try_recv().is_err(),
            "s1 received its own message (echo should be disabled)"
        );
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
                channel_id: aura_protocol::make_mls_group_id(&channel_id, false),
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
            channel_id: aura_protocol::make_mls_group_id(&channel_id, false),
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
        state
            .reset_text_ratchet_counters(aura_protocol::make_mls_group_id(&channel_id, false))
            .await;

        // Next message should not ratchet (use new unique ID)
        let packet51 = EncryptedTextPacket {
            sender_session_id: s1,
            channel_id: aura_protocol::make_mls_group_id(&channel_id, false),
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
        use aura_protocol::{mls_envelope, MlsEnvelope, MlsGroupType};
        let state = create_test_state();
        let channel_id = "C_500".to_string();
        state.create_channel(channel_id.clone()); // Creates epoch 0

        // 1. Submit valid COMMIT for Voice Group
        let commit_msg = MlsEnvelope {
            channel_id: aura_protocol::make_mls_group_id(&channel_id, true),
            group_type: MlsGroupType::Voice as i32,
            epoch: 0, // Current epoch
            sender_id: 12345,
            target_session_id: 0,
            target_uuid: String::new(),
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
            channel_id: aura_protocol::make_mls_group_id(&channel_id, true),
            group_type: MlsGroupType::Voice as i32,
            epoch: 2,
            sender_id: 12345,
            target_session_id: 0,
            target_uuid: String::new(),
            content: Some(mls_envelope::Content::Commit(vec![4, 5, 6])),
        };
        let res_future = state.handle_mls_message(future_msg).await.unwrap();
        assert!(!res_future.success); // Should fail strict check
        assert_eq!(res_future.current_epoch, 1);

        // 4. Unknown Group
        let unknown_msg = MlsEnvelope {
            channel_id: "C_UNKNOWN".to_string(),
            group_type: MlsGroupType::Voice as i32,
            epoch: 0,
            sender_id: 12345,
            target_session_id: 0,
            target_uuid: String::new(),
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
        // use std::thread;
        // use std::time::Duration;

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

    #[test]
    fn test_seen_messages_expire_after_ttl() {
        let seen = SeenMessages::with_ttl(Duration::from_millis(20));
        assert!(seen.check_and_mark("C_1".to_string(), "msg-001"));
        assert!(!seen.check_and_mark("C_1".to_string(), "msg-001"));

        std::thread::sleep(Duration::from_millis(40));

        // Past the TTL the ID is accepted again, even before a sweep...
        assert!(seen.check_and_mark("C_1".to_string(), "msg-001"));
        // ...and is blocked for a fresh TTL from that point.
        assert!(!seen.check_and_mark("C_1".to_string(), "msg-001"));

        std::thread::sleep(Duration::from_millis(40));

        // A sweep drops expired entries and the channels they leave empty.
        seen.cleanup_expired();
        assert_eq!(seen.message_count(), 0);
        assert!(seen.messages.is_empty());
    }

    #[tokio::test]
    async fn test_seen_messages_spawned_cleanup_sweeps() {
        let seen = Arc::new(SeenMessages::with_ttl(Duration::from_millis(10)));
        for i in 0..100 {
            seen.check_and_mark("C_1".to_string(), &format!("msg-{i}"));
        }
        assert_eq!(seen.message_count(), 100);

        let sweeper = Arc::clone(&seen).spawn_cleanup(Duration::from_millis(10));
        tokio::time::sleep(Duration::from_millis(100)).await;
        sweeper.abort();

        assert_eq!(seen.message_count(), 0);
    }

    #[tokio::test]
    async fn test_lazy_mls_group_initialization() {
        let state = create_test_state();
        let channel_id = "C_LAZY_100".to_string();

        // Simulating channels being present but groups missing (fresh restart)
        // Groups are empty by default in create_test_state
        assert!(!state.voice_groups.contains_key(&channel_id));
        assert!(!state.text_groups.contains_key(&channel_id));

        // Setup session
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let session_id = state.register_session("user-lazy".into(), addr, tx);

        // Attempt to join voice group - should lazily create it and make us founder
        state
            .handle_voice_mls_join(
                channel_id.clone(),
                session_id,
                "uuid-lazy".into(),
                vec![0, 1, 2], // Fake key package
            )
            .await;

        // Verify group now exists
        assert!(state
            .voice_groups
            .contains_key(&aura_protocol::make_mls_group_id(&channel_id, true)));

        // Match the message to confirm founder status
        if let Some(ServiceMessage::MlsCreateGroup {
            channel_id: c,
            is_voice: v,
        }) = rx.recv().await
        {
            assert_eq!(c, channel_id);
            assert!(v);
        } else {
            panic!("Should have received MlsCreateGroup for voice!");
        }

        // Attempt to join text group
        state
            .handle_text_mls_join(
                channel_id.clone(),
                session_id,
                "uuid-lazy".into(),
                vec![0, 1, 2],
            )
            .await;

        // Verify group now exists
        assert!(state
            .text_groups
            .contains_key(&aura_protocol::make_mls_group_id(&channel_id, false)));

        // Confirm text founder status
        if let Some(ServiceMessage::MlsCreateGroup {
            channel_id: c,
            is_voice: v,
        }) = rx.recv().await
        {
            assert_eq!(c, channel_id);
            assert!(!v);
        } else {
            panic!("Should have received MlsCreateGroup for text!");
        }
    }

    #[tokio::test]
    async fn voice_ratchet_timer_asks_the_founder() {
        let state = create_test_state();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let addr = "127.0.0.1:9999".parse().unwrap();
        let session_id = state.register_session("uuid-voice-ratchet".into(), addr, tx);

        let channel_id = "voice-ratchet-ch".to_string();
        state.create_channel(channel_id.clone());
        state
            .handle_voice_mls_join(
                channel_id.clone(),
                session_id,
                "uuid-voice-ratchet".into(),
                vec![1, 2, 3],
            )
            .await;

        // The first joiner is told to create the group, and becomes founder.
        assert!(matches!(
            rx.recv().await,
            Some(ServiceMessage::MlsCreateGroup { is_voice: true, .. })
        ));

        // Not due yet.
        state
            .request_due_voice_ratchets_after(Duration::from_secs(900))
            .await;
        assert!(rx.try_recv().is_err());

        // Due: the founder is asked once...
        state.request_due_voice_ratchets_after(Duration::ZERO).await;
        match rx.recv().await {
            Some(ServiceMessage::MlsRatchetRequest {
                channel_id: c,
                is_voice: true,
            }) => assert_eq!(c, channel_id),
            other => panic!("expected a voice MlsRatchetRequest, got {other:?}"),
        }

        // ...and the clock restarts, so the next check doesn't ask again.
        state
            .request_due_voice_ratchets_after(Duration::from_secs(900))
            .await;
        assert!(rx.try_recv().is_err());
    }

    // --- MLS membership follows the channel ---

    type Rx = tokio::sync::mpsc::UnboundedReceiver<ServiceMessage>;

    fn drain(rx: &mut Rx) -> Vec<ServiceMessage> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(msg);
        }
        out
    }

    /// Join a session to `channel` and, if a founder exists, have the founder
    /// add it (Commit + Welcome), the way the clients do.
    async fn join_voice(state: &ServerState, channel: &str, session: u32, uuid: &str) {
        state.add_to_voice_group(channel.to_string(), session).await;
        state
            .handle_voice_mls_join(channel.to_string(), session, uuid.into(), vec![1])
            .await;
        let founder = {
            let group_id = aura_protocol::make_mls_group_id(channel, true);
            let lock = state.voice_groups.get(&group_id).unwrap().clone();
            let founder = lock.read().await.mls.founder();
            founder
        };
        if let Some(founder) = founder {
            if founder != session {
                state
                    .handle_mls_commit_welcome(
                        channel.to_string(),
                        true,
                        founder,
                        session,
                        vec![2],
                        vec![3],
                    )
                    .await;
            }
        }
    }

    fn session(state: &ServerState, uuid: &str) -> (u32, Rx) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let addr = "127.0.0.1:9999".parse().unwrap();
        (state.register_session(uuid.into(), addr, tx), rx)
    }

    fn sync_uuids(msgs: &[ServiceMessage]) -> Option<Vec<String>> {
        msgs.iter().find_map(|m| match m {
            ServiceMessage::MlsSyncMembers { member_uuids, .. } => Some(member_uuids.clone()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn founder_leaving_hands_over_and_asks_for_their_removal() {
        let state = create_test_state();
        state.create_channel("C".into());
        let (alice, mut rx_a) = session(&state, "alice");
        let (bob, mut rx_b) = session(&state, "bob");
        let (carol, mut rx_c) = session(&state, "carol");
        join_voice(&state, "C", alice, "alice").await;
        join_voice(&state, "C", bob, "bob").await;
        join_voice(&state, "C", carol, "carol").await;
        drain(&mut rx_a);
        drain(&mut rx_b);
        drain(&mut rx_c);

        state.remove_from_voice_group("C".into(), alice).await;

        // Bob, the next-longest member, takes over and is asked to remove Alice.
        let to_bob = drain(&mut rx_b);
        assert_eq!(
            sync_uuids(&to_bob),
            Some(vec!["bob".to_string(), "carol".to_string()])
        );
        assert!(drain(&mut rx_c).is_empty());

        // Ratchets now go to Bob...
        state.request_due_voice_ratchets_after(Duration::ZERO).await;
        assert!(matches!(
            drain(&mut rx_b).as_slice(),
            [ServiceMessage::MlsRatchetRequest { is_voice: true, .. }]
        ));
        assert!(drain(&mut rx_a).is_empty());

        // ...and a commit from Alice, no longer the founder, goes nowhere.
        state
            .handle_mls_ratchet_commit("C".into(), true, alice, vec![9])
            .await;
        assert!(drain(&mut rx_c).is_empty());
        state
            .handle_mls_ratchet_commit("C".into(), true, bob, vec![9])
            .await;
        assert!(matches!(
            drain(&mut rx_c).as_slice(),
            [ServiceMessage::MlsCommit { is_voice: true, .. }]
        ));
    }

    #[tokio::test]
    async fn disconnect_asks_the_founder_to_remove_the_member() {
        let state = create_test_state();
        state.create_channel("C".into());
        let (alice, mut rx_a) = session(&state, "alice");
        let (bob, _rx_b) = session(&state, "bob");
        let (carol, mut rx_c) = session(&state, "carol");
        join_voice(&state, "C", alice, "alice").await;
        join_voice(&state, "C", bob, "bob").await;
        join_voice(&state, "C", carol, "carol").await;
        drain(&mut rx_a);
        drain(&mut rx_c);

        // Removed without the connection's own cleanup (e.g. a deleted account).
        state.remove_session(bob).await;

        let to_alice = drain(&mut rx_a);
        assert_eq!(
            sync_uuids(&to_alice),
            Some(vec!["alice".to_string(), "carol".to_string()])
        );
        // UserLeft names the channel, not the MLS group id.
        assert!(drain(&mut rx_c).iter().any(|m| matches!(
            m,
            ServiceMessage::UserLeft { channel_id, session_id } if channel_id == "C" && *session_id == bob
        )));
    }

    #[tokio::test]
    async fn emptied_group_restarts_instead_of_splitting() {
        let state = create_test_state();
        state.create_channel("C".into());
        let (alice, mut rx_a) = session(&state, "alice");
        join_voice(&state, "C", alice, "alice").await;
        drain(&mut rx_a);

        // Bob offers his key package but Alice leaves before adding him.
        let (bob, mut rx_b) = session(&state, "bob");
        state.add_to_voice_group("C".into(), bob).await;
        state
            .handle_voice_mls_join("C".into(), bob, "bob".into(), vec![1])
            .await;
        assert!(drain(&mut rx_b).is_empty());
        // As founder, Alice was handed Bob's key package.
        assert!(matches!(
            drain(&mut rx_a).as_slice(),
            [ServiceMessage::MlsAddMemberRequest { joiner_session_id, .. }] if *joiner_session_id == bob
        ));
        state.remove_from_voice_group("C".into(), alice).await;

        // Bob starts a fresh group rather than waiting forever.
        assert!(matches!(
            drain(&mut rx_b).as_slice(),
            [ServiceMessage::MlsCreateGroup { is_voice: true, .. }]
        ));

        // Alice is still connected (she only switched channels). A newcomer
        // goes to Bob, not to her.
        let (dave, mut rx_d) = session(&state, "dave");
        state.add_to_voice_group("C".into(), dave).await;
        state
            .handle_voice_mls_join("C".into(), dave, "dave".into(), vec![1])
            .await;
        assert!(drain(&mut rx_d).is_empty());
        assert!(matches!(
            drain(&mut rx_b).as_slice(),
            [ServiceMessage::MlsAddMemberRequest { joiner_session_id, .. }] if *joiner_session_id == dave
        ));
        assert!(drain(&mut rx_a).is_empty());
    }

    #[tokio::test]
    async fn welcome_from_a_non_founder_is_dropped() {
        let state = create_test_state();
        state.create_channel("C".into());
        let (alice, _rx_a) = session(&state, "alice");
        let (mallory, _rx_m) = session(&state, "mallory");
        join_voice(&state, "C", alice, "alice").await;
        join_voice(&state, "C", mallory, "mallory").await;

        let (bob, mut rx_b) = session(&state, "bob");
        state.add_to_voice_group("C".into(), bob).await;
        state
            .handle_voice_mls_join("C".into(), bob, "bob".into(), vec![1])
            .await;
        state
            .handle_mls_commit_welcome("C".into(), true, mallory, bob, vec![2], vec![3])
            .await;
        assert!(drain(&mut rx_b).is_empty(), "no Welcome from a non-founder");

        state
            .handle_mls_commit_welcome("C".into(), true, alice, bob, vec![2], vec![3])
            .await;
        assert!(matches!(
            drain(&mut rx_b).as_slice(),
            [ServiceMessage::MlsWelcome { .. }]
        ));
    }
}
