//! End-to-end test harness: a real `aura-server` and headless clients.
//!
//! [`TestServer`] runs the production [`QuicServer`] on `127.0.0.1:0` with a
//! per-test self-signed certificate (loaded through `cert_path`/`key_path`,
//! so ACME is never involved) and an in-memory database.
//!
//! [`TestClient`] is a headless Aura client built from the same shared Rust
//! core the desktop and macOS clients link (`aura-core`: MLS, DAVE text and
//! audio crypto, the audio sender/receiver). It speaks the real wire
//! protocol over real QUIC and drives the MLS join/commit/welcome dance the
//! way `QuicNetworkClient.swift` and `AuraNetworkClient.cs` do. A background
//! task reads control frames, performs the MLS protocol, keeps per-sender
//! audio and text keys current, and reports everything as typed [`Event`]s.
//!
//! Every wait has a timeout so a failure panics instead of hanging CI.

#![allow(dead_code)] // each test binary uses a different subset

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use aura_core::text_crypto::{create_text_message, decrypt_text, encrypt_text};
use aura_core::uniffi_bindings::{AudioReceiverWrapper, AudioSenderWrapper, MlsWrapper};
use aura_identity::{auth_transcript, IdentitySecret, Purpose};
use aura_protocol::{
    mls_envelope::Content, AdminResponse, AuthRequest, AuthResponse, ChannelDeleted,
    EncryptedTextPacket, FastAudioPacket, JoinChannelRequest, ListUsersRequest, ListUsersResponse,
    MlsCommitWelcome, MlsEnvelope, MlsGroupType, MlsSyncMembers, ServerState, TextMessage,
    UserJoined, UserLeft,
};
use aura_server::config::Config;
use aura_server::connection::QuicServer;
use aura_server::db::Database;
use aura_server::state::ServerState as ServerCore;
use prost::Message;
use rustls::pki_types::CertificateDer;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout, Instant};

/// Upper bound for any single wait. Generous for slow CI; a passing test
/// only waits as long as the thing it is waiting for takes.
pub const WAIT: Duration = Duration::from_secs(30);

// Control-stream message types (docs/protocol/control_stream.md).
pub const MSG_PING: u8 = 0x05;
pub const MSG_PONG: u8 = 0x06;
pub const MSG_JOIN_CHANNEL: u8 = 0x10;
pub const MSG_CHANNEL_STATE: u8 = 0x13;
pub const MSG_TEXT_PACKET: u8 = 0x30;
pub const MSG_CHANNEL_DELETED: u8 = 0x47;
pub const MSG_LIST_USERS: u8 = 0x48;
pub const MSG_BAN_USER: u8 = 0x4A;
pub const MSG_KICK_USER: u8 = 0x4C;
pub const MSG_MLS_JOIN: u8 = 0x50;
pub const MSG_MLS_COMMIT_WELCOME: u8 = 0x51;
pub const MSG_MLS_CREATE_GROUP: u8 = 0x52;
pub const MSG_MLS_ADD_MEMBER: u8 = 0x53;
pub const MSG_MLS_COMMIT: u8 = 0x54;
pub const MSG_MLS_WELCOME: u8 = 0x55;
pub const MSG_MLS_RATCHET_REQUEST: u8 = 0x56;
pub const MSG_MLS_RATCHET_COMMIT: u8 = 0x57;
pub const MSG_MLS_SYNC_MEMBERS: u8 = 0x58;

/// Replies that answer a client request on the request's own message type.
const RESPONSE_TYPES: &[u8] = &[
    MSG_PONG, 0x40, 0x41, 0x42, 0x43, 0x44, 0x48, 0x49, 0x4A, 0x4B, 0x4C,
];

/// Poll `f` every 10 ms until it returns `Some`, or panic after [`WAIT`].
pub async fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// A running `aura-server` on an ephemeral loopback port.
pub struct TestServer {
    pub addr: SocketAddr,
    /// The server's state, for assertions and in-process setup.
    pub state: Arc<ServerCore>,
    pub db: Arc<Database>,
    /// The lobby channel every server starts with.
    pub lobby_id: String,
    cert: CertificateDer<'static>,
    dir: PathBuf,
    task: JoinHandle<()>,
}

impl TestServer {
    pub async fn start() -> Self {
        Self::try_start().await.expect("start test server")
    }

