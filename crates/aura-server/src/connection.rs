//! QUIC connection handler module.
//!
//! Handles incoming QUIC connections, authentication, and stream routing.

use crate::auth::{AuthError, AuthService};
use crate::db::AdminPermissions;
use crate::rate_limit::{HandshakeRateLimiter, PingRateLimiter};
use crate::state::{ModerationError, ServerState, ServiceMessage};
use crate::tls;
use anyhow::{anyhow, Result};
use aura_protocol::{AUDIO_DATAGRAM, AUDIO_DATAGRAM_V1, MSG_AUDIO, MSG_AUDIO_V1};
use bytes::{BufMut, BytesMut};
use prost::Message;
use quinn::{Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_acme::{caches::DirCache, AcmeConfig, UseChallenge};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tracing::field::Empty;
use tracing::{debug, error, info, trace, warn, Instrument as _};

// Protocol message types
//
// Every control-stream message except the bare 0x00 keepalive and the
// ServerHello is framed as [type u8][u32 LE length][payload].
// const MSG_CHALLENGE_REQUEST: u8 = 0x01;
const MSG_CHALLENGE_RESPONSE: u8 = 0x02;
const MSG_AUTH_REQUEST: u8 = 0x03;
const MSG_AUTH_RESPONSE: u8 = 0x04;
/// Client → server RTT probe on the control stream, for clients that can't
/// send QUIC datagrams: the payload is an 8-byte nonce.
const MSG_PING: u8 = 0x05;
/// Server → client reply to `MSG_PING`, carrying the same nonce.
const MSG_PONG: u8 = 0x06;
/// Length of a `MSG_PING` / `MSG_PONG` payload.
const PING_NONCE_LEN: usize = 8;
const MSG_JOIN_CHANNEL: u8 = 0x10;
// Audio over the reliable stream is `aura_protocol::MSG_AUDIO` (0x21, header
// authenticated) or `MSG_AUDIO_V1` (0x20, relayed only so receivers can report
// the old format).
const MSG_TEXT_PACKET: u8 = 0x30;
const MSG_CREATE_CHANNEL: u8 = 0x40;
const MSG_UPDATE_CHANNEL: u8 = 0x41;
const MSG_UPDATE_PROFILE: u8 = 0x42;
const MSG_PROFILE_UPDATED: u8 = 0x46;
/// Server → occupants of a channel that was just deleted.
const MSG_CHANNEL_DELETED: u8 = 0x47;
const MSG_DELETE_CHANNEL: u8 = 0x43;
const MSG_DELETE_USER: u8 = 0x44;
const MSG_UPDATE_STATUS: u8 = 0x45;

// Moderation. Each request is answered on its own message type.
/// ListUsersRequest → ListUsersResponse (any admin).
const MSG_LIST_USERS: u8 = 0x48;
/// VerifyUserRequest → AdminResponse (verify_users).
const MSG_VERIFY_USER: u8 = 0x49;
/// BanUserRequest → AdminResponse (ban_users). Bans and unbans.
const MSG_BAN_USER: u8 = 0x4A;
/// GrantAdminRequest → AdminResponse (grant_admin).
const MSG_GRANT_ADMIN: u8 = 0x4B;
/// KickUserRequest → AdminResponse (ban_users).
const MSG_KICK_USER: u8 = 0x4C;
/// ServerMuteRequest → AdminResponse (ban_users).
const MSG_SERVER_MUTE: u8 = 0x4D;

/// QUIC application error code the server closes a connection with when the
/// account has just been banned.
const CLOSE_BANNED: u32 = 0x01;

// MLS Protocol messages
const MSG_MLS_JOIN: u8 = 0x50; // Client sends key package on channel join
const MSG_MLS_COMMIT_WELCOME: u8 = 0x51; // Client sends commit + welcome after adding member
const MSG_MLS_RATCHET_REQUEST: u8 = 0x56; // Server asks founder to advance the epoch
const MSG_MLS_RATCHET_COMMIT: u8 = 0x57; // Founder returns a commit with no Welcome (ratchet or removals)
const MSG_MLS_SYNC_MEMBERS: u8 = 0x58; // Server tells the founder who should remain in a group

// Security limits
const MAX_AUDIO_PACKET_SIZE: usize = 65536; // 64KB for audio (far more than enough for Opus)
const MAX_CONTROL_PACKET_SIZE: usize = 256 * 1024; // 256KB for signaling/metadata
/// Cap for MLS Commit/Welcome uploads (`MSG_MLS_COMMIT_WELCOME`,
/// `MSG_MLS_RATCHET_COMMIT`), which grow with group size. Under the X-Wing
/// ciphersuite each member adds ~1.45 KB to a Welcome (it carries the ratchet
/// tree) and ~1.2 KB to a Commit; a founder's combined Commit+Welcome measured
/// ~280 KB at 100 members and ~690 KB at 256. 1 MiB holds groups of roughly
/// 390. Only these message types get the larger cap.
const MAX_MLS_PACKET_SIZE: usize = 1024 * 1024;

/// Upper bound on a login request. A real one is about 5.5 KB: a 1,984-byte
/// identity key, a 3,373-byte signature, the challenge, name and password.
const MAX_AUTH_REQUEST_SIZE: usize = 16 * 1024;
/// QUIC's base UDP payload size: every path must carry it, and it is what the
/// server holds both directions to when `server.mtu_discovery` is off.
const BASE_UDP_PAYLOAD_SIZE: u16 = 1200;

// Hard ceiling on how long a single control frame body may take to arrive.
// Prevents drip-feed (Slowloris-style) attacks that stay under the byte cap.
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a refused login waits for the client to acknowledge the
/// AuthResponse that says why, before the connection is dropped.
const REFUSAL_DELIVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// QUIC server for handling client connections.
pub struct QuicServer {
    endpoint: Endpoint,
    state: Arc<ServerState>,
    rate_limiter: HandshakeRateLimiter,
}

impl QuicServer {
    /// Create a new QUIC server based on the provided configuration.
    pub fn new(bind_addr: SocketAddr, state: Arc<ServerState>) -> Result<Self> {
        // The TCP listener (ACME HTTP-01 + health check) shares the QUIC IP
        // unless configured otherwise; Fly.io binds QUIC to
        // fly-global-services, which TCP must not use.
        let tcp_ip = state.config.server.http_bind_ip.unwrap_or(bind_addr.ip());

        // TLS source priority: explicit cert/key files > ACME (Let's Encrypt) > self-signed.
        let server_config = if let (Some(cert_path), Some(key_path)) = (
            &state.config.server.cert_path,
            &state.config.server.key_path,
        ) {
            info!("Loading custom TLS certificates from {:?}...", cert_path);
            // Nothing else binds the health-check port in this mode.
            Self::start_health_check_listener(&state, tcp_ip);
            Self::configure_manual_tls(cert_path, key_path)?
        } else if let Some(domain) = state.config.server.acme_domain.clone() {
            info!("Configuring ACME (Let's Encrypt) for domain {}...", domain);
            // configure_acme spawns its own Axum server on acme_bind_port that
            // serves the HTTP-01 challenge route AND a "/" health check, so we
            // must NOT also call start_health_check_listener — both would bind
            // acme_bind_port and the second bind would fail.
            Self::configure_acme(&domain, tcp_ip, &state)?
        } else {
            info!("No TLS cert or ACME domain configured. Checking persisted self-signed cert...");
            Self::start_health_check_listener(&state, tcp_ip);
            Self::generate_or_load_self_signed_config(&state)?
        };

        let mut server_config = server_config;
        let mut endpoint_config = quinn::EndpointConfig::default();
        if !state.config.server.mtu_discovery {
            info!("Path MTU discovery disabled; packets stay at 1200 bytes in both directions");
            server_config.transport_config(Arc::new(Self::transport_config(false)?));
            // Our own packets are capped by the transport config above. This
            // caps the peer's: it is the max_udp_payload_size we advertise,
            // and a client may not send, or probe with, anything larger. The
            // default (1472) invites exactly the oversized packets this
            // setting exists to keep off the path.
            endpoint_config
                .max_udp_payload_size(BASE_UDP_PAYLOAD_SIZE)
                .map_err(|e| anyhow!("Invalid max UDP payload size: {}", e))?;
        }

        info!("Creating QUIC endpoint on {}...", bind_addr);
        let socket = std::net::UdpSocket::bind(bind_addr)
            .map_err(|e| anyhow!("Failed to bind QUIC endpoint to {}: {}", bind_addr, e))?;
        let runtime = quinn::default_runtime()
            .ok_or_else(|| anyhow!("No async runtime for the QUIC endpoint"))?;
        let endpoint = Endpoint::new(endpoint_config, Some(server_config), socket, runtime)
            .map_err(|e| anyhow!("Failed to create QUIC endpoint on {}: {}", bind_addr, e))?;

        let local_addr = endpoint
            .local_addr()
            .map_err(|e| anyhow!("Failed to get local address: {}", e))?;

        info!("✓ QUIC server bound to UDP socket: {}", local_addr);
        info!("✓ ALPN protocol: '{}'", String::from_utf8_lossy(tls::ALPN));
        info!(
            "✓ TLS 1.3 key exchange: X25519MLKEM768 (hybrid post-quantum) with classical fallback"
        );

        let rate_limiter = HandshakeRateLimiter::new(
            state.config.server.handshake_per_minute,
            state.config.server.handshake_burst,
        );

        Ok(Self {
            endpoint,
            state,
            rate_limiter,
        })
    }

    /// The UDP address the QUIC endpoint is bound to (useful when the
    /// configured port was 0).
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint
            .local_addr()
            .map_err(|e| anyhow!("Failed to get local address: {}", e))
    }

    /// Configure ACME (Let's Encrypt) for automated certificate management.
    fn configure_acme(
        domain: &str,
        tcp_ip: IpAddr,
        state: &Arc<ServerState>,
    ) -> Result<ServerConfig> {
        let contact = state
            .config
            .server
            .acme_contact
            .clone()
            .unwrap_or_else(|| "admin@aura.local".to_string());
        let cache_path = state
            .config
            .server
            .acme_cache_path
            .clone()
            .unwrap_or_else(|| Path::new("data/acme").to_path_buf());

        // Ensure cache directory exists
        std::fs::create_dir_all(&cache_path)?;

        // Build ACME config — must set challenge_type to Http01
        let mut acme_builder = AcmeConfig::new([domain])
            .contact([format!("mailto:{}", contact)])
            .cache_with_boxed_err(DirCache::new(cache_path))
            .challenge_type(UseChallenge::Http01);

        if let Some(url) = &state.config.server.acme_directory_url {
            info!("[ACME] Using custom directory URL: {}", url);
            acme_builder = acme_builder.directory(url);
        } else {
            // rustls-acme's builder defaults to the Let's Encrypt STAGING
            // directory, whose certificates clients don't trust. No configured
            // URL means production.
            acme_builder = acme_builder.directory_lets_encrypt(true);
        }

        let mut acme_state = acme_builder.state();
        let resolver = acme_state.resolver();

        // Get the Tower service that automatically responds to HTTP-01 challenges.
        // This handles token lookup and key authorization computation internally.
        let challenge_service = acme_state.http01_challenge_tower_service();

        let acme_port = state.config.server.acme_bind_port.unwrap_or(8080);
        let tcp_bind_addr = SocketAddr::new(tcp_ip, acme_port);

        // Drive the ACME event loop in a background task
        tokio::spawn(async move {
            use tokio_stream::StreamExt;
            loop {
                match acme_state.next().await {
                    Some(Ok(ok)) => info!("[ACME] Event: {:?}", ok),
                    Some(Err(err)) => error!("[ACME] Error: {:?}", err),
                    None => break,
                }
            }
        });

        // Serve HTTP-01 challenges + health check via Axum on a background task
        tokio::spawn(async move {
            use axum::{routing::get, Router};

            let app = Router::new()
                .route_service(
                    "/.well-known/acme-challenge/{challenge_token}",
                    challenge_service,
                )
                .route("/", get(|| async { "Aura Server ACME/Health-check OK" }));

            info!(
                "[ACME] Driving HTTP-01 challenges via Axum on port {}...",
                acme_port
            );
            let listener = match tokio::net::TcpListener::bind(tcp_bind_addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!(
                        "[ACME] Failed to bind Axum listener on port {}: {}",
                        acme_port, e
                    );
                    return;
                }
            };

            if let Err(e) = axum::serve(listener, app).await {
                error!("[ACME] Axum server error on port {}: {}", acme_port, e);
            }
        });

        Self::quic_server_config(tls::server_config_builder()?.with_cert_resolver(resolver))
    }

    /// Minimal TCP listener for Fly.io health checks when ACME is disabled.
    fn start_health_check_listener(state: &Arc<ServerState>, tcp_ip: IpAddr) {
        let acme_port = state.config.server.acme_bind_port.unwrap_or(443);
        let tcp_bind_addr = SocketAddr::new(tcp_ip, acme_port);

        tokio::spawn(async move {
            info!(
                "[Network] Starting health-check TCP listener on port {}...",
                acme_port
            );
            let listener = match tokio::net::TcpListener::bind(tcp_bind_addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!("[Network] Failed to bind health-check listener: {}", e);
                    return;
                }
            };

            while let Ok((stream, _)) = listener.accept().await {
                // Just keep the port open to satisfy the health check
                drop(stream);
            }
        });
    }

    /// Configure manual TLS using certificates from the filesystem.
    fn configure_manual_tls(cert_path: &Path, key_path: &Path) -> Result<ServerConfig> {
        let cert_chain: Vec<CertificateDer> = CertificateDer::pem_file_iter(cert_path)
            .map_err(|e| anyhow!("Failed to read certificate file {:?}: {}", cert_path, e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow!("Failed to parse certificate in {:?}: {}", cert_path, e))?;

        let key = PrivateKeyDer::from_pem_file(key_path)
            .map_err(|e| anyhow!("Failed to read private key from {:?}: {}", key_path, e))?;

        Self::quic_server_config_from_cert(cert_chain, key)
    }

    /// Directory where `aura-cert.pem` / `aura-key.pem` should be persisted, or
    /// `None` for in-memory databases (`:memory:`).
    fn self_signed_cert_dir(db_path: &Path) -> Option<&Path> {
        if db_path == Path::new(":memory:") {
            return None;
        }
        match db_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => Some(p),
            _ => Some(Path::new(".")),
        }
    }

    /// Load persisted self-signed TLS certificate from the data directory, or
    /// generate and persist a new one so restarts preserve the TLS fingerprint.
    fn generate_or_load_self_signed_config(state: &ServerState) -> Result<ServerConfig> {
        let data_dir = Self::self_signed_cert_dir(&state.config.database.path);

        if let Some(dir) = data_dir {
            let cert_path = dir.join("aura-cert.pem");
            let key_path = dir.join("aura-key.pem");
            if cert_path.exists() && key_path.exists() {
                info!(
                    "Loading persisted self-signed TLS certificate from {:?}...",
                    cert_path
                );
                return Self::configure_manual_tls(&cert_path, &key_path);
            }

            let cert =
                rcgen::generate_simple_self_signed(vec!["localhost".into(), "aura.local".into()])?;
            let cert_pem = cert.cert.pem();
            let key_pem = cert.signing_key.serialize_pem();

            std::fs::create_dir_all(dir)?;
            let tmp_cert = dir.join(format!(".aura-cert.pem.tmp.{}", std::process::id()));
            let tmp_key = dir.join(format!(".aura-key.pem.tmp.{}", std::process::id()));
            {
                use std::io::Write;
                let mut f = std::fs::File::create(&tmp_cert)?;
                f.write_all(cert_pem.as_bytes())?;
                f.sync_all()?;
            }
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&tmp_key)?;
                f.write_all(key_pem.as_bytes())?;
                f.sync_all()?;
            }
            #[cfg(not(unix))]
            {
                use std::io::Write;
                let mut f = std::fs::File::create(&tmp_key)?;
                f.write_all(key_pem.as_bytes())?;
                f.sync_all()?;
            }
            std::fs::rename(&tmp_key, &key_path)?;
            std::fs::rename(&tmp_cert, &cert_path)?;

            info!(
                "Generated and persisted self-signed TLS certificate to {:?}",
                cert_path
            );
            return Self::configure_manual_tls(&cert_path, &key_path);
        }

        let cert =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "aura.local".into()])?;
        let cert_der = cert.cert.der().to_vec();
        let key_der = cert.signing_key.serialize_der();

        let cert_chain = vec![CertificateDer::from(cert_der)];
        let key = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(key_der));

        Self::quic_server_config_from_cert(cert_chain, key)
    }

    /// QUIC server config for a fixed certificate chain and key.
    pub(crate) fn quic_server_config_from_cert(
        cert_chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<ServerConfig> {
        Self::quic_server_config(tls::server_config_builder()?.with_single_cert(cert_chain, key)?)
    }

    /// Wrap a rustls config (built from [`tls::server_config_builder`], so
    /// hybrid PQ key exchange is on) for QUIC: set ALPN and apply Aura's
    /// transport settings.
    fn quic_server_config(mut server_crypto: rustls::ServerConfig) -> Result<ServerConfig> {
        server_crypto.alpn_protocols = vec![tls::ALPN.to_vec()];

        let quinn_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .map_err(|e| anyhow!("Failed to build QUIC TLS config: {}", e))?;

        Self::apply_transport_config(ServerConfig::with_crypto(Arc::new(quinn_crypto)))
    }

    /// Apply common transport settings for low-latency voice.
    fn apply_transport_config(mut server_config: ServerConfig) -> Result<ServerConfig> {
        server_config.transport_config(Arc::new(Self::transport_config(true)?));
        Ok(server_config)
    }

    /// Transport settings for low-latency voice. `mtu_discovery` is the
    /// `server.mtu_discovery` setting.
    fn transport_config(mtu_discovery: bool) -> Result<quinn::TransportConfig> {
        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into()?));
        transport.keep_alive_interval(Some(std::time::Duration::from_secs(5)));

        if !mtu_discovery {
            // Keep packets at QUIC's base size instead of probing for a
            // larger path MTU. Some paths treat an oversized probe badly: on
            // one deployment the first probe above ~1350 bytes was followed
            // by everything the server sent to that client being dropped for
            // about a minute, which hung logins right after the handshake.
            transport.mtu_discovery_config(None);
        }

        // Enable QUIC datagrams for unreliable audio packets
        transport.datagram_receive_buffer_size(Some(65536));
        transport.datagram_send_buffer_size(65536);
        // Client-opened bidirectional streams carry file uploads only (the
        // control stream is opened by the server); a few per user are
        // allowed (see file_relay), so cap them low.
        transport.max_concurrent_bidi_streams(16u32.into());

        Ok(transport)
    }

    /// Run the server, accepting connections.
    pub async fn run(&self) -> Result<()> {
        info!("QUIC server ready, waiting for connections...");
        info!("Listening for ALPN protocol: 'aura-dave'");

        while let Some(connecting) = self.endpoint.accept().await {
            let state = Arc::clone(&self.state);
            let remote = connecting.remote_address();

            // Per-IP handshake rate limit — reject before paying TLS or
            // Ed25519 CPU cost. Loopback is always allowed.
            if let Err(e) = self.rate_limiter.check(remote.ip()) {
                crate::metrics::METRICS.connection_rejected("rate_limited");
                warn!("[QUIC] Rejecting connection from {}: {}", remote, e);
                drop(connecting);
                continue;
            }

            info!("[QUIC] Incoming connection from {}", remote);

            tokio::spawn(async move {
                info!("[QUIC] Awaiting TLS handshake from {}", remote);
                match connecting.await {
                    Ok(connection) => {
                        let remote = connection.remote_address();
                        info!("[QUIC] TLS handshake complete from {}", remote);

                        let _open = crate::metrics::METRICS.connection_opened();
                        if let Err(e) = handle_connection(connection, state).await {
                            warn!("[QUIC] Connection error from {}: {}", remote, e);
                        }
                    }
                    Err(e) => {
                        crate::metrics::METRICS.connection_rejected("handshake_failed");
                        warn!("[QUIC] TLS handshake failed from {}: {}", remote, e);
                    }
                }
            });
        }

        Ok(())
    }
}

