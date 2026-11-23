//! QUIC connection handler module.
//!
//! Handles incoming QUIC connections, authentication, and stream routing.

use crate::auth::AuthService;
use crate::rate_limit::HandshakeRateLimiter;
use crate::state::{ServerState, ServiceMessage};
use crate::tls;
use anyhow::{anyhow, Result};
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
use tracing::{debug, error, info, warn, Instrument as _};

// Protocol message types
// const MSG_CHALLENGE_REQUEST: u8 = 0x01;
const MSG_CHALLENGE_RESPONSE: u8 = 0x02;
const MSG_AUTH_REQUEST: u8 = 0x03;
const MSG_AUTH_RESPONSE: u8 = 0x04;
const MSG_JOIN_CHANNEL: u8 = 0x10;
// const MSG_AUDIO_STREAM: u8 = 0x20;
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
// Hard ceiling on how long a single control frame body may take to arrive.
// Prevents drip-feed (Slowloris-style) attacks that stay under the byte cap.
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(10);

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
            info!("No TLS cert or ACME domain configured. Generating self-signed fallback...");
            Self::start_health_check_listener(&state, tcp_ip);
            Self::generate_self_signed_config()?
        };

        info!("Creating QUIC endpoint on {}...", bind_addr);
        let endpoint = Endpoint::server(server_config, bind_addr)
            .map_err(|e| anyhow!("Failed to bind QUIC endpoint to {}: {}", bind_addr, e))?;

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

    /// Generate self-signed TLS certificate for QUIC.
    fn generate_self_signed_config() -> Result<ServerConfig> {
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
        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into()?));
        transport.keep_alive_interval(Some(std::time::Duration::from_secs(5)));

        // Enable QUIC datagrams for unreliable audio packets
        transport.datagram_receive_buffer_size(Some(65536));
        transport.datagram_send_buffer_size(65536);
        server_config.transport_config(Arc::new(transport));

        Ok(server_config)
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
        MSG_MLS_JOIN => "mls_join",
        MSG_MLS_COMMIT_WELCOME => "mls_commit_welcome",
        MSG_MLS_RATCHET_COMMIT => "mls_ratchet_commit",
        _ => "other",
    }
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
                                0x01 if data.len() > 1 => {
                                    crate::metrics::METRICS.audio_datagram(data.len() - 1);
                                    ctx.state.route_audio_packet(bytes::Bytes::copy_from_slice(&data[1..])).await;
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

            // 2. Control stream messages
            msg_type = ctx.recv.read_u8() => {
                match msg_type {
                    Ok(type_byte) => {
                        last_activity = std::time::Instant::now();
                        if type_byte == 0x00 {
                            debug!("[{}] Keepalive ping received", remote);
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
                if let Err(e) = ctx.handle_service_message(msg).await {
                    warn!("[{}] Error handling service message: {}", remote, e);
                    break;
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
    if let Some(channel_id) = ctx.current_channel_id.clone() {
        ctx.state
            .remove_from_voice_group(channel_id.clone(), session_id)
            .await;
        ctx.state
            .remove_from_text_group(channel_id.clone(), session_id)
            .await;
        ctx.state.broadcast_user_left(channel_id, session_id).await;
    }

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
    info!("[Auth] Display name: {}", request.display_name);

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
            };
            (response, Ok((session_id, result.user_uuid, rx)))
        }
        Err(refused) => {
            let response = aura_protocol::AuthResponse {
                success: false,
                error_message: refused.message.clone(),
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

                // Leave previous channel if any
                if let Some(old_id) = self.current_channel_id.clone() {
                    self.state
                        .remove_from_voice_group(old_id.clone(), self.session_id)
                        .await;
                    self.state
                        .remove_from_text_group(old_id.clone(), self.session_id)
                        .await;
                    self.state
                        .broadcast_user_left(old_id, self.session_id)
                        .await;
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
            0x20 => {
                // MSG_AUDIO (legacy reliable path)
                // Format: [20] [Length u32] [Payload...]
                let mut len_buf = [0u8; 4];
                self.recv.read_exact(&mut len_buf).await?;
                let packet_len = u32::from_le_bytes(len_buf) as usize;

                info!(
                    "[{}] Received audio packet: {} bytes",
                    self.remote, packet_len
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

                self.state
                    .route_audio_packet(bytes::Bytes::from(packet_buf))
                    .await;
                info!(
                    "[{}] Routed audio packet from session {}",
                    self.remote, self.session_id
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

                // Only admins can create channels
                if !self.state.db.is_admin(&self.user_uuid)? {
                    let resp = aura_protocol::CreateChannelResponse {
                        success: false,
                        channel_id: String::new(),
                        error_message: "Admin required".into(),
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

                // Only admins can update channels
                if !self.state.db.is_admin(&self.user_uuid)? {
                    let resp = aura_protocol::MetadataUpdateResponse {
                        success: false,
                        error_message: "Admin required".into(),
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

                // Only admins can delete channels
                if !self.state.db.is_admin(&self.user_uuid)? {
                    let resp = aura_protocol::AdminResponse {
                        success: false,
                        error_message: "Admin required".into(),
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

                // Only admins or the user themselves can delete their profile
                if self.user_uuid != req.user_uuid && !self.state.db.is_admin(&self.user_uuid)? {
                    let resp = aura_protocol::AdminResponse {
                        success: false,
                        error_message: "Permission denied".into(),
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

                self.state
                    .broadcast_user_status(req.session_id, req.is_muted, req.is_deafened)
                    .await;
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
                // Unknown message
                warn!("[{}] Unknown message type: 0x{:02x}", self.remote, msg_type);
            }
        }
        Ok(true)
    }

    async fn handle_service_message(&mut self, msg: ServiceMessage) -> Result<()> {
        match msg {
            ServiceMessage::RelayAudio(packet) => {
                // Use QUIC datagrams for unreliable, low-latency audio
                let mut dgram_data = vec![0x01u8]; // Audio type
                dgram_data.extend_from_slice(&packet);

                if self
                    .conn
                    .send_datagram(bytes::Bytes::from(dgram_data))
                    .is_err()
                {
                    // Fallback to reliable stream if datagrams fail
                    self.send.write_u8(0x20).await?; // MSG_AUDIO
                    self.send.write_u32_le(packet.len() as u32).await?;
                    self.send.write_all(&packet).await?;
                    self.send.flush().await?;
                }
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
            } => {
                let update = aura_protocol::UserStatusUpdate {
                    session_id,
                    is_muted,
                    is_deafened,
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
        }
        Ok(())
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
}