    async fn try_start() -> Result<Self> {
        aura_server::tls::install_default_provider();

        // Per-test self-signed certificate, handed to the server as files
        // (the production "bring your own certificate" path).
        let dir = std::env::temp_dir().join(format!("aura-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir)?;
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
        let cert_der = generated.cert.der().to_vec();
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        std::fs::write(&cert_path, pem("CERTIFICATE", &cert_der))?;
        std::fs::write(
            &key_path,
            pem("PRIVATE KEY", &generated.signing_key.serialize_der()),
        )?;

        let mut config = Config::default();
        config.server.bind_address = "127.0.0.1:0".into();
        config.server.cert_path = Some(cert_path);
        config.server.key_path = Some(key_path);
        // The health-check listener shares this port setting; 0 = ephemeral.
        config.server.acme_bind_port = Some(0);
        config.server.acme_domain = None;

        let db = Arc::new(Database::open_in_memory()?);
        let lobby_id = format!("C_{}", uuid::Uuid::new_v4());
        db.upsert_channel(Some(lobby_id.clone()), "Lobby", "", 0, &[], 0, 1)?;

        let state = Arc::new(ServerCore::new(Arc::clone(&db), config));
        let server = QuicServer::new("127.0.0.1:0".parse()?, Arc::clone(&state))?;
        let addr = server.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = server.run().await;
        });

        Ok(Self {
            addr,
            state,
            db,
            lobby_id,
            cert: CertificateDer::from(cert_der),
            dir,
            task,
        })
    }

    /// Create a regular channel through the server's own code path.
    pub async fn create_channel(&self, name: &str) -> String {
        self.state
            .create_channel_persistent(name.to_string(), String::new(), None)
            .await
            .expect("create channel")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// PEM-encode `der` (no PEM/base64 crate is a direct dependency).
fn pem(label: &str, der: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::new();
    for chunk in der.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                b64.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                b64.push('=');
            }
        }
    }
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// What a client observed on its control stream.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Event {
    /// 0x13 full snapshot.
    Snapshot(ServerState),
    UserJoined(UserJoined),
    UserLeft(UserLeft),
    /// 0x47: no longer in a channel (deleted, not found, kicked).
    ChannelDeleted(ChannelDeleted),
    /// 0x30 text relayed to us, with the result of decrypting it.
    Text(ReceivedText),
    /// An MLS signalling message (0x52-0x55) was handled.
    Mls(MlsEvent),
    /// 0x58 member sync (we are the founder asked to remove leavers).
    SyncMembers(MlsSyncMembers),
    /// A reply to a request (see `RESPONSE_TYPES`).
    Response {
        msg_type: u8,
        payload: Vec<u8>,
    },
    /// Any other frame.
    Other {
        msg_type: u8,
        payload: Vec<u8>,
    },
    /// The control stream ended.
    Closed(String),
}

#[derive(Debug, Clone)]
pub struct ReceivedText {
    pub packet: EncryptedTextPacket,
    /// The `EncryptedTextPacket` bytes exactly as the server delivered them.
    pub wire: Vec<u8>,
    pub message: Result<TextMessage, String>,
}

#[derive(Debug, Clone)]
pub struct MlsEvent {
    pub msg_type: u8,
    pub channel_id: String,
    pub is_voice: bool,
    /// The group's epoch after handling it, or why handling failed.
    pub outcome: Result<u64, String>,
}

/// An audio datagram as it reached the receiver.
#[derive(Debug, Clone)]
pub struct AudioRecord {
    pub session_id: u32,
    pub sequence: u16,
    pub epoch_hint: u16,
    /// `Ok` if the receiver authenticated, decrypted and queued the frame.
    pub accepted: Result<(), String>,
}

/// A text message we sent, as it went on the wire.
#[derive(Debug, Clone)]
pub struct SentText {
    pub message_id: String,
    pub epoch: u64,
    pub wire: Vec<u8>,
}

#[derive(Default)]
struct ClientState {
    current_channel: Option<String>,
    /// channel id -> sessions known to be in it (snapshot + join/leave).
    occupants: HashMap<String, BTreeSet<u32>>,
    sender: Option<Arc<AudioSenderWrapper>>,
}

struct Shared {
    session_id: u32,
    user_uuid: String,
    mls: MlsWrapper,
    receiver: AudioReceiverWrapper,
    state: Mutex<ClientState>,
    audio_log: Mutex<Vec<AudioRecord>>,
    send: tokio::sync::Mutex<quinn::SendStream>,
    events: mpsc::UnboundedSender<Event>,
}

/// A login the server refused.
#[derive(Debug)]
pub struct LoginRefused(pub String);

impl std::fmt::Display for LoginRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "login refused: {}", self.0)
    }
}

impl std::error::Error for LoginRefused {}

pub struct TestClient {
    pub name: String,
    pub session_id: u32,
    pub user_uuid: String,
    pub is_admin: bool,
    pub identity: IdentitySecret,
    shared: Arc<Shared>,
    conn: quinn::Connection,
    _endpoint: quinn::Endpoint,
    rx: mpsc::UnboundedReceiver<Event>,
    buffered: VecDeque<Event>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for TestClient {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        self.conn.close(0u32.into(), b"test done");
    }
}