/// Telemetry name for a control-stream message type. Names only: the
/// payload, channel and sender never go into a span.
fn message_type_name(msg_type: u8) -> &'static str {
    match msg_type {
        MSG_JOIN_CHANNEL => "join_channel",
        MSG_TEXT_PACKET => "text_packet",
        MSG_CREATE_CHANNEL => "create_channel",
        MSG_UPDATE_CHANNEL => "update_channel",
        MSG_UPDATE_PROFILE => "update_profile",
        MSG_DELETE_CHANNEL => "delete_channel",
        MSG_DELETE_USER => "delete_user",
        MSG_UPDATE_STATUS => "update_status",
        MSG_LIST_USERS => "list_users",
        MSG_VERIFY_USER => "verify_user",
        MSG_BAN_USER => "ban_user",
        MSG_GRANT_ADMIN => "grant_admin",
        MSG_KICK_USER => "kick_user",
        MSG_SERVER_MUTE => "server_mute",
        MSG_MLS_JOIN => "mls_join",
        MSG_MLS_COMMIT_WELCOME => "mls_commit_welcome",
        MSG_MLS_RATCHET_COMMIT => "mls_ratchet_commit",
        _ => "other",
    }
}

/// Whether an audio packet names `session_id` as its sender (its first four
/// bytes, big-endian). Fan-out is decided by that field, so a packet may only
/// be relayed on behalf of the connection that sent it.
fn names_sender(packet: &[u8], session_id: u32) -> bool {
    packet.get(..4) == Some(&session_id.to_be_bytes()[..])
}

/// A login the server refused, as opposed to a broken or malformed
/// connection. Carries a reason code for telemetry.
#[derive(Debug, thiserror::Error)]
#[error("Authentication failed: {message}")]
struct LoginRefused {
    code: &'static str,
    message: String,
}

/// Handle a single QUIC connection.
async fn handle_connection(conn: Connection, state: Arc<ServerState>) -> Result<()> {
    let remote = conn.remote_address();
    info!("[{}] Connection established", remote);

    // Open control stream (reliable, bidirectional) - Server initiates for Apple compat
    let (control_send_initial, control_recv_initial) = conn
        .open_bi()
        .await
        .map_err(|e| anyhow!("Failed to open control stream: {}", e))?;

    info!("[{}] Control stream opened", remote);

    // Authenticate the client - returns session_id directly now
    let (session_id, user_uuid, control_send, control_recv, mut rx) =
        match authenticate_client(control_send_initial, control_recv_initial, &state, remote).await
        {
            Ok(result) => result,
            Err(e) => {
                warn!("[{}] Authentication failed: {}", remote, e);
                return Err(e);
            }
        };

    // Session already registered in authenticate_client
    info!(
        "[{}] Session {} authenticated for user {}",
        remote, session_id, user_uuid
    );

    // Send initial state of all channels to the new user
    state.send_server_snapshot(session_id).await;

    // Keepalive interval
    let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(10));
    let mut last_activity = std::time::Instant::now();
    let keepalive_timeout = std::time::Duration::from_secs(30);

    // Reader handle for datagrams (cloned)
    let datagram_conn = conn.clone();

    let ping_limiter = PingRateLimiter::new();

    // File transfers are delivered over server-opened streams on this connection.
    state.file_relay.register(session_id, conn.clone());

    // Initialize context
    let mut ctx = ConnectionContext {
        conn, // Move original connection handle
        send: control_send,
        recv: control_recv,
        state: Arc::clone(&state),
        remote,
        session_id,
        user_uuid: user_uuid.clone(),
        current_channel_id: None,
    };

    // The older audio format is logged once per connection, not per frame.
    let mut warned_legacy_audio = false;

    // Main Main Loop
    loop {
        tokio::select! {
            // 1. Unreliable audio datagrams (fast path)
            datagram = datagram_conn.read_datagram() => {
                match datagram {
                    Ok(data) => {
                        let data: bytes::Bytes = data;
                        last_activity = std::time::Instant::now();
                        if !data.is_empty() {
                            match data[0] {
                                // Audio. Skip empty audio frames (header only).
                                AUDIO_DATAGRAM if data.len() > 1 => {
                                    if !names_sender(&data[1..], session_id) {
                                        debug!("[{}] Dropped audio naming another session", remote);
                                        continue;
                                    }
                                    crate::metrics::METRICS.audio_datagram(data.len() - 1);
                                    ctx.state.route_audio_packet(bytes::Bytes::copy_from_slice(&data[1..])).await;
                                }
                                AUDIO_DATAGRAM_V1 if data.len() > 1 => {
                                    if !names_sender(&data[1..], session_id) {
                                        debug!("[{}] Dropped audio naming another session", remote);
                                        continue;
                                    }
                                    if !warned_legacy_audio {
                                        warned_legacy_audio = true;
                                        info!("[{}] Client sends the older audio format (v1); peers will drop it until it updates", remote);
                                    }
                                    ctx.state.route_legacy_audio_packet(bytes::Bytes::copy_from_slice(&data[1..])).await;
                                }
                                0x00 => {
                                    // Keepalive / RTT probe. Echo the full
                                    // datagram so a client-supplied nonce
                                    // (any trailing bytes) can be used to
                                    // measure round-trip latency.
                                    let _ = ctx.conn.send_datagram(data.clone());
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                         debug!("[{}] Datagram read error: {}", remote, e);
                    }
                }
            }

            // 1b. Client-opened bidirectional streams: encrypted file uploads.
            upload = datagram_conn.accept_bi() => {
                match upload {
                    Ok((send, recv)) => {
                        last_activity = std::time::Instant::now();
                        tokio::spawn(crate::file_relay::handle_upload(
                            Arc::clone(&ctx.state),
                            session_id,
                            user_uuid.clone(),
                            send,
                            recv,
                        ));
                    }
                    Err(e) => debug!("[{}] accept_bi ended: {}", remote, e),
                }
            }

            // 2. Control stream messages
            msg_type = ctx.recv.read_u8() => {
                match msg_type {
                    Ok(type_byte) => {
                        last_activity = std::time::Instant::now();
                        if type_byte == 0x00 {
                            debug!("[{}] Keepalive ping received", remote);
                            continue;
                        }
                        // RTT probes skip the per-message span and metrics:
                        // they arrive every few seconds from every client.
                        if type_byte == MSG_PING {
                            if let Err(e) = ctx.handle_ping(&ping_limiter).await {
                                warn!("[{}] Bad ping: {}", remote, e);
                                break;
                            }
                            continue;
                        }

                        // Root span per message; its type is the only detail.
                        let span = tracing::info_span!(
                            target: "aura::server",
                            parent: None,
                            "control_message",
                            msg_type = message_type_name(type_byte),
                            outcome = Empty,
                        );
                        let handled = ctx
                            .handle_client_message(type_byte)
                            .instrument(span.clone())
                            .await;
                        span.record("outcome", if handled.is_ok() { "ok" } else { "error" });
                        crate::metrics::METRICS
                            .control_message(message_type_name(type_byte), handled.is_ok());
                        drop(span);
                        match handled {
                            Ok(continue_conn) => {
                                if !continue_conn { break; }
                            }
                            Err(e) => {
                                warn!("[{}] Error handling message 0x{:02x}: {}", remote, type_byte, e);
                                break;
                            }
                        }
                    }
                    Err(_) => break, // Stream closed/error
                }
            }

            // 3. Service internal messages (relay)
            Some(msg) = rx.recv() => {
                match ctx.handle_service_message(msg).await {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => {
                        warn!("[{}] Error handling service message: {}", remote, e);
                        break;
                    }
                }
            }

            // 4. Keepalive timer
            _ = keepalive.tick() => {
                if last_activity.elapsed() > keepalive_timeout {
                    warn!("[{}] Session {} timed out", remote, session_id);
                    break;
                }
                // Send keepalive ping via datagram
                 let _ = ctx.conn.send_datagram(bytes::Bytes::from_static(&[0x00]));
            }
        }
    }

    // Cleanup
    ctx.leave_current_channel().await;
    state.file_relay.unregister(session_id);

    // Log bandwidth before removing session
    if let Some(sess) = state.sessions.get(&session_id) {
        let bytes_in = sess.bytes_in.load(std::sync::atomic::Ordering::Relaxed);
        let bytes_out = sess.bytes_out.load(std::sync::atomic::Ordering::Relaxed);
        info!(
            "[{}] Session {} bandwidth: {:.1} KB in, {:.1} KB out",
            remote,
            session_id,
            bytes_in as f64 / 1024.0,
            bytes_out as f64 / 1024.0,
        );
    }

    state.remove_session(session_id).await;
    info!("[{}] Session {} disconnected", remote, session_id);
    Ok(())
}

// Client session after authentication.
// (Was a struct AuthSession { session_id: u32, username: String } — left as a
//  comment for now, but no longer needed since session bookkeeping moved into
//  ServerState. Remove or restore as needed.)

/// Authenticate a client using TOFU protocol.
/// Server-first protocol for Apple Network.framework compatibility:
/// 1. Server sends ServerHello with challenge
/// 2. Client sends AuthRequest: identity key, name, hybrid signature over the transcript
/// 3. Server validates and sends AuthResponse
async fn authenticate_client(
    send: SendStream,
    recv: RecvStream,
    state: &Arc<ServerState>,
    remote: SocketAddr,
) -> Result<(
    u32,
    String,
    SendStream,
    RecvStream,
    tokio::sync::mpsc::UnboundedReceiver<ServiceMessage>,
)> {
    // Root span (not a child of the connection): one per login attempt, with
    // only an outcome code.
    let span = tracing::info_span!(target: "aura::server", parent: None, "auth", outcome = Empty);
    let result = authenticate_client_inner(send, recv, state, remote)
        .instrument(span.clone())
        .await;
    let outcome = match &result {
        Ok(_) => "ok",
        Err(e) => e
            .downcast_ref::<LoginRefused>()
            .map_or("protocol_error", |r| r.code),
    };
    span.record("outcome", outcome);
    crate::metrics::METRICS.login(outcome);
    result
}

async fn authenticate_client_inner(
    mut send: SendStream,
    mut recv: RecvStream,
    state: &Arc<ServerState>,
    remote: SocketAddr,
) -> Result<(
    u32,
    String,
    SendStream,
    RecvStream,
    tokio::sync::mpsc::UnboundedReceiver<ServiceMessage>,
)> {
    // Step 1: Server sends challenge first (ServerHello)
    let challenge = AuthService::generate_challenge();
    info!(
        "[Auth] Sending ServerHello with challenge: {}...",
        hex::encode(&challenge[..8])
    );

    let mut hello = BytesMut::new();
    hello.put_u8(MSG_CHALLENGE_RESPONSE); // Reuse message type for ServerHello
    hello.put_slice(&challenge);
    send.write_all(&hello).await?;
    send.flush().await?;
    info!("[Auth] Sent ServerHello ({} bytes)", hello.len());

    // Step 2: Wait for AuthRequest from client: [0x03][u32 LE length][AuthRequest]
    info!("[Auth] Waiting for AuthRequest...");
    let msg_type = recv.read_u8().await?;
    if msg_type != MSG_AUTH_REQUEST {
        return Err(anyhow!(
            "Expected AuthRequest (0x03), got 0x{:02x}",
            msg_type
        ));
    }
    let len = recv.read_u32_le().await? as usize;
    if len > MAX_AUTH_REQUEST_SIZE {
        return Err(anyhow!("AuthRequest too large ({} bytes)", len));
    }
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload).await?;
    let request = aura_protocol::AuthRequest::decode(payload.as_slice())
        .map_err(|e| anyhow!("Malformed AuthRequest: {}", e))?;
    info!(
        "[Auth] Display name: {} (client_version={:?}, protocol_version={:?})",
        request.display_name, request.client_version, request.protocol_version
    );

    // The signature covers this challenge; a mismatch means a stale or
    // replayed request.
    let auth_result: Result<_, LoginRefused> = if request.challenge != challenge {
        Err(LoginRefused {
            code: "challenge_mismatch",
            message: "Challenge mismatch".into(),
        })
    } else {
        state
            .auth
            .authenticate(
                &request.identity_public_key,
                &request.display_name,
                &request.signature,
                &challenge,
                request.server_password.as_deref(),
            )
            .map_err(|e| LoginRefused {
                code: e.code(),
                message: e.to_string(),
            })
    };

    // Step 3: AuthResponse: [0x04][u32 LE length][AuthResponse]
    let (response, outcome) = match auth_result {
        Ok(result) => {
            // Register session BEFORE sending auth response so we have a real session_id
            // IMPORTANT: Do NOT prefix rx with '_' — that would cause Rust to drop the
            // receiver immediately, silently discarding all service messages to this session.
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let session_id = state.register_session(result.user_uuid.to_string(), remote, tx);
            info!(
                "[Auth] Registered session {} for user {}",
                session_id, result.user_uuid
            );
            debug!(
                "[Auth] Sending AuthResponse: session_id={}, verified={}, is_admin={}",
                session_id, result.verified, result.is_admin
            );
            let response = aura_protocol::AuthResponse {
                success: true,
                session_id,
                session_token: result.session_token.clone(),
                verified: result.verified,
                error_message: String::new(),
                user_uuid: result.user_uuid.clone(),
                is_admin: result.is_admin,
                server_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                protocol_version: Some(aura_protocol::PROTOCOL_VERSION),
            };
            (response, Ok((session_id, result.user_uuid, rx)))
        }
        Err(refused) => {
            let response = aura_protocol::AuthResponse {
                success: false,
                error_message: refused.message.clone(),
                server_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                protocol_version: Some(aura_protocol::PROTOCOL_VERSION),
                ..Default::default()
            };
            (response, Err(anyhow::Error::new(refused)))
        }
    };

    let payload = response.encode_to_vec();
    let mut frame = BytesMut::with_capacity(5 + payload.len());
    frame.put_u8(MSG_AUTH_RESPONSE);
    frame.put_u32_le(payload.len() as u32);
    frame.put_slice(&payload);
    send.write_all(&frame).await?;

    if outcome.is_err() {
        // A refused login drops the connection, which would discard the
        // response if it hasn't been delivered yet, and the client would see
        // a lost connection instead of why (banned, wrong password, ...).
        // Wait for the client to acknowledge it, briefly.
        let _ = send.finish();
        let _ = timeout(REFUSAL_DELIVERY_TIMEOUT, send.stopped()).await;
    }

    let (session_id, user_uuid, rx) = outcome?;
    Ok((session_id, user_uuid, send, recv, rx))
}

/// Context for an active client connection
struct ConnectionContext {
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
    state: Arc<ServerState>,
    remote: SocketAddr,
    session_id: u32,
    user_uuid: String,
    current_channel_id: Option<String>,
}

impl ConnectionContext {
    async fn handle_client_message(&mut self, msg_type: u8) -> Result<bool> {
        match msg_type {
            0x00 => {
                // Keepalive ping, ignore
            }
            MSG_JOIN_CHANNEL => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::JoinChannelRequest::decode(&buf[..])?;
                let channel_id = req.channel_id;

                info!("[{}] Joining channel {}", self.remote, channel_id);

                // Leave previous channel if any. A join is a move, and the
                // client has already moved on locally, even if the channel
                // it asked for turns out not to exist.
                self.leave_current_channel().await;

                if !self.state.channel_exists(&channel_id) {
                    // Typically a client reconnecting into a channel that was
                    // deleted while it was away. Joining would leave it in a
                    // group nobody can see and let its MLS join create one.
                    // Answer the way clients already handle a deletion: point
                    // them at the lobby.
                    info!(
                        "[{}] Refused join to unknown channel {}",
                        self.remote, channel_id
                    );
                    let notice = aura_protocol::ChannelDeleted {
                        channel_id,
                        fallback_channel_id: self.state.lobby_channel_id().unwrap_or_default(),
                        reason: aura_protocol::ChannelRemovalReason::NotFound as i32,
                        detail: String::new(),
                    };
                    self.send_proto_response(MSG_CHANNEL_DELETED, notice)
                        .await?;
                    return Ok(true);
                }

                // Join new channel
                self.current_channel_id = Some(channel_id.clone());
                self.state
                    .add_to_voice_group(channel_id.clone(), self.session_id)
                    .await;
                self.state
                    .add_to_text_group(channel_id.clone(), self.session_id)
                    .await;

                // Broadcast join
                if let Some(profile) = self.state.get_profile(&self.user_uuid) {
                    self.state
                        .broadcast_user_joined(channel_id, self.session_id, profile.display_name)
                        .await;
                }
            }
            MSG_AUDIO | MSG_AUDIO_V1 => {
                // Reliable audio path. Format: [21|20] [Length u32 LE] [Payload...]
                // 0x21 is the current format, 0x20 the older one (relayed under
                // its own tag so receivers can report it).
                let legacy = msg_type == MSG_AUDIO_V1;
                let mut len_buf = [0u8; 4];
                self.recv.read_exact(&mut len_buf).await?;
                let packet_len = u32::from_le_bytes(len_buf) as usize;

                trace!(
                    "[{}] Received audio packet: {} bytes",
                    self.remote,
                    packet_len
                );

                // Limit audio packet size too
                if packet_len > MAX_AUDIO_PACKET_SIZE {
                    warn!("[{}] Audio packet too large: {}", self.remote, packet_len);
                    return Ok(false); // Disconnect
                }

                let mut packet_buf = Vec::new();
                (&mut self.recv)
                    .take(packet_len as u64)
                    .read_to_end(&mut packet_buf)
                    .await?;
                if packet_buf.len() != packet_len {
                    return Err(anyhow!("Incomplete audio packet received"));
                }

                if !names_sender(&packet_buf, self.session_id) {
                    debug!("[{}] Dropped audio naming another session", self.remote);
                    return Ok(true);
                }
                let packet = bytes::Bytes::from(packet_buf);
                if legacy {
                    self.state.route_legacy_audio_packet(packet).await;
                } else {
                    self.state.route_audio_packet(packet).await;
                }
                trace!(
                    "[{}] Routed audio packet from session {}",
                    self.remote,
                    self.session_id
                );
            }
            MSG_TEXT_PACKET => {
                let packet_buf = self.read_frame_payload().await?;
                let packet = aura_protocol::EncryptedTextPacket::decode(&packet_buf[..])?;

                info!(
                    "[{}] Text packet from session {} in channel {} (msgId: {})",
                    self.remote, packet.sender_session_id, packet.channel_id, packet.message_id
                );

                let channel_id_for_ratchet = packet.channel_id.clone();

                // The relay tells us when the group has crossed its ratchet
                // threshold; acting on it is what makes batched PFS ratcheting
                // real rather than a counter nobody reads.
                let should_ratchet = self
                    .state
                    .broadcast_text_message(self.session_id, packet)
                    .await;

                if should_ratchet {
                    self.state
                        .request_text_ratchet(channel_id_for_ratchet)
                        .await;
                }
            }
            MSG_CREATE_CHANNEL => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::CreateChannelRequest::decode(&buf[..])?;

                // Only admins with manage_channels can create channels
                if let Some(denied) = self.channel_permission_error()? {
                    let resp = aura_protocol::CreateChannelResponse {
                        success: false,
                        channel_id: String::new(),
                        error_message: denied.into(),
                    };
                    self.send_proto_response(0x40, resp).await?;
                    return Ok(true);
                }

                match self
                    .state
                    .create_channel_persistent(req.name, req.comment, req.icon)
                    .await
                {
                    Ok(id) => {
                        let resp = aura_protocol::CreateChannelResponse {
                            success: true,
                            channel_id: id,
                            error_message: String::new(),
                        };
                        self.send_proto_response(0x40, resp).await?;
                    }
                    Err(e) => {
                        let resp = aura_protocol::CreateChannelResponse {
                            success: false,
                            channel_id: String::new(),
                            error_message: e.to_string(),
                        };
                        self.send_proto_response(0x40, resp).await?;
                    }
                }
            }
            MSG_UPDATE_CHANNEL => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::UpdateChannelRequest::decode(&buf[..])?;

                // Only admins with manage_channels can update channels
                if let Some(denied) = self.channel_permission_error()? {
                    let resp = aura_protocol::MetadataUpdateResponse {
                        success: false,
                        error_message: denied.into(),
                    };
                    self.send_proto_response(0x41, resp).await?;
                    return Ok(true);
                }

                match self
                    .state
                    .update_channel_persistent(
                        req.channel_id,
                        req.name,
                        req.comment,
                        req.icon,
                        req.position,
                    )
                    .await
                {
                    Ok(_) => {
                        let resp = aura_protocol::MetadataUpdateResponse {
                            success: true,
                            error_message: String::new(),
                        };
                        self.send_proto_response(0x41, resp).await?;
                    }
                    Err(e) => {
                        let resp = aura_protocol::MetadataUpdateResponse {
                            success: false,
                            error_message: e.to_string(),
                        };
                        self.send_proto_response(0x41, resp).await?;
                    }
                }
            }
            MSG_UPDATE_PROFILE => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::UpdateProfile::decode(&buf[..])?;

                // Ensure they are only updating their own user_id
                if let Some(profile) = req.profile {
                    if profile.user_id != self.user_uuid {
                        let resp = aura_protocol::MetadataUpdateResponse {
                            success: false,
                            error_message: "Cannot update other profiles".into(),
                        };
                        self.send_proto_response(0x42, resp).await?;
                        return Ok(true);
                    }

                    match self
                        .state
                        .update_profile_persistent(self.session_id, profile)
                        .await
                    {
                        Ok(_) => {
                            let resp = aura_protocol::MetadataUpdateResponse {
                                success: true,
                                error_message: String::new(),
                            };
                            self.send_proto_response(0x42, resp).await?;
                        }
                        Err(e) => {
                            let resp = aura_protocol::MetadataUpdateResponse {
                                success: false,
                                error_message: e.to_string(),
                            };
                            self.send_proto_response(0x42, resp).await?;
                        }
                    }
                }
            }
            MSG_DELETE_CHANNEL => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::DeleteChannelRequest::decode(&buf[..])?;

                // Only admins with manage_channels can delete channels
                if let Some(denied) = self.channel_permission_error()? {
                    let resp = aura_protocol::AdminResponse {
                        success: false,
                        error_message: denied.into(),
                    };
                    self.send_proto_response(MSG_DELETE_CHANNEL, resp).await?;
                    return Ok(true);
                }

                match self.state.delete_channel_persistent(&req.channel_id).await {
                    Ok(_) => {
                        let resp = aura_protocol::AdminResponse {
                            success: true,
                            error_message: String::new(),
                        };
                        self.send_proto_response(MSG_DELETE_CHANNEL, resp).await?;
                    }
                    Err(e) => {
                        let resp = aura_protocol::AdminResponse {
                            success: false,
                            error_message: e.to_string(),
                        };
                        self.send_proto_response(MSG_DELETE_CHANNEL, resp).await?;
                    }
                }
            }
            MSG_DELETE_USER => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::DeleteUserRequest::decode(&buf[..])?;

                // Users may delete their own account; deleting someone
                // else's is moderation (ban_users, and the same rules as a ban).
                if let Err(e) = self
                    .state
                    .auth
                    .authorize_delete_user(&self.user_uuid, &req.user_uuid)
                {
                    let resp = aura_protocol::AdminResponse {
                        success: false,
                        error_message: e.client_message(),
                    };
                    self.send_proto_response(MSG_DELETE_USER, resp).await?;
                    return Ok(true);
                }

                match self.state.delete_user_persistent(&req.user_uuid).await {
                    Ok(_) => {
                        let resp = aura_protocol::AdminResponse {
                            success: true,
                            error_message: String::new(),
                        };
                        self.send_proto_response(MSG_DELETE_USER, resp).await?;
                    }
                    Err(e) => {
                        let resp = aura_protocol::AdminResponse {
                            success: false,
                            error_message: e.to_string(),
                        };
                        self.send_proto_response(MSG_DELETE_USER, resp).await?;
                    }
                }
            }
            MSG_UPDATE_STATUS => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::UserStatusUpdate::decode(&buf[..])?;

                // Only allow users to update their own status
                if req.session_id != self.session_id {
                    warn!(
                        "[{}] Session {} tried to update status for {}",
                        self.remote, self.session_id, req.session_id
                    );
                    return Ok(true);
                }

                let presence = req
                    .presence
                    .and_then(|p| aura_protocol::PresenceStatus::try_from(p).ok());
                self.state
                    .broadcast_user_status_with_presence(
                        req.session_id,
                        req.is_muted,
                        req.is_deafened,
                        presence,
                        req.custom_status,
                    )
                    .await;
            }
            // Moderation. The requests' `session_token` fields are ignored:
            // like every other admin operation here, these are authorised
            // with the identity this connection logged in as, and the
            // permissions that account holds now.
            MSG_LIST_USERS => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::ListUsersRequest::decode(&buf[..])?;
                let resp = self
                    .state
                    .list_users(&self.user_uuid, req.offset, req.limit)
                    .unwrap_or_else(|e| {
                        info!("[{}] List users refused: {}", self.remote, e);
                        aura_protocol::ListUsersResponse {
                            success: false,
                            error_message: e.client_message(),
                            ..Default::default()
                        }
                    });
                self.send_proto_response(MSG_LIST_USERS, resp).await?;
            }
            MSG_VERIFY_USER => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::VerifyUserRequest::decode(&buf[..])?;
                let result = self
                    .state
                    .auth
                    .verify_user(&self.user_uuid, &req.target_user_uuid);
                self.send_admin_response(
                    MSG_VERIFY_USER,
                    "Verify user",
                    result,
                    AuthError::client_message,
                )
                .await?;
            }
            MSG_BAN_USER => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::BanUserRequest::decode(&buf[..])?;
                let result =
                    self.state
                        .set_user_banned(&self.user_uuid, &req.target_user_uuid, req.banned);
                if result.is_ok() && req.banned {
                    info!(
                        "[{}] Ban of {} gave reason {:?}",
                        self.remote, req.target_user_uuid, req.reason
                    );
                }
                self.send_admin_response(
                    MSG_BAN_USER,
                    "Ban user",
                    result,
                    AuthError::client_message,
                )
                .await?;
            }
            MSG_GRANT_ADMIN => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::GrantAdminRequest::decode(&buf[..])?;
                let permissions = AdminPermissions {
                    verify_users: req.verify_users,
                    ban_users: req.ban_users,
                    grant_admin: req.grant_admin,
                    manage_channels: req.manage_channels,
                };
                let result = self.state.auth.grant_admin(
                    &self.user_uuid,
                    &req.target_user_uuid,
                    &permissions,
                );
                self.send_admin_response(
                    MSG_GRANT_ADMIN,
                    "Grant admin",
                    result,
                    AuthError::client_message,
                )
                .await?;
            }
            MSG_KICK_USER => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::KickUserRequest::decode(&buf[..])?;
                let result = self
                    .state
                    .kick_user(&self.user_uuid, &req.target_user_uuid, &req.reason)
                    .map(|_| ());
                self.send_admin_response(
                    MSG_KICK_USER,
                    "Kick user",
                    result,
                    ModerationError::client_message,
                )
                .await?;
            }
            MSG_SERVER_MUTE => {
                let buf = self.read_frame_payload().await?;
                let req = aura_protocol::ServerMuteRequest::decode(&buf[..])?;
                let result =
                    self.state
                        .set_server_muted(&self.user_uuid, req.target_session_id, req.muted);
                self.send_admin_response(
                    MSG_SERVER_MUTE,
                    "Server mute",
                    result,
                    ModerationError::client_message,
                )
                .await?;
            }
            MSG_MLS_JOIN => {
                let buf = self.read_frame_payload().await?;
                let envelope = aura_protocol::MlsEnvelope::decode(&buf[..])?;
                let channel_id = envelope.channel_id.clone();
                let is_voice = envelope.group_type() == aura_protocol::MlsGroupType::Voice;

                let Some(content) = envelope.content else {
                    return Err(anyhow!("MLS join envelope missing content"));
                };

                let key_package = match content {
                    aura_protocol::mls_envelope::Content::KeyPackage(kp) => kp,
                    _ => return Err(anyhow!("MLS join envelope must contain key_package")),
                };

                // Key packages are only taken for the channel this session is
                // in. Clients send them right after JoinChannel, so one for
                // any other channel follows a refused join (the channel
                // doesn't exist) or raced the channel's deletion, and
                // handling it would bring the channel's MLS group back.
                let in_channel = self.current_channel_id.as_deref().is_some_and(|current| {
                    aura_protocol::make_mls_group_id(current, is_voice)
                        == aura_protocol::make_mls_group_id(&channel_id, is_voice)
                });
                if !in_channel {
                    warn!(
                        "[{}] Ignoring MLS join for channel {}, which session {} is not in",
                        self.remote, channel_id, self.session_id
                    );
                    return Ok(true);
                }

                info!(
                    "[{}] MLS join for {} channel {}",
                    self.remote,
                    if is_voice { "voice" } else { "text" },
                    channel_id
                );

                self.state
                    .handle_mls_join(
                        channel_id,
                        is_voice,
                        self.session_id,
                        self.user_uuid.clone(),
                        key_package,
                    )
                    .await;
            }
            MSG_MLS_COMMIT_WELCOME => {
                let buf = self.read_frame_payload_limited(MAX_MLS_PACKET_SIZE).await?;
                let envelope = aura_protocol::MlsEnvelope::decode(&buf[..])?;
                let channel_id = envelope.channel_id.clone();
                let is_voice = envelope.group_type() == aura_protocol::MlsGroupType::Voice;

                let Some(content) = envelope.content else {
                    return Err(anyhow!("MLS commit/welcome envelope missing content"));
                };

                let (new_member_session_id, commit, welcome) = match content {
                    aura_protocol::mls_envelope::Content::CommitWelcome(cw) => {
                        (cw.new_member_session_id, cw.commit, cw.welcome)
                    }
                    _ => {
                        return Err(anyhow!(
                            "MLS commit/welcome envelope must contain commit_welcome"
                        ))
                    }
                };

                info!(
                    "[{}] MLS commit/welcome for {} channel {} (new member: {})",
                    self.remote,
                    if is_voice { "voice" } else { "text" },
                    channel_id,
                    new_member_session_id
                );

                self.state
                    .handle_mls_commit_welcome(
                        channel_id,
                        is_voice,
                        self.session_id,
                        new_member_session_id,
                        commit,
                        welcome,
                    )
                    .await;
            }
            MSG_MLS_RATCHET_COMMIT => {
                let buf = self.read_frame_payload_limited(MAX_MLS_PACKET_SIZE).await?;
                let envelope = aura_protocol::MlsEnvelope::decode(&buf[..])?;
                let channel_id = envelope.channel_id.clone();
                let is_voice = envelope.group_type() == aura_protocol::MlsGroupType::Voice;

                let Some(aura_protocol::mls_envelope::Content::Commit(commit)) = envelope.content
                else {
                    return Err(anyhow!("MLS ratchet envelope must contain commit"));
                };

                info!(
                    "[{}] MLS ratchet commit for {} channel {}",
                    self.remote,
                    if is_voice { "voice" } else { "text" },
                    channel_id
                );

                self.state
                    .handle_mls_ratchet_commit(channel_id, is_voice, self.session_id, commit)
                    .await;
            }
            _ => {
                // Unknown message. Every message but the 0x00 keepalive is
                // framed, so skip the whole frame: reading on from the length
                // prefix would take its bytes for message types and lose
                // sync with the client. Skipping also lets newer clients send
                // messages this server doesn't know.
                let skipped = self.skip_frame_payload(MAX_MLS_PACKET_SIZE).await?;
                warn!(
                    "[{}] Skipped unknown message type 0x{:02x} ({} bytes)",
                    self.remote, msg_type, skipped
                );
            }
        }
        Ok(true)
    }

    /// Leave the channel this session is in, if any: drop it from the
    /// channel's voice and text groups and tell everyone it left.
    async fn leave_current_channel(&mut self) {
        if let Some(channel_id) = self.current_channel_id.take() {
            // A transfer in flight from this session must not outlive its membership.
            self.state.file_relay.abort_sender(self.session_id);
            self.state
                .remove_from_voice_group(channel_id.clone(), self.session_id)
                .await;
            self.state
                .remove_from_text_group(channel_id.clone(), self.session_id)
                .await;
            self.state
                .broadcast_user_left(channel_id, self.session_id)
                .await;
        }
    }

    /// Why this connection's user may not create, edit or delete channels,
    /// or `None` if they may (an admin holding `manage_channels`). Non-admins
    /// get "Admin required", which the macOS client takes as its cue to hide
    /// its admin controls.
    fn channel_permission_error(&self) -> Result<Option<&'static str>> {
        Ok(
            match self.state.db.get_admin_permissions(&self.user_uuid)? {
                None => Some("Admin required"),
                Some(p) if !p.manage_channels => Some("Channel management permission required"),
                Some(_) => None,
            },
        )
    }

    /// Answer a control-stream ping: `[MSG_PING][u32 LE 8][nonce]` comes in,
    /// `[MSG_PONG][u32 LE 8][nonce]` goes back. The reply is built in place
    /// in a stack buffer. Pings over the rate limit are read and dropped.
    async fn handle_ping(&mut self, limiter: &PingRateLimiter) -> Result<()> {
        let mut frame = [0u8; 5 + PING_NONCE_LEN];
        frame[0] = MSG_PONG;
        timeout(FRAME_READ_TIMEOUT, self.recv.read_exact(&mut frame[1..5]))
            .await
            .map_err(|_| anyhow!("Ping read timed out"))??;
        let len = u32::from_le_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
        if len != PING_NONCE_LEN {
            return Err(anyhow!(
                "Ping must carry a {}-byte nonce, got {} bytes",
                PING_NONCE_LEN,
                len
            ));
        }
        timeout(FRAME_READ_TIMEOUT, self.recv.read_exact(&mut frame[5..]))
            .await
            .map_err(|_| anyhow!("Ping read timed out"))??;

        if limiter.check() {
            self.send.write_all(&frame).await?;
            self.send.flush().await?;
        } else {
            debug!("[{}] Ping over the rate limit dropped", self.remote);
        }
        Ok(())
    }

    /// Read the rest of a frame (`[u32 LE length][payload]`) and discard it,
    /// without buffering it. Returns the payload length.
    async fn skip_frame_payload(&mut self, max_len: usize) -> Result<usize> {
        let mut len_buf = [0u8; 4];
        self.recv.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len > max_len {
            return Err(anyhow!(
                "Incoming frame too large: {} bytes (max {})",
                len,
                max_len
            ));
        }

        let skipped = timeout(
            FRAME_READ_TIMEOUT,
            tokio::io::copy(
                &mut (&mut self.recv).take(len as u64),
                &mut tokio::io::sink(),
            ),
        )
        .await
        .map_err(|_| anyhow!("Frame read timed out after {:?}", FRAME_READ_TIMEOUT))??;
        if skipped != len as u64 {
            return Err(anyhow!(
                "Incomplete frame: got {} of {} bytes",
                skipped,
                len
            ));
        }
        Ok(len)
    }

    /// Reply to a moderation request with an `AdminResponse`. A refusal is
    /// logged in full; the client gets `client_message` of it.
    async fn send_admin_response<E: std::fmt::Display>(
        &mut self,
        msg_type: u8,
        operation: &str,
        result: std::result::Result<(), E>,
        client_message: fn(&E) -> String,
    ) -> Result<()> {
        let resp = match result {
            Ok(()) => aura_protocol::AdminResponse {
                success: true,
                error_message: String::new(),
            },
            Err(e) => {
                info!("[{}] {} refused: {}", self.remote, operation, e);
                aura_protocol::AdminResponse {
                    success: false,
                    error_message: client_message(&e),
                }
            }
        };
        self.send_proto_response(msg_type, resp).await
    }

    /// Send an audio packet to this client under the given tags: a QUIC
    /// datagram for low latency, the reliable stream if that fails.
    async fn relay_audio(
        &mut self,
        packet: &bytes::Bytes,
        datagram_type: u8,
        stream_type: u8,
    ) -> Result<()> {
        let mut dgram_data = vec![datagram_type];
        dgram_data.extend_from_slice(packet);

        if self
            .conn
            .send_datagram(bytes::Bytes::from(dgram_data))
            .is_err()
        {
            self.send.write_u8(stream_type).await?;
            self.send.write_u32_le(packet.len() as u32).await?;
            self.send.write_all(packet).await?;
            self.send.flush().await?;
        }
        Ok(())
    }

    /// Act on a message from the rest of the server. `Ok(false)` ends the
    /// connection.
    async fn handle_service_message(&mut self, msg: ServiceMessage) -> Result<bool> {
        match msg {
            ServiceMessage::RelayAudio(packet) => {
                self.relay_audio(&packet, AUDIO_DATAGRAM, MSG_AUDIO).await?;
            }
            ServiceMessage::RelayLegacyAudio(packet) => {
                self.relay_audio(&packet, AUDIO_DATAGRAM_V1, MSG_AUDIO_V1)
                    .await?;
            }
            ServiceMessage::UserJoined {
                channel_id,
                session_id,
                display_name,
                user_uuid,
            } => {
                let msg = aura_protocol::UserJoined {
                    channel_id,
                    session_id,
                    display_name,
                    user_uuid,
                };
                self.send_proto_response(0x11, msg).await?;
            }
            ServiceMessage::UserLeft {
                channel_id,
                session_id,
            } => {
                let msg = aura_protocol::UserLeft {
                    channel_id,
                    session_id,
                };
                self.send_proto_response(0x12, msg).await?;
            }
            ServiceMessage::ServerSnapshot(snapshot) => {
                let mut payload = Vec::new();
                snapshot.encode(&mut payload)?;

                let mut msg = vec![0x13u8]; // MSG_CHANNEL_STATE
                msg.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                msg.extend_from_slice(&payload);

                self.send.write_all(&msg).await?;
                self.send.flush().await?;
            }
            ServiceMessage::RelayText(text_packet) => {
                self.send_proto_response(MSG_TEXT_PACKET, text_packet)
                    .await?;
            }

            // --- MLS Protocol Messages ---
            ServiceMessage::MlsCreateGroup {
                channel_id,
                is_voice,
            } => {
                let envelope = aura_protocol::MlsEnvelope {
                    sender_id: self.session_id,
                    channel_id,
                    group_type: if is_voice {
                        aura_protocol::MlsGroupType::Voice as i32
                    } else {
                        aura_protocol::MlsGroupType::Text as i32
                    },
                    epoch: 0,
                    content: Some(aura_protocol::mls_envelope::Content::CreateGroup(true)),
                    ..Default::default()
                };
                self.send_proto_response(0x52, envelope).await?;
            }
            ServiceMessage::MlsAddMemberRequest {
                channel_id,
                is_voice,
                joiner_session_id,
                joiner_uuid,
                key_package,
            } => {
                let envelope = aura_protocol::MlsEnvelope {
                    sender_id: self.session_id,
                    channel_id,
                    group_type: if is_voice {
                        aura_protocol::MlsGroupType::Voice as i32
                    } else {
                        aura_protocol::MlsGroupType::Text as i32
                    },
                    target_session_id: joiner_session_id,
                    target_uuid: joiner_uuid,
                    content: Some(aura_protocol::mls_envelope::Content::KeyPackage(
                        key_package,
                    )),
                    ..Default::default()
                };
                self.send_proto_response(0x53, envelope).await?;
            }
            ServiceMessage::MlsCommit {
                channel_id,
                is_voice,
                commit,
            } => {
                let envelope = aura_protocol::MlsEnvelope {
                    sender_id: self.session_id,
                    channel_id,
                    group_type: if is_voice {
                        aura_protocol::MlsGroupType::Voice as i32
                    } else {
                        aura_protocol::MlsGroupType::Text as i32
                    },
                    content: Some(aura_protocol::mls_envelope::Content::Commit(commit)),
                    ..Default::default()
                };
                self.send_proto_response(0x54, envelope).await?;
            }
            ServiceMessage::MlsWelcome {
                channel_id,
                is_voice,
                welcome,
            } => {
                let envelope = aura_protocol::MlsEnvelope {
                    sender_id: self.session_id,
                    channel_id,
                    group_type: if is_voice {
                        aura_protocol::MlsGroupType::Voice as i32
                    } else {
                        aura_protocol::MlsGroupType::Text as i32
                    },
                    content: Some(aura_protocol::mls_envelope::Content::Welcome(welcome)),
                    ..Default::default()
                };
                self.send_proto_response(0x55, envelope).await?;
            }
            ServiceMessage::UserStatusUpdate {
                session_id,
                is_muted,
                is_deafened,
                presence,
                custom_status,
                is_server_muted,
            } => {
                let update = aura_protocol::UserStatusUpdate {
                    session_id,
                    is_muted,
                    is_deafened,
                    presence: presence.map(|p| p as i32),
                    custom_status,
                    is_server_muted: Some(is_server_muted),
                };
                self.send_proto_response(MSG_UPDATE_STATUS, update).await?;
            }
            ServiceMessage::ProfileUpdated(profile) => {
                self.send_proto_response(MSG_PROFILE_UPDATED, profile)
                    .await?;
            }
            ServiceMessage::ChannelDeleted {
                channel_id,
                fallback_channel_id,
            } => {
                // Stop tracking the dead channel, otherwise the disconnect
                // cleanup path would later try to leave a channel that no
                // longer exists.
                if self.current_channel_id.as_deref() == Some(channel_id.as_str()) {
                    self.current_channel_id = None;
                }

                let notice = aura_protocol::ChannelDeleted {
                    channel_id,
                    fallback_channel_id,
                    reason: aura_protocol::ChannelRemovalReason::Deleted as i32,
                    detail: String::new(),
                };
                self.send_proto_response(MSG_CHANNEL_DELETED, notice)
                    .await?;
            }
            ServiceMessage::MlsRatchetRequest {
                channel_id,
                is_voice,
            } => {
                let envelope = aura_protocol::MlsEnvelope {
                    sender_id: 0,
                    channel_id,
                    group_type: if is_voice {
                        aura_protocol::MlsGroupType::Voice as i32
                    } else {
                        aura_protocol::MlsGroupType::Text as i32
                    },
                    epoch: 0,
                    target_session_id: 0,
                    target_uuid: String::new(),
                    content: None,
                };
                self.send_proto_response(MSG_MLS_RATCHET_REQUEST, envelope)
                    .await?;
            }
            ServiceMessage::MlsSyncMembers {
                channel_id,
                is_voice,
                member_uuids,
            } => {
                let sync = aura_protocol::MlsSyncMembers {
                    channel_id,
                    group_type: if is_voice {
                        aura_protocol::MlsGroupType::Voice as i32
                    } else {
                        aura_protocol::MlsGroupType::Text as i32
                    },
                    member_uuids,
                };
                self.send_proto_response(MSG_MLS_SYNC_MEMBERS, sync).await?;
            }
            ServiceMessage::Kicked {
                channel_id,
                fallback_channel_id,
                reason,
            } => {
                // The client may have moved on by itself since the kick.
                if self.current_channel_id.as_deref() == Some(channel_id.as_str()) {
                    info!(
                        "[{}] Session {} kicked from channel {}",
                        self.remote, self.session_id, channel_id
                    );
                    self.leave_current_channel().await;
                    let notice = aura_protocol::ChannelDeleted {
                        channel_id,
                        fallback_channel_id,
                        reason: aura_protocol::ChannelRemovalReason::Kicked as i32,
                        detail: reason,
                    };
                    self.send_proto_response(MSG_CHANNEL_DELETED, notice)
                        .await?;
                }
            }
            ServiceMessage::Banned => {
                info!(
                    "[{}] Closing session {}: account banned",
                    self.remote, self.session_id
                );
                self.conn.close(CLOSE_BANNED.into(), b"banned");
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn read_frame_payload(&mut self) -> Result<Vec<u8>> {
        self.read_frame_payload_limited(MAX_CONTROL_PACKET_SIZE)
            .await
    }

    async fn read_frame_payload_limited(&mut self, max_len: usize) -> Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.recv.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;

        if len > max_len {
            return Err(anyhow!(
                "Incoming frame too large: {} bytes (max {})",
                len,
                max_len
            ));
        }

        if len == 0 {
            return Ok(Vec::new());
        }

        // Do NOT pre-allocate based on the attacker-controlled length prefix:
        // that lets a client claim the maximum and then send nothing, pinning one
        // buffer per connection. take() caps the upper bound, read_to_end
        // grows the Vec only as bytes actually arrive, and the timeout
        // bounds how long a single frame may remain in flight.
        let mut buf = Vec::new();
        let read = timeout(
            FRAME_READ_TIMEOUT,
            (&mut self.recv).take(len as u64).read_to_end(&mut buf),
        )
        .await
        .map_err(|_| anyhow!("Frame read timed out after {:?}", FRAME_READ_TIMEOUT))??;

        if read != len {
            return Err(anyhow!("Incomplete frame: got {} of {} bytes", read, len));
        }
        Ok(buf)
    }

    async fn send_proto_response<M: Message>(&mut self, msg_type: u8, msg: M) -> Result<()> {
        let mut payload = Vec::new();
        msg.encode(&mut payload)?;

        let mut header = vec![msg_type];
        header.extend_from_slice(&(payload.len() as u32).to_le_bytes());

        self.send.write_all(&header).await?;
        self.send.write_all(&payload).await?;
        self.send.flush().await?;
        Ok(())
    }
}

// Helper functions for packet handling

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::crypto::aws_lc_rs::{self, kx_group};
    use rustls::crypto::{CryptoProvider, SupportedKxGroup};

    mod file_transfer;

    /// Complete a real QUIC handshake against the production server config
    /// from a client offering only `groups`. Returns the ALPN it agreed on.
    async fn quic_handshake(groups: &[&'static dyn SupportedKxGroup]) -> Result<Vec<u8>> {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
        let cert_der = CertificateDer::from(cert.cert.der().to_vec());
        let key = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
            cert.signing_key.serialize_der(),
        ));

        let server_config = QuicServer::quic_server_config_from_cert(vec![cert_der.clone()], key)?;
        let server = Endpoint::server(server_config, "127.0.0.1:0".parse()?)?;
        let server_addr = server.local_addr()?;
        let accept = tokio::spawn(async move {
            let conn = server.accept().await.expect("incoming").await?;
            conn.closed().await;
            Ok::<_, quinn::ConnectionError>(())
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert_der)?;
        let provider = CryptoProvider {
            kx_groups: groups.to_vec(),
            ..aws_lc_rs::default_provider()
        };
        let mut client_crypto = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_root_certificates(roots)
            .with_no_client_auth();
        client_crypto.alpn_protocols = vec![tls::ALPN.to_vec()];

        let mut client = Endpoint::client("127.0.0.1:0".parse()?)?;
        client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
        )));
        let conn = client.connect(server_addr, "localhost")?.await?;

        let alpn = conn
            .handshake_data()
            .and_then(|d| d.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
            .and_then(|d| d.protocol)
            .unwrap_or_default();

        conn.close(0u32.into(), b"done");
        client.wait_idle().await;
        let _ = accept.await;
        Ok(alpn)
    }

    /// A client that offers *only* X25519MLKEM768 can complete a handshake,
    /// so the server really negotiates the hybrid group over QUIC. Its ~1.2 KB
    /// key share also pushes the ClientHello across multiple Initial packets.
    #[tokio::test]
    async fn quic_handshake_with_hybrid_pq_only_client() {
        let alpn = quic_handshake(&[kx_group::X25519MLKEM768])
            .await
            .expect("PQ-only client must connect");
        assert_eq!(alpn, tls::ALPN);
    }

    /// Clients without ML-KEM support (e.g. macOS < 26) keep working.
    #[tokio::test]
    async fn quic_handshake_with_classical_only_client() {
        let alpn = quic_handshake(&[kx_group::X25519])
            .await
            .expect("classical client must still connect");
        assert_eq!(alpn, tls::ALPN);
    }

    /// Serializes the tests that log in. They share the `auth` span site, and
    /// `tracing` caches whether a site is enabled process-wide: one test
    /// logging in with no subscriber while another records spans makes the
    /// recording test miss its spans.
    static LOGIN_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Log in over a real QUIC connection against `authenticate_client`, the
    /// way a client does: read the hello, send a signed AuthRequest, read the
    /// framed AuthResponse.
    async fn login_over_quic(
        state: Arc<ServerState>,
        id: &aura_identity::IdentitySecret,
        name: &str,
        password: Option<&str>,
    ) -> Result<aura_protocol::AuthResponse> {
        use aura_identity::{auth_transcript, Purpose};

        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
        let cert_der = CertificateDer::from(cert.cert.der().to_vec());
        let key = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
            cert.signing_key.serialize_der(),
        ));
        let server_config = QuicServer::quic_server_config_from_cert(vec![cert_der.clone()], key)?;
        let server = Endpoint::server(server_config, "127.0.0.1:0".parse()?)?;
        let server_addr = server.local_addr()?;
        let accept = tokio::spawn(async move {
            let conn = server.accept().await.expect("incoming").await?;
            let (send, recv) = conn.open_bi().await?;
            let _ = authenticate_client(send, recv, &state, conn.remote_address()).await;
            conn.closed().await;
            Ok::<_, anyhow::Error>(())
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert_der)?;
        let mut client_crypto =
            rustls::ClientConfig::builder_with_provider(Arc::new(tls::crypto_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])?
                .with_root_certificates(roots)
                .with_no_client_auth();
        client_crypto.alpn_protocols = vec![tls::ALPN.to_vec()];
        let mut client = Endpoint::client("127.0.0.1:0".parse()?)?;
        client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
        )));
        let conn = client.connect(server_addr, "localhost")?.await?;
        let (mut send, mut recv) = conn.accept_bi().await?;

        let mut hello = [0u8; 33];
        recv.read_exact(&mut hello).await?;
        assert_eq!(hello[0], MSG_CHALLENGE_RESPONSE);
        let challenge: [u8; 32] = hello[1..].try_into()?;

        let pk = id.public_key();
        let sig = id.sign(Purpose::Auth, &auth_transcript(&challenge, name, &pk));
        let payload = aura_protocol::AuthRequest {
            identity_public_key: pk.as_bytes().to_vec(),
            display_name: name.into(),
            signature: sig.as_bytes().to_vec(),
            server_password: password.map(str::to_owned),
            challenge: challenge.to_vec(),
            ..Default::default()
        }
        .encode_to_vec();
        send.write_all(&[MSG_AUTH_REQUEST]).await?;
        send.write_all(&(payload.len() as u32).to_le_bytes())
            .await?;
        send.write_all(&payload).await?;

        assert_eq!(recv.read_u8().await?, MSG_AUTH_RESPONSE);
        let len = recv.read_u32_le().await? as usize;
        let mut buf = vec![0u8; len];
        recv.read_exact(&mut buf).await?;
        let response = aura_protocol::AuthResponse::decode(buf.as_slice())?;

        conn.close(0u32.into(), b"done");
        client.wait_idle().await;
        let _ = accept.await;
        Ok(response)
    }

    fn test_state(password: Option<&str>) -> Arc<ServerState> {
        let db = Arc::new(crate::db::Database::open_in_memory().unwrap());
        let mut config = crate::config::Config::default();
        config.server.password = password.map(str::to_owned);
        Arc::new(ServerState::new(db, config))
    }

    #[tokio::test]
    async fn login_v2_succeeds_and_returns_the_user_id() {
        let _serial = LOGIN_TESTS.lock().await;
        crate::tls::install_default_provider();
        let id = aura_identity::IdentitySecret::generate();
        let r = login_over_quic(test_state(None), &id, "Alice", None)
            .await
            .unwrap();
        assert!(r.success, "{}", r.error_message);
        assert!(r.user_uuid.starts_with("U_"));
        assert!(!r.session_token.is_empty());
        assert!(r.error_message.is_empty());
    }

    #[tokio::test]
    async fn login_v2_failure_is_a_well_formed_response() {
        let _serial = LOGIN_TESTS.lock().await;
        crate::tls::install_default_provider();
        let id = aura_identity::IdentitySecret::generate();
        let r = login_over_quic(test_state(Some("secret")), &id, "Alice", Some("wrong"))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.user_uuid.is_empty() && r.session_token.is_empty());
        assert!(r.error_message.contains("password"), "{}", r.error_message);
    }

    /// A real login produces one `auth` span with an outcome code, and no
    /// exported attribute carries the user's name, id, token or address.
    #[tokio::test]
    async fn login_spans_carry_no_identifying_details() {
        let _serial = LOGIN_TESTS.lock().await;
        use tracing_subscriber::layer::SubscriberExt as _;
        crate::tls::install_default_provider();
        let (telemetry, exporter) = aura_telemetry::testing::in_memory();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(telemetry.layer()),
        );
        // Span sites are cached process-wide as enabled or not. Another test
        // thread with no subscriber may have reached the `auth` site first;
        // re-evaluate now that this thread has one.
        tracing::callsite::rebuild_interest_cache();

        let id = aura_identity::IdentitySecret::generate();
        let ok = login_over_quic(test_state(None), &id, "Alice", None)
            .await
            .unwrap();
        assert!(ok.success);
        let refused = login_over_quic(test_state(Some("secret")), &id, "Alice", Some("wrong"))
            .await
            .unwrap();
        assert!(!refused.success);

        let spans = exporter.get_finished_spans().unwrap();
        let outcomes: Vec<String> = spans
            .iter()
            .filter(|s| s.name == "auth")
            .map(|s| {
                s.attributes
                    .iter()
                    .find(|kv| kv.key.as_str() == "outcome")
                    .map(|kv| kv.value.as_str().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(outcomes, ["ok", "invalid_password"]);

        let exported: String = spans
            .iter()
            .flat_map(|s| {
                s.attributes
                    .iter()
                    .map(|kv| format!("{}={} ", kv.key, kv.value))
            })
            .collect();
        // Name the field on failure, never print its value (or the export).
        for (field, value) in [
            ("display name", "Alice"),
            ("user id", ok.user_uuid.as_str()),
            ("session token", ok.session_token.as_str()),
            ("client address", "127.0.0.1"),
        ] {
            assert!(
                !exported.contains(value),
                "the {field} leaked into exported span attributes"
            );
        }
        assert!(
            spans.iter().all(|s| s.events.is_empty()),
            "log events exported"
        );
        drop(_guard);
        drop(telemetry);
    }

    /// `main` starts and stops the real OTLP exporter inside the tokio
    /// runtime. Its HTTP client is reqwest's blocking client, which panics if
    /// built or dropped on a runtime thread; this catches a regression there.
    #[tokio::test(flavor = "multi_thread")]
    async fn real_exporter_starts_and_stops_inside_the_runtime() {
        let config = aura_telemetry::Config::with_endpoint(
            "http://127.0.0.1:9",
            Default::default(),
            "aura-server-test",
            "0",
        )
        .unwrap();
        let telemetry = aura_telemetry::Telemetry::start(config).unwrap();
        drop(telemetry);
    }

    /// Logins feed `aura_logins_total` by outcome. Under the login lock, so
    /// no other test's logins land in between.
    #[tokio::test]
    async fn logins_are_counted_by_outcome() {
        let _serial = LOGIN_TESTS.lock().await;
        crate::tls::install_default_provider();
        let m = &crate::metrics::METRICS;
        let (ok0, pw0) = (m.logins_with("ok"), m.logins_with("invalid_password"));

        let id = aura_identity::IdentitySecret::generate();
        login_over_quic(test_state(None), &id, "Alice", None)
            .await
            .unwrap();
        login_over_quic(test_state(Some("secret")), &id, "Alice", Some("wrong"))
            .await
            .unwrap();

        assert_eq!(m.logins_with("ok"), ok0 + 1);
        assert_eq!(m.logins_with("invalid_password"), pw0 + 1);
    }

    // --- End to end: the real connection handler over QUIC ---

    /// A server running `handle_connection` for every client, on a loopback
    /// port with a throwaway certificate.
    struct TestServer {
        addr: SocketAddr,
        cert: CertificateDer<'static>,
        accept: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.accept.abort();
        }
    }

    fn start_server(state: Arc<ServerState>) -> Result<TestServer> {
        crate::tls::install_default_provider();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
        let cert_der = CertificateDer::from(cert.cert.der().to_vec());
        let key = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
            cert.signing_key.serialize_der(),
        ));
        let server_config = QuicServer::quic_server_config_from_cert(vec![cert_der.clone()], key)?;
        let endpoint = Endpoint::server(server_config, "127.0.0.1:0".parse()?)?;
        let addr = endpoint.local_addr()?;
        let accept = tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Ok(conn) = incoming.await {
                        let _ = handle_connection(conn, state).await;
                    }
                });
            }
        });
        Ok(TestServer {
            addr,
            cert: cert_der,
            accept,
        })
    }

    /// One client's control stream, after its login attempt.
    struct TestClient {
        _endpoint: Endpoint,
        conn: Connection,
        send: SendStream,
        recv: RecvStream,
        auth: aura_protocol::AuthResponse,
    }

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    impl TestClient {
        /// Connect and log in as `name`, the way a client does.
        async fn login(
            server: &TestServer,
            id: &aura_identity::IdentitySecret,
            name: &str,
        ) -> Result<Self> {
            use aura_identity::{auth_transcript, Purpose};

            let mut roots = rustls::RootCertStore::empty();
            roots.add(server.cert.clone())?;
            let mut client_crypto =
                rustls::ClientConfig::builder_with_provider(Arc::new(tls::crypto_provider()))
                    .with_protocol_versions(&[&rustls::version::TLS13])?
                    .with_root_certificates(roots)
                    .with_no_client_auth();
            client_crypto.alpn_protocols = vec![tls::ALPN.to_vec()];
            let mut endpoint = Endpoint::client("127.0.0.1:0".parse()?)?;
            endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
                quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
            )));
            let conn = endpoint.connect(server.addr, "localhost")?.await?;
            let (send, mut recv) = conn.accept_bi().await?;

            let mut hello = [0u8; 33];
            recv.read_exact(&mut hello).await?;
            let challenge: [u8; 32] = hello[1..].try_into()?;
            let pk = id.public_key();
            let sig = id.sign(Purpose::Auth, &auth_transcript(&challenge, name, &pk));
            let request = aura_protocol::AuthRequest {
                identity_public_key: pk.as_bytes().to_vec(),
                display_name: name.into(),
                signature: sig.as_bytes().to_vec(),
                server_password: None,
                challenge: challenge.to_vec(),
                ..Default::default()
            };
            let mut client = Self {
                _endpoint: endpoint,
                conn,
                send,
                recv,
                auth: Default::default(),
            };
            client.send_proto(MSG_AUTH_REQUEST, request).await?;
            client.auth = client.expect(MSG_AUTH_RESPONSE).await?;
            Ok(client)
        }

        async fn send_frame(&mut self, msg_type: u8, payload: &[u8]) -> Result<()> {
            self.send.write_all(&[msg_type]).await?;
            self.send
                .write_all(&(payload.len() as u32).to_le_bytes())
                .await?;
            self.send.write_all(payload).await?;
            Ok(())
        }

        async fn send_proto(&mut self, msg_type: u8, msg: impl Message) -> Result<()> {
            self.send_frame(msg_type, &msg.encode_to_vec()).await
        }

        /// The next frame from the server, whatever its type.
        async fn read_frame(&mut self) -> Result<(u8, Vec<u8>)> {
            timeout(TEST_TIMEOUT, async {
                let msg_type = self.recv.read_u8().await?;
                let len = self.recv.read_u32_le().await? as usize;
                let mut payload = vec![0u8; len];
                self.recv.read_exact(&mut payload).await?;
                Ok((msg_type, payload))
            })
            .await
            .map_err(|_| anyhow!("no frame from the server within {TEST_TIMEOUT:?}"))?
        }

        /// The next frame of `msg_type`, skipping anything else (snapshots,
        /// presence) the server sends meanwhile.
        async fn expect<M: Message + Default>(&mut self, msg_type: u8) -> Result<M> {
            loop {
                let (t, payload) = self.read_frame().await?;
                if t == msg_type {
                    return Ok(M::decode(payload.as_slice())?);
                }
            }
        }

        async fn ping(&mut self, nonce: u64) -> Result<()> {
            self.send_frame(MSG_PING, &nonce.to_le_bytes()).await
        }

        /// Ping and wait for the pong: once it arrives, the server has
        /// handled everything sent before it.
        async fn sync(&mut self, nonce: u64) -> Result<()> {
            self.ping(nonce).await?;
            loop {
                let (t, payload) = self.read_frame().await?;
                if t == MSG_PONG && payload == nonce.to_le_bytes() {
                    return Ok(());
                }
            }
        }

        async fn admin_request(
            &mut self,
            msg_type: u8,
            msg: impl Message,
        ) -> Result<aura_protocol::AdminResponse> {
            self.send_proto(msg_type, msg).await?;
            self.expect(msg_type).await
        }

        /// How the server ended the connection.
        async fn closed(&self) -> Result<quinn::ConnectionError> {
            timeout(TEST_TIMEOUT, self.conn.closed())
                .await
                .map_err(|_| anyhow!("the server kept the connection open"))
        }

        /// Hang up. Skips waiting out QUIC's draining period, which the
        /// tests have no use for.
        fn close(self) {
            self.conn.close(0u32.into(), b"done");
        }
    }

    /// A channel in the DB and in memory, as `main` creates the lobby.
    fn add_channel(state: &ServerState, name: &str, lobby: bool) -> String {
        let channel_type = i32::from(lobby);
        let id = state
            .db
            .upsert_channel(None, name, "", 0, &[], 0, channel_type)
            .unwrap();
        state.channel_metadata.insert(
            id.clone(),
            crate::state::ChannelMetadata {
                id: id.clone(),
                name: name.into(),
                comment: String::new(),
                icon_type: 0,
                icon_data: vec![],
                position: 0,
                channel_type,
            },
        );
        state.create_channel(id.clone());
        id
    }

    /// A server with a lobby and the bootstrap admin "Root" for `root_id`.
    fn moderation_state(root_id: &aura_identity::IdentitySecret) -> (Arc<ServerState>, String) {
        let state = test_state(None);
        state
            .db
            .create_bootstrap_admin(&root_id.public_key().ed25519_bytes(), "Root")
            .unwrap();
        let lobby = add_channel(&state, "Lobby", true);
        (state, lobby)
    }

    #[tokio::test]
    async fn control_stream_ping_echoes_the_nonce() {
        let _serial = LOGIN_TESTS.lock().await;
        let server = start_server(test_state(None)).unwrap();
        let id = aura_identity::IdentitySecret::generate();
        let mut alice = TestClient::login(&server, &id, "Alice").await.unwrap();
        assert!(alice.auth.success);

        let nonce = 0x0123_4567_89ab_cdefu64;
        alice.ping(nonce).await.unwrap();
        let (msg_type, payload) = loop {
            let frame = alice.read_frame().await.unwrap();
            if frame.0 != 0x13 {
                break frame;
            }
        };
        assert_eq!(msg_type, MSG_PONG);
        assert_eq!(payload, nonce.to_le_bytes());
        alice.close();
    }

    /// Past the burst, pings go unanswered; the connection stays up.
    #[tokio::test]
    async fn control_stream_pings_are_rate_limited() {
        let _serial = LOGIN_TESTS.lock().await;
        let server = start_server(test_state(None)).unwrap();
        let id = aura_identity::IdentitySecret::generate();
        let mut alice = TestClient::login(&server, &id, "Alice").await.unwrap();

        for nonce in 0..20u64 {
            alice.ping(nonce).await.unwrap();
        }
        // A request that isn't a ping marks the end of the flood.
        alice
            .send_proto(MSG_LIST_USERS, aura_protocol::ListUsersRequest::default())
            .await
            .unwrap();
        let mut pongs = 0;
        loop {
            match alice.read_frame().await.unwrap() {
                (MSG_PONG, _) => pongs += 1,
                (MSG_LIST_USERS, _) => break,
                _ => {}
            }
        }
        assert!((1..20).contains(&pongs), "{pongs} of 20 pings answered");
        alice.close();
    }

    #[tokio::test]
    async fn malformed_ping_closes_the_connection() {
        let _serial = LOGIN_TESTS.lock().await;
        let server = start_server(test_state(None)).unwrap();
        let id = aura_identity::IdentitySecret::generate();
        let mut alice = TestClient::login(&server, &id, "Alice").await.unwrap();

        alice.send_frame(MSG_PING, &[1, 2, 3, 4]).await.unwrap();
        alice.closed().await.unwrap();
        alice.close();
    }

    /// An unknown message type is skipped whole. Its payload starts with
    /// bytes that are valid message types, which a server reading on from
    /// the length prefix would act on.
    #[tokio::test]
    async fn unknown_message_types_are_skipped_without_losing_sync() {
        let _serial = LOGIN_TESTS.lock().await;
        let server = start_server(test_state(None)).unwrap();
        let id = aura_identity::IdentitySecret::generate();
        let mut alice = TestClient::login(&server, &id, "Alice").await.unwrap();

        let payload = [
            MSG_TEXT_PACKET,
            0xff,
            0xff,
            0xff,
            0xff,
            MSG_JOIN_CHANNEL,
            0,
            0,
        ];
        alice.send_frame(0x7e, &payload).await.unwrap();
        alice.sync(42).await.unwrap();
        alice.close();
    }

    /// A client rejoining a channel deleted while it was away is pointed at
    /// the lobby, and its MLS join for the dead channel doesn't bring the
    /// channel's group back.
    #[tokio::test]
    async fn joining_an_unknown_channel_points_at_the_lobby() {
        let _serial = LOGIN_TESTS.lock().await;
        let root_id = aura_identity::IdentitySecret::generate();
        let (state, lobby) = moderation_state(&root_id);
        let server = start_server(Arc::clone(&state)).unwrap();
        let id = aura_identity::IdentitySecret::generate();
        let mut alice = TestClient::login(&server, &id, "Alice").await.unwrap();

        let gone = "C_deleted_while_away".to_string();
        alice
            .send_proto(
                MSG_JOIN_CHANNEL,
                aura_protocol::JoinChannelRequest {
                    channel_id: gone.clone(),
                },
            )
            .await
            .unwrap();
        let notice: aura_protocol::ChannelDeleted =
            alice.expect(MSG_CHANNEL_DELETED).await.unwrap();
        assert_eq!(notice.channel_id, gone);
        assert_eq!(notice.fallback_channel_id, lobby);
        assert_eq!(
            notice.reason(),
            aura_protocol::ChannelRemovalReason::NotFound
        );

        for is_voice in [true, false] {
            let group_type = if is_voice {
                aura_protocol::MlsGroupType::Voice
            } else {
                aura_protocol::MlsGroupType::Text
            };
            let envelope = aura_protocol::MlsEnvelope {
                channel_id: gone.clone(),
                group_type: group_type as i32,
                content: Some(aura_protocol::mls_envelope::Content::KeyPackage(vec![
                    1, 2, 3,
                ])),
                ..Default::default()
            };
            alice.send_proto(MSG_MLS_JOIN, envelope).await.unwrap();
        }
        alice.sync(1).await.unwrap();
        assert!(!state
            .voice_groups
            .contains_key(&aura_protocol::make_mls_group_id(&gone, true)));
        assert!(!state
            .text_groups
            .contains_key(&aura_protocol::make_mls_group_id(&gone, false)));

        // Joining a channel that exists still works.
        alice
            .send_proto(
                MSG_JOIN_CHANNEL,
                aura_protocol::JoinChannelRequest {
                    channel_id: lobby.clone(),
                },
            )
            .await
            .unwrap();
        alice.sync(2).await.unwrap();
        let voice = state
            .voice_groups
            .get(&aura_protocol::make_mls_group_id(&lobby, true))
            .unwrap()
            .clone();
        assert!(voice.read().await.members.contains(&alice.auth.session_id));
        alice.close();
    }

    #[tokio::test]
    async fn non_admins_cannot_moderate() {
        let _serial = LOGIN_TESTS.lock().await;
        let root_id = aura_identity::IdentitySecret::generate();
        let (state, _) = moderation_state(&root_id);
        let server = start_server(Arc::clone(&state)).unwrap();
        let mut root = TestClient::login(&server, &root_id, "Root").await.unwrap();
        let bob_id = aura_identity::IdentitySecret::generate();
        let mut bob = TestClient::login(&server, &bob_id, "Bob").await.unwrap();
        let root_uuid = root.auth.user_uuid.clone();

        let list: aura_protocol::ListUsersResponse = {
            bob.send_proto(MSG_LIST_USERS, aura_protocol::ListUsersRequest::default())
                .await
                .unwrap();
            bob.expect(MSG_LIST_USERS).await.unwrap()
        };
        assert!(!list.success && list.users.is_empty());
        assert_eq!(list.error_message, "Permission denied");

        let requests: [(u8, Vec<u8>); 4] = [
            (
                MSG_VERIFY_USER,
                aura_protocol::VerifyUserRequest {
                    target_user_uuid: root_uuid.clone(),
                    ..Default::default()
                }
                .encode_to_vec(),
            ),
            (
                MSG_BAN_USER,
                aura_protocol::BanUserRequest {
                    target_user_uuid: root_uuid.clone(),
                    banned: true,
                    ..Default::default()
                }
                .encode_to_vec(),
            ),
            (
                MSG_GRANT_ADMIN,
                aura_protocol::GrantAdminRequest {
                    target_user_uuid: bob.auth.user_uuid.clone(),
                    grant_admin: true,
                    ..Default::default()
                }
                .encode_to_vec(),
            ),
            (
                MSG_KICK_USER,
                aura_protocol::KickUserRequest {
                    target_user_uuid: root_uuid.clone(),
                    reason: String::new(),
                }
                .encode_to_vec(),
            ),
        ];
        for (msg_type, payload) in requests {
            bob.send_frame(msg_type, &payload).await.unwrap();
            let resp: aura_protocol::AdminResponse = bob.expect(msg_type).await.unwrap();
            assert!(!resp.success, "0x{msg_type:02x} succeeded");
            assert_eq!(resp.error_message, "Permission denied");
        }
        assert!(!state.db.is_admin(&bob.auth.user_uuid).unwrap());
        assert!(
            !state
                .db
                .find_user_by_uuid(&root_uuid)
                .unwrap()
                .unwrap()
                .banned
        );

        // Root is still connected and an admin.
        root.sync(7).await.unwrap();
        bob.close();
        root.close();
    }

    #[tokio::test]
    async fn admin_lists_verifies_and_grants() {
        let _serial = LOGIN_TESTS.lock().await;
        let root_id = aura_identity::IdentitySecret::generate();
        let (state, _) = moderation_state(&root_id);
        let server = start_server(Arc::clone(&state)).unwrap();
        let mut root = TestClient::login(&server, &root_id, "Root").await.unwrap();
        assert!(root.auth.is_admin);
        let bob_id = aura_identity::IdentitySecret::generate();
        let bob = TestClient::login(&server, &bob_id, "Bob").await.unwrap();
        let bob_uuid = bob.auth.user_uuid.clone();

        root.send_proto(MSG_LIST_USERS, aura_protocol::ListUsersRequest::default())
            .await
            .unwrap();
        let list: aura_protocol::ListUsersResponse = root.expect(MSG_LIST_USERS).await.unwrap();
        assert!(list.success, "{}", list.error_message);
        assert_eq!(list.total_count, 2);
        let names: Vec<(&str, bool)> = list
            .users
            .iter()
            .map(|u| (u.display_name.as_str(), u.is_admin))
            .collect();
        assert_eq!(names, [("Root", true), ("Bob", false)]);

        let resp = root
            .admin_request(
                MSG_VERIFY_USER,
                aura_protocol::VerifyUserRequest {
                    target_user_uuid: bob_uuid.clone(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(resp.success, "{}", resp.error_message);
        assert!(
            state
                .db
                .find_user_by_uuid(&bob_uuid)
                .unwrap()
                .unwrap()
                .verified
        );

        let resp = root
            .admin_request(
                MSG_GRANT_ADMIN,
                aura_protocol::GrantAdminRequest {
                    target_user_uuid: bob_uuid.clone(),
                    manage_channels: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(resp.success, "{}", resp.error_message);
        let perms = state.db.get_admin_permissions(&bob_uuid).unwrap().unwrap();
        assert!(perms.manage_channels && !perms.ban_users && !perms.grant_admin);

        // Root can't change its own permissions.
        let resp = root
            .admin_request(
                MSG_GRANT_ADMIN,
                aura_protocol::GrantAdminRequest {
                    target_user_uuid: root.auth.user_uuid.clone(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!resp.success);
        assert!(state.db.is_admin(&root.auth.user_uuid).unwrap());

        bob.close();
        root.close();
    }

    #[tokio::test]
    async fn kick_moves_the_target_to_the_lobby() {
        let _serial = LOGIN_TESTS.lock().await;
        let root_id = aura_identity::IdentitySecret::generate();
        let (state, lobby) = moderation_state(&root_id);
        let room = add_channel(&state, "Room", false);
        let server = start_server(Arc::clone(&state)).unwrap();
        let mut root = TestClient::login(&server, &root_id, "Root").await.unwrap();
        let bob_id = aura_identity::IdentitySecret::generate();
        let mut bob = TestClient::login(&server, &bob_id, "Bob").await.unwrap();

        bob.send_proto(
            MSG_JOIN_CHANNEL,
            aura_protocol::JoinChannelRequest {
                channel_id: room.clone(),
            },
        )
        .await
        .unwrap();
        bob.sync(1).await.unwrap();

        let resp = root
            .admin_request(
                MSG_KICK_USER,
                aura_protocol::KickUserRequest {
                    target_user_uuid: bob.auth.user_uuid.clone(),
                    reason: "cool off".into(),
                },
            )
            .await
            .unwrap();
        assert!(resp.success, "{}", resp.error_message);

        let notice: aura_protocol::ChannelDeleted = bob.expect(MSG_CHANNEL_DELETED).await.unwrap();
        assert_eq!(notice.channel_id, room);
        assert_eq!(notice.fallback_channel_id, lobby);
        assert_eq!(notice.reason(), aura_protocol::ChannelRemovalReason::Kicked);
        assert_eq!(notice.detail, "cool off");

        // Root sees Bob leave the room, and the server has him in no channel.
        let left: aura_protocol::UserLeft = root.expect(0x12).await.unwrap();
        assert_eq!(
            (left.channel_id.as_str(), left.session_id),
            (room.as_str(), bob.auth.session_id)
        );
        let session = state.sessions.get(&bob.auth.session_id).unwrap();
        assert!(session.voice_group_id.is_none() && session.text_group_id.is_none());
        drop(session);

        // Kicking him again finds nothing to kick.
        let resp = root
            .admin_request(
                MSG_KICK_USER,
                aura_protocol::KickUserRequest {
                    target_user_uuid: bob.auth.user_uuid.clone(),
                    reason: String::new(),
                },
            )
            .await
            .unwrap();
        assert!(!resp.success);

        bob.close();
        root.close();
    }

    #[tokio::test]
    async fn ban_drops_the_target_and_refuses_its_logins() {
        let _serial = LOGIN_TESTS.lock().await;
        let root_id = aura_identity::IdentitySecret::generate();
        let (state, _) = moderation_state(&root_id);
        let server = start_server(Arc::clone(&state)).unwrap();
        let mut root = TestClient::login(&server, &root_id, "Root").await.unwrap();
        let bob_id = aura_identity::IdentitySecret::generate();
        let bob = TestClient::login(&server, &bob_id, "Bob").await.unwrap();
        let bob_uuid = bob.auth.user_uuid.clone();

        // Root can't ban itself.
        let resp = root
            .admin_request(
                MSG_BAN_USER,
                aura_protocol::BanUserRequest {
                    target_user_uuid: root.auth.user_uuid.clone(),
                    banned: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!resp.success);

        let resp = root
            .admin_request(
                MSG_BAN_USER,
                aura_protocol::BanUserRequest {
                    target_user_uuid: bob_uuid.clone(),
                    banned: true,
                    reason: "spam".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(resp.success, "{}", resp.error_message);

        match bob.closed().await.unwrap() {
            quinn::ConnectionError::ApplicationClosed(close) => {
                assert_eq!(close.error_code, CLOSE_BANNED.into());
                assert_eq!(&close.reason[..], b"banned");
            }
            other => panic!("expected an application close, got {other}"),
        }
        bob.close();

        let again = TestClient::login(&server, &bob_id, "Bob").await.unwrap();
        assert!(!again.auth.success);
        assert!(
            again.auth.error_message.contains("banned"),
            "{}",
            again.auth.error_message
        );
        again.close();

        // Unbanned, Bob is back.
        let resp = root
            .admin_request(
                MSG_BAN_USER,
                aura_protocol::BanUserRequest {
                    target_user_uuid: bob_uuid,
                    banned: false,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(resp.success, "{}", resp.error_message);
        let back = TestClient::login(&server, &bob_id, "Bob").await.unwrap();
        assert!(back.auth.success, "{}", back.auth.error_message);
        back.close();
        root.close();
    }

    /// Channel management needs `manage_channels`, not merely being an
    /// admin. Non-admins still get the "Admin required" the macOS client
    /// looks for.
    #[tokio::test]
    async fn channel_management_needs_manage_channels() {
        let _serial = LOGIN_TESTS.lock().await;
        let root_id = aura_identity::IdentitySecret::generate();
        let (state, _) = moderation_state(&root_id);
        let server = start_server(Arc::clone(&state)).unwrap();
        let bob_id = aura_identity::IdentitySecret::generate();
        let mut bob = TestClient::login(&server, &bob_id, "Bob").await.unwrap();
        let create = aura_protocol::CreateChannelRequest {
            name: "Bob's".into(),
            ..Default::default()
        };

        bob.send_proto(MSG_CREATE_CHANNEL, create.clone())
            .await
            .unwrap();
        let resp: aura_protocol::CreateChannelResponse =
            bob.expect(MSG_CREATE_CHANNEL).await.unwrap();
        assert_eq!(
            (resp.success, resp.error_message.as_str()),
            (false, "Admin required")
        );

        let moderator = AdminPermissions {
            ban_users: true,
            ..Default::default()
        };
        state
            .db
            .grant_admin(&bob.auth.user_uuid, &moderator, None)
            .unwrap();
        bob.send_proto(MSG_CREATE_CHANNEL, create.clone())
            .await
            .unwrap();
        let resp: aura_protocol::CreateChannelResponse =
            bob.expect(MSG_CREATE_CHANNEL).await.unwrap();
        assert!(!resp.success);
        assert_ne!(resp.error_message, "Admin required");

        let channel_admin = AdminPermissions {
            manage_channels: true,
            ..Default::default()
        };
        state
            .db
            .grant_admin(&bob.auth.user_uuid, &channel_admin, None)
            .unwrap();
        bob.send_proto(MSG_CREATE_CHANNEL, create).await.unwrap();
        let resp: aura_protocol::CreateChannelResponse =
            bob.expect(MSG_CREATE_CHANNEL).await.unwrap();
        assert!(resp.success, "{}", resp.error_message);
        assert!(state.channel_exists(&resp.channel_id));
        bob.close();
    }

    #[test]
    fn test_self_signed_cert_persistence() {
        let dir = std::env::temp_dir().join(format!("aura-cert-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = crate::config::Config::default();
        config.database.path = dir.join("aura.db");
        let db = Arc::new(crate::db::Database::open_in_memory().unwrap());
        let state = ServerState::new(db, config);

        // First call generates and persists aura-cert.pem / aura-key.pem
        QuicServer::generate_or_load_self_signed_config(&state).unwrap();
        let cert_path = dir.join("aura-cert.pem");
        let key_path = dir.join("aura-key.pem");
        assert!(cert_path.exists());
        assert!(key_path.exists());
        let first_cert = std::fs::read(&cert_path).unwrap();

        // Second call loads the persisted cert without overwriting
        QuicServer::generate_or_load_self_signed_config(&state).unwrap();
        let second_cert = std::fs::read(&cert_path).unwrap();
        assert_eq!(first_cert, second_cert);

        // Default relative path ("aura.db") falls back to "." rather than skipping persistence
        assert_eq!(
            QuicServer::self_signed_cert_dir(Path::new("aura.db")),
            Some(Path::new("."))
        );
        assert_eq!(
            QuicServer::self_signed_cert_dir(Path::new(":memory:")),
            None
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