impl TestClient {
    /// Connect and log in under a fresh identity.
    pub async fn connect(server: &TestServer, name: &str) -> Self {
        Self::try_connect(server, name, IdentitySecret::generate())
            .await
            .unwrap_or_else(|e| panic!("{name}: connect failed: {e:#}"))
    }

    /// Connect as a bootstrap admin: the account is created in the database
    /// first, keyed by the new identity, as `AURA_BOOTSTRAP_ADMIN_KEY` does.
    pub async fn connect_admin(server: &TestServer, name: &str) -> Self {
        let identity = IdentitySecret::generate();
        server
            .db
            .create_bootstrap_admin(&identity.public_key().ed25519_bytes(), name)
            .expect("create bootstrap admin");
        Self::try_connect(server, name, identity)
            .await
            .unwrap_or_else(|e| panic!("{name}: admin connect failed: {e:#}"))
    }

    /// Connect with a given identity. A refused login is a [`LoginRefused`]
    /// error carrying the server's message.
    pub async fn try_connect(
        server: &TestServer,
        name: &str,
        identity: IdentitySecret,
    ) -> Result<Self> {
        let endpoint = client_endpoint(&server.cert)?;
        let conn = timeout(WAIT, endpoint.connect(server.addr, "localhost")?)
            .await
            .context("QUIC handshake timed out")??;

        // The server opens the control stream and speaks first.
        let (mut send, mut recv) = timeout(WAIT, conn.accept_bi())
            .await
            .context("control stream never opened")??;

        let hello = timeout(WAIT, async {
            let ty = recv.read_u8().await?;
            let mut challenge = [0u8; 32];
            recv.read_exact(&mut challenge).await?;
            Ok::<_, anyhow::Error>((ty, challenge))
        })
        .await
        .context("ServerHello timed out")??;
        if hello.0 != 0x02 {
            bail!("expected ServerHello (0x02), got 0x{:02x}", hello.0);
        }
        let challenge = hello.1;

        let public = identity.public_key();
        let signature = identity.sign(Purpose::Auth, &auth_transcript(&challenge, name, &public));
        let request = AuthRequest {
            identity_public_key: public.as_bytes().to_vec(),
            display_name: name.to_string(),
            signature: signature.as_bytes().to_vec(),
            server_password: None,
            challenge: challenge.to_vec(),
        };
        write_frame(&mut send, 0x03, &request.encode_to_vec()).await?;

        let response = timeout(WAIT, async {
            let ty = recv.read_u8().await?;
            let len = recv.read_u32_le().await? as usize;
            let mut payload = vec![0u8; len];
            recv.read_exact(&mut payload).await?;
            Ok::<_, anyhow::Error>((ty, payload))
        })
        .await
        .context("AuthResponse timed out")??;
        if response.0 != 0x04 {
            bail!("expected AuthResponse (0x04), got 0x{:02x}", response.0);
        }
        let auth = AuthResponse::decode(&response.1[..])?;
        if !auth.success {
            return Err(LoginRefused(auth.error_message).into());
        }

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            session_id: auth.session_id,
            user_uuid: auth.user_uuid.clone(),
            mls: MlsWrapper::new(auth.user_uuid.clone(), identity.to_bytes().to_vec())
                .map_err(|e| anyhow!("MLS client: {e}"))?,
            receiver: AudioReceiverWrapper::new(),
            state: Mutex::new(ClientState::default()),
            audio_log: Mutex::new(Vec::new()),
            send: tokio::sync::Mutex::new(send),
            events: events_tx,
        });

        let tasks = vec![
            tokio::spawn(control_loop(Arc::clone(&shared), recv)),
            tokio::spawn(datagram_loop(Arc::clone(&shared), conn.clone())),
        ];

        Ok(Self {
            name: name.to_string(),
            session_id: auth.session_id,
            user_uuid: auth.user_uuid,
            is_admin: auth.is_admin,
            identity,
            shared,
            conn,
            _endpoint: endpoint,
            rx: events_rx,
            buffered: VecDeque::new(),
            tasks,
        })
    }

    // ----- events ---------------------------------------------------------

    /// Wait for the next event matching `pred`. Events that don't match stay
    /// buffered for later waits.
    pub async fn wait_event(&mut self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        if let Some(i) = self.buffered.iter().position(&pred) {
            return self.buffered.remove(i).unwrap();
        }
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match timeout(left, self.rx.recv()).await {
                Ok(Some(ev)) if pred(&ev) => return ev,
                Ok(Some(ev)) => self.buffered.push_back(ev),
                Ok(None) => panic!("{}: event channel closed waiting for {what}", self.name),
                Err(_) => panic!(
                    "{}: timed out waiting for {what}; unmatched events: {:?}",
                    self.name,
                    self.buffered
                        .iter()
                        .map(|e| format!("{e:?}").chars().take(120).collect::<String>())
                        .collect::<Vec<_>>()
                ),
            }
        }
    }

    /// Pull everything pending off the channel into the buffer.
    fn pump(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            self.buffered.push_back(ev);
        }
    }

    /// Whether an event matching `pred` has been seen and not yet consumed.
    pub fn has_event(&mut self, pred: impl Fn(&Event) -> bool) -> bool {
        self.pump();
        self.buffered.iter().any(pred)
    }

    /// Wait until the server has handled everything this client sent before
    /// (a control-stream ping/pong round trip).
    pub async fn sync(&mut self) {
        let nonce: [u8; 8] = rand_nonce();
        self.send(MSG_PING, &nonce).await.expect("send ping");
        self.wait_event("pong", |e| {
            matches!(
                e,
                Event::Response {
                    msg_type: MSG_PONG,
                    ..
                }
            )
        })
        .await;
    }

    // ----- sending --------------------------------------------------------

    async fn send(&self, msg_type: u8, payload: &[u8]) -> Result<()> {
        let mut send = self.shared.send.lock().await;
        write_frame(&mut send, msg_type, payload).await
    }

    /// Send a request and wait for the reply on the same message type.
    async fn request(&mut self, msg_type: u8, payload: &[u8]) -> Vec<u8> {
        self.send(msg_type, payload).await.expect("send request");
        match self
            .wait_event(
                &format!("reply to 0x{msg_type:02x}"),
                |e| matches!(e, Event::Response { msg_type: t, .. } if *t == msg_type),
            )
            .await
        {
            Event::Response { payload, .. } => payload,
            _ => unreachable!(),
        }
    }

    // ----- channels -------------------------------------------------------

    /// Join (move to) a channel and offer key packages for its voice and text
    /// MLS groups, like the real clients. State kept for the channel we
    /// leave is dropped (`forget_group`), as they do.
    pub async fn join_channel(&mut self, channel_id: &str) {
        self.move_to_channel(channel_id, true).await;
    }

    /// Like [`join_channel`](Self::join_channel), but keep our local MLS
    /// state for the channel we leave, so a test can show the old keys are
    /// useless without relying on the client discarding them.
    pub async fn join_channel_keeping_old_groups(&mut self, channel_id: &str) {
        self.move_to_channel(channel_id, false).await;
    }

    async fn move_to_channel(&mut self, channel_id: &str, forget_previous: bool) {
        let previous = {
            let mut st = self.shared.state.lock().unwrap();
            let previous = st.current_channel.replace(channel_id.to_string());
            st.sender = None;
            previous
        };
        if let Some(prev) = &previous {
            let occupants = self
                .shared
                .state
                .lock()
                .unwrap()
                .occupants
                .get(prev)
                .cloned()
                .unwrap_or_default();
            for s in occupants {
                self.shared.receiver.remove_sender(s);
            }
        }

        let req = JoinChannelRequest {
            channel_id: channel_id.to_string(),
        };
        self.send(MSG_JOIN_CHANNEL, &req.encode_to_vec())
            .await
            .expect("send join");

        if forget_previous {
            if let Some(prev) = previous.filter(|p| p != channel_id) {
                let _ = self.shared.mls.forget_group(prev.clone(), true);
                let _ = self.shared.mls.forget_group(prev, false);
            }
        }

        for is_voice in [true, false] {
            let key_package = self.shared.mls.create_key_package().expect("key package");
            let envelope = MlsEnvelope {
                sender_id: self.session_id,
                channel_id: channel_id.to_string(),
                group_type: group_type(is_voice) as i32,
                content: Some(Content::KeyPackage(key_package)),
                ..Default::default()
            };
            self.send(MSG_MLS_JOIN, &envelope.encode_to_vec())
                .await
                .expect("send mls join");
        }
    }

    /// The sessions this client believes are in `channel_id`.
    pub fn occupants(&self, channel_id: &str) -> BTreeSet<u32> {
        self.shared
            .state
            .lock()
            .unwrap()
            .occupants
            .get(channel_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Wait until `session_id` shows up in `channel_id` for this client.
    pub async fn wait_sees(&self, channel_id: &str, session_id: u32) {
        eventually(
            &format!("{} to see session {session_id} in {channel_id}", self.name),
            || {
                self.occupants(channel_id)
                    .contains(&session_id)
                    .then_some(())
            },
        )
        .await;
    }

    // ----- MLS state ------------------------------------------------------

    pub fn epoch(&self, channel_id: &str, is_voice: bool) -> Option<u64> {
        self.shared
            .mls
            .current_epoch(channel_id.to_string(), is_voice)
            .ok()
    }

    pub fn is_member(&self, channel_id: &str, is_voice: bool) -> bool {
        self.shared.mls.is_member(channel_id.to_string(), is_voice)
    }

    /// Wait until both of the channel's groups exist locally and the voice
    /// sending key is installed.
    pub async fn wait_ready(&self, channel_id: &str) {
        eventually(&format!("{} to hold both MLS groups", self.name), || {
            (self.is_member(channel_id, true)
                && self.is_member(channel_id, false)
                && self.shared.state.lock().unwrap().sender.is_some())
            .then_some(())
        })
        .await;
    }

    /// Wait until the group is at exactly `epoch`.
    pub async fn wait_epoch(&self, channel_id: &str, is_voice: bool, epoch: u64) {
        eventually(
            &format!(
                "{} to reach {} epoch {epoch} (now {:?})",
                self.name,
                if is_voice { "voice" } else { "text" },
                self.epoch(channel_id, is_voice)
            ),
            || (self.epoch(channel_id, is_voice) == Some(epoch)).then_some(()),
        )
        .await;
    }

    // ----- text -----------------------------------------------------------

    /// Encrypt `content` with this client's key for the text group's current
    /// epoch and send it, as the real clients do.
    pub async fn send_text(&self, channel_id: &str, content: &str) -> SentText {
        // The Welcome may land after JoinChannel returns; wait like the
        // clients do (they retry for ~3 s).
        eventually(&format!("{} text group", self.name), || {
            self.is_member(channel_id, false).then_some(())
        })
        .await;
        let key = self
            .shared
            .mls
            .export_text_key(channel_id.to_string(), self.session_id, None)
            .expect("own text key");
        let epoch = self
            .shared
            .mls
            .current_epoch(channel_id.to_string(), false)
            .expect("text epoch");
        let msg = create_text_message(&self.user_uuid, content, None);
        let packet = encrypt_text(
            &key.try_into().expect("32-byte key"),
            epoch,
            channel_id.to_string(),
            self.session_id,
            &msg,
        )
        .expect("encrypt text");
        let wire = packet.encode_to_vec();
        self.send(MSG_TEXT_PACKET, &wire).await.expect("send text");
        SentText {
            message_id: packet.message_id,
            epoch,
            wire,
        }
    }

    /// Wait for the next text message relayed to us.
    pub async fn next_text(&mut self) -> ReceivedText {
        match self
            .wait_event("a text message", |e| matches!(e, Event::Text(_)))
            .await
        {
            Event::Text(t) => t,
            _ => unreachable!(),
        }
    }

    /// Try to decrypt a relayed packet with this client's current MLS state,
    /// exactly as the receive path does.
    pub fn try_decrypt(&self, packet: &EncryptedTextPacket) -> Result<TextMessage, String> {
        decrypt_packet(&self.shared, packet)
    }

    // ----- voice ----------------------------------------------------------

    /// Encode, encrypt and send one 20 ms frame as a QUIC datagram. Returns
    /// the packet exactly as sent (after the `0x01` datagram tag). Refuses
    /// to send without an MLS-derived key, as the clients now do.
    pub fn send_audio_frame(&self, pcm: &[i16]) -> Result<Vec<u8>> {
        let sender = self
            .shared
            .state
            .lock()
            .unwrap()
            .sender
            .clone()
            .ok_or_else(|| anyhow!("no MLS voice key yet; refusing to send audio"))?;
        let packet = sender
            .process(pcm)
            .map_err(|e| anyhow!("audio encode/encrypt: {e}"))?
            .ok_or_else(|| anyhow!("VAD swallowed the frame"))?;
        self.send_audio_datagram(&packet)?;
        Ok(packet)
    }

    /// Send `packet` as an audio datagram verbatim (`0x01` + packet). For
    /// tests that send frames the real send path never would.
    pub fn send_audio_datagram(&self, packet: &[u8]) -> Result<()> {
        let mut dgram = Vec::with_capacity(1 + packet.len());
        dgram.push(0x01);
        dgram.extend_from_slice(packet);
        self.conn.send_datagram(dgram.into())?;
        Ok(())
    }

    /// Every audio datagram that has reached this client, in arrival order.
    pub fn audio_log(&self) -> Vec<AudioRecord> {
        self.shared.audio_log.lock().unwrap().clone()
    }

    /// Wait until `n` audio datagrams have arrived.
    pub async fn wait_audio_packets(&self, n: usize) -> Vec<AudioRecord> {
        eventually(
            &format!("{} to receive {n} audio packets", self.name),
            || {
                let log = self.audio_log();
                (log.len() >= n).then_some(log)
            },
        )
        .await
    }

    /// Decoded frames ready for playback, as `(sender session, pcm)`.
    pub fn pop_decoded(&self) -> Vec<(u32, Vec<i16>)> {
        self.shared
            .receiver
            .pop_decoded()
            .into_iter()
            .map(|f| (f.session_id, f.pcm))
            .collect()
    }

    // ----- moderation -----------------------------------------------------

    pub async fn list_users(&mut self, offset: u32, limit: u32) -> ListUsersResponse {
        let req = ListUsersRequest {
            session_token: String::new(),
            offset,
            limit,
        };
        let payload = self.request(MSG_LIST_USERS, &req.encode_to_vec()).await;
        ListUsersResponse::decode(&payload[..]).expect("ListUsersResponse")
    }

    pub async fn kick_user(&mut self, user_uuid: &str, reason: &str) -> AdminResponse {
        let req = aura_protocol::KickUserRequest {
            target_user_uuid: user_uuid.to_string(),
            reason: reason.to_string(),
        };
        let payload = self.request(MSG_KICK_USER, &req.encode_to_vec()).await;
        AdminResponse::decode(&payload[..]).expect("AdminResponse")
    }

    pub async fn ban_user(&mut self, user_uuid: &str, banned: bool, reason: &str) -> AdminResponse {
        let req = aura_protocol::BanUserRequest {
            session_token: String::new(),
            target_user_uuid: user_uuid.to_string(),
            banned,
            reason: reason.to_string(),
        };
        let payload = self.request(MSG_BAN_USER, &req.encode_to_vec()).await;
        AdminResponse::decode(&payload[..]).expect("AdminResponse")
    }

    // ----- connection -----------------------------------------------------

    /// Wait for the server to close the connection; returns how.
    pub async fn wait_closed(&self) -> quinn::ConnectionError {
        timeout(WAIT, self.conn.closed())
            .await
            .unwrap_or_else(|_| panic!("{}: connection was not closed by the server", self.name))
    }

    /// Close the connection cleanly.
    pub fn disconnect(&self) {
        self.conn.close(0u32.into(), b"bye");
    }
}

fn group_type(is_voice: bool) -> MlsGroupType {
    if is_voice {
        MlsGroupType::Voice
    } else {
        MlsGroupType::Text
    }
}

fn rand_nonce() -> [u8; 8] {
    let id = uuid::Uuid::new_v4();
    id.as_bytes()[..8].try_into().unwrap()
}

fn client_endpoint(server_cert: &CertificateDer<'static>) -> Result<quinn::Endpoint> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(server_cert.clone())?;
    let mut crypto =
        rustls::ClientConfig::builder_with_provider(Arc::new(aura_server::tls::crypto_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_root_certificates(roots)
            .with_no_client_auth();
    crypto.alpn_protocols = vec![aura_server::tls::ALPN.to_vec()];

    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    config.transport_config(Arc::new(transport));

    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

async fn write_frame(send: &mut quinn::SendStream, msg_type: u8, payload: &[u8]) -> Result<()> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(msg_type);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    send.write_all(&frame).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Background tasks: the part of a real client that lives in its network layer
// ---------------------------------------------------------------------------

async fn control_loop(shared: Arc<Shared>, mut recv: quinn::RecvStream) {
    let reason = loop {
        let frame = async {
            let msg_type = recv.read_u8().await?;
            if msg_type == 0x00 {
                return Ok::<_, anyhow::Error>((0u8, Vec::new()));
            }
            let len = recv.read_u32_le().await? as usize;
            let mut payload = vec![0u8; len];
            recv.read_exact(&mut payload).await?;
            Ok((msg_type, payload))
        }
        .await;
        match frame {
            Ok((0, _)) => {}
            Ok((msg_type, payload)) => shared.handle(msg_type, payload).await,
            Err(e) => break format!("{e}"),
        }
    };
    let _ = shared.events.send(Event::Closed(reason));
}

/// Audio arrives as datagrams tagged `0x01`; `0x00` is the keepalive.
async fn datagram_loop(shared: Arc<Shared>, conn: quinn::Connection) {
    while let Ok(data) = conn.read_datagram().await {
        if data.len() <= 1 || data[0] != 0x01 {
            continue;
        }
        let packet = &data[1..];
        let Ok(parsed) = FastAudioPacket::parse(bytes::Bytes::copy_from_slice(packet)) else {
            continue;
        };
        let accepted = shared
            .receiver
            .on_packet(packet)
            .map_err(|e| format!("{e:?}"));
        shared.audio_log.lock().unwrap().push(AudioRecord {
            session_id: parsed.session_id,
            sequence: parsed.sequence,
            epoch_hint: parsed.epoch_hint,
            accepted,
        });
    }
}

fn decrypt_packet(shared: &Shared, packet: &EncryptedTextPacket) -> Result<TextMessage, String> {
    // Derive the key for the epoch on the packet, never the current one.
    let key = shared
        .mls
        .export_text_key(
            packet.channel_id.clone(),
            packet.sender_session_id,
            Some(packet.epoch),
        )
        .map_err(|e| format!("key derivation failed: {e}"))?;
    let key: [u8; 32] = key.try_into().map_err(|_| "bad key length".to_string())?;
    decrypt_text(&key, packet).map_err(|e| format!("decrypt failed: {e}"))
}

impl Shared {
    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    async fn send_frame(&self, msg_type: u8, payload: &[u8]) -> Result<()> {
        let mut send = self.send.lock().await;
        write_frame(&mut send, msg_type, payload).await
    }

    async fn handle(&self, msg_type: u8, payload: Vec<u8>) {
        match msg_type {
            MSG_CHANNEL_STATE => {
                if let Ok(snapshot) = ServerState::decode(&payload[..]) {
                    {
                        let mut st = self.state.lock().unwrap();
                        st.occupants.clear();
                        for ch in &snapshot.channels {
                            let set = st.occupants.entry(ch.channel_id.clone()).or_default();
                            set.extend(ch.users.iter().map(|u| u.session_id));
                        }
                    }
                    self.refresh_current();
                    self.emit(Event::Snapshot(snapshot));
                }
            }
            0x11 => {
                if let Ok(j) = UserJoined::decode(&payload[..]) {
                    {
                        // A join is a move: the session leaves any other channel.
                        let mut st = self.state.lock().unwrap();
                        for set in st.occupants.values_mut() {
                            set.remove(&j.session_id);
                        }
                        st.occupants
                            .entry(j.channel_id.clone())
                            .or_default()
                            .insert(j.session_id);
                    }
                    self.refresh_keys(&j.channel_id);
                    self.emit(Event::UserJoined(j));
                }
            }
            0x12 => {
                if let Ok(l) = UserLeft::decode(&payload[..]) {
                    if let Some(set) = self.state.lock().unwrap().occupants.get_mut(&l.channel_id) {
                        set.remove(&l.session_id);
                    }
                    self.receiver.remove_sender(l.session_id);
                    self.emit(Event::UserLeft(l));
                }
            }
            MSG_CHANNEL_DELETED => {
                if let Ok(d) = ChannelDeleted::decode(&payload[..]) {
                    {
                        let mut st = self.state.lock().unwrap();
                        if st.current_channel.as_deref() == Some(d.channel_id.as_str()) {
                            st.current_channel = None;
                            st.sender = None;
                        }
                    }
                    self.emit(Event::ChannelDeleted(d));
                }
            }
            MSG_TEXT_PACKET => {
                if let Ok(packet) = EncryptedTextPacket::decode(&payload[..]) {
                    let message = decrypt_packet(self, &packet);
                    self.emit(Event::Text(ReceivedText {
                        packet,
                        wire: payload,
                        message,
                    }));
                }
            }
            MSG_MLS_CREATE_GROUP
            | MSG_MLS_ADD_MEMBER
            | MSG_MLS_COMMIT
            | MSG_MLS_WELCOME
            | MSG_MLS_RATCHET_REQUEST => {
                let Ok(envelope) = MlsEnvelope::decode(&payload[..]) else {
                    return;
                };
                let channel_id = envelope.channel_id.clone();
                let is_voice = envelope.group_type() == MlsGroupType::Voice;
                let outcome = self.handle_mls(msg_type, envelope).await;
                if outcome.is_ok() {
                    self.refresh_keys(&channel_id);
                }
                self.emit(Event::Mls(MlsEvent {
                    msg_type,
                    channel_id,
                    is_voice,
                    outcome,
                }));
            }
            MSG_MLS_SYNC_MEMBERS => {
                let Ok(sync) = MlsSyncMembers::decode(&payload[..]) else {
                    return;
                };
                let is_voice = sync.group_type() == MlsGroupType::Voice;
                let result = self
                    .handle_sync_members(&sync.channel_id, is_voice, sync.member_uuids.clone())
                    .await;
                if result.is_ok() {
                    self.refresh_keys(&sync.channel_id);
                }
                self.emit(Event::SyncMembers(sync));
            }
            t if RESPONSE_TYPES.contains(&t) => self.emit(Event::Response {
                msg_type: t,
                payload,
            }),
            t => self.emit(Event::Other {
                msg_type: t,
                payload,
            }),
        }
    }

    /// The MLS half of the protocol; returns the group's epoch afterwards.
    async fn handle_mls(&self, msg_type: u8, envelope: MlsEnvelope) -> Result<u64, String> {
        let channel_id = envelope.channel_id.clone();
        let is_voice = envelope.group_type() == MlsGroupType::Voice;
        let mls = &self.mls;
        match msg_type {
            // We are the founder of a fresh group.
            MSG_MLS_CREATE_GROUP => {
                mls.create_group(channel_id.clone(), is_voice)
                    .map_err(|e| e.to_string())?;
            }
            // Add the joiner, tell the server (commit for the group,
            // welcome for the joiner).
            MSG_MLS_ADD_MEMBER => {
                let Some(Content::KeyPackage(kp)) = envelope.content else {
                    return Err("add-member without a key package".into());
                };
                let cw = mls
                    .add_member(channel_id.clone(), is_voice, kp)
                    .map_err(|e| e.to_string())?;
                let out = MlsEnvelope {
                    sender_id: self.session_id,
                    channel_id: channel_id.clone(),
                    group_type: group_type(is_voice) as i32,
                    content: Some(Content::CommitWelcome(MlsCommitWelcome {
                        commit: cw.commit,
                        welcome: cw.welcome,
                        new_member_session_id: envelope.target_session_id,
                    })),
                    ..Default::default()
                };
                self.send_frame(MSG_MLS_COMMIT_WELCOME, &out.encode_to_vec())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            MSG_MLS_COMMIT => {
                let Some(Content::Commit(commit)) = envelope.content else {
                    return Err("commit message without a commit".into());
                };
                mls.process_commit(channel_id.clone(), is_voice, commit)
                    .map_err(|e| e.to_string())?;
            }
            MSG_MLS_WELCOME => {
                let Some(Content::Welcome(welcome)) = envelope.content else {
                    return Err("welcome message without a welcome".into());
                };
                mls.join_group(welcome).map_err(|e| e.to_string())?;
            }
            // Founder: advance the epoch with an empty commit.
            MSG_MLS_RATCHET_REQUEST => {
                if !mls.is_member(channel_id.clone(), is_voice) {
                    return Err("ratchet requested for a group we are not in".into());
                }
                let commit = mls
                    .self_update(channel_id.clone(), is_voice)
                    .map_err(|e| e.to_string())?;
                self.send_ratchet_commit(&channel_id, is_voice, commit)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            _ => unreachable!(),
        }
        mls.current_epoch(channel_id, is_voice)
            .map_err(|e| e.to_string())
    }

    /// 0x58: remove every member whose account is not listed, and return the
    /// commit on the ratchet-commit path (nothing is sent if nobody left).
    async fn handle_sync_members(
        &self,
        channel_id: &str,
        is_voice: bool,
        keep: Vec<String>,
    ) -> Result<(), String> {
        if !self.mls.is_member(channel_id.to_string(), is_voice) {
            return Err("member sync for a group we are not in".into());
        }
        let commit = self
            .mls
            .remove_members_except(channel_id.to_string(), is_voice, keep)
            .map_err(|e| e.to_string())?;
        if let Some(commit) = commit {
            self.send_ratchet_commit(channel_id, is_voice, commit)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    async fn send_ratchet_commit(
        &self,
        channel_id: &str,
        is_voice: bool,
        commit: Vec<u8>,
    ) -> Result<()> {
        let out = MlsEnvelope {
            sender_id: self.session_id,
            channel_id: channel_id.to_string(),
            group_type: group_type(is_voice) as i32,
            content: Some(Content::Commit(commit)),
            ..Default::default()
        };
        self.send_frame(MSG_MLS_RATCHET_COMMIT, &out.encode_to_vec())
            .await
    }

    fn refresh_current(&self) {
        let current = self.state.lock().unwrap().current_channel.clone();
        if let Some(channel) = current {
            self.refresh_keys(&channel);
        }
    }

    /// Bring text and voice keys up to date for `channel_id`: warm retained
    /// text keys for everyone visible, and (for the channel we are in) set the
    /// audio sending key and a receive key per other occupant.
    fn refresh_keys(&self, channel_id: &str) {
        let (is_current, occupants) = {
            let st = self.state.lock().unwrap();
            (
                st.current_channel.as_deref() == Some(channel_id),
                st.occupants.get(channel_id).cloned().unwrap_or_default(),
            )
        };
        let channel = channel_id.to_string();

        if self.mls.is_member(channel.clone(), false) {
            for s in occupants.iter().copied().chain([self.session_id]) {
                let _ = self.mls.register_text_sender(channel.clone(), s);
            }
        }

        if !is_current || !self.mls.is_member(channel.clone(), true) {
            return;
        }
        let Ok(epoch) = self.mls.current_epoch(channel.clone(), true) else {
            return;
        };
        let Ok(my_key) = self.mls.export_audio_key(channel.clone(), self.session_id) else {
            return;
        };
        {
            let mut st = self.state.lock().unwrap();
            match &st.sender {
                Some(sender) => {
                    let _ = sender.update_key(my_key, epoch);
                }
                None => {
                    // Only ever built from an MLS-derived key.
                    if let Ok(sender) = AudioSenderWrapper::new(self.session_id, &my_key) {
                        let _ = sender.update_key(my_key.clone(), epoch);
                        st.sender = Some(Arc::new(sender));
                    }
                }
            }
        }
        for s in occupants.into_iter().filter(|s| *s != self.session_id) {
            let Ok(key) = self.mls.export_audio_key(channel.clone(), s) else {
                continue;
            };
            let hint = (epoch & 0xFFFF) as u16;
            if !self
                .receiver
                .update_sender_key(s, &key, hint)
                .unwrap_or(false)
            {
                let _ = self.receiver.add_sender(s, &key, hint);
            }
        }
    }
}
