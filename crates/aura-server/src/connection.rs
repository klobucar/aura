//! QUIC connection handler module.
//!
//! Handles incoming QUIC connections, authentication, and stream routing.

use crate::auth::AuthService;
use crate::rate_limit::HandshakeRateLimiter;
use crate::state::{ServerState, ServiceMessage};
use anyhow::{anyhow, Result};
use bytes::{BufMut, BytesMut};
use prost::Message;
use quinn::{Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_acme::{caches::DirCache, AcmeConfig, UseChallenge};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

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
const MSG_DELETE_CHANNEL: u8 = 0x43;
const MSG_DELETE_USER: u8 = 0x44;
const MSG_UPDATE_STATUS: u8 = 0x45;

// MLS Protocol messages
const MSG_MLS_JOIN: u8 = 0x50; // Client sends key package on channel join
const MSG_MLS_COMMIT_WELCOME: u8 = 0x51; // Client sends commit + welcome after adding member

// Security limits
const MAX_AUDIO_PACKET_SIZE: usize = 65536; // 64KB for audio (far more than enough for Opus)
const MAX_CONTROL_PACKET_SIZE: usize = 256 * 1024; // 256KB for signaling/metadata/MLS welcomes
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
        // We're bypassing ACME for now due to JSON parsing issues with Let's Encrypt Staging ("missing field token").
        // Falling back to self-signed or manual TLS as requested.

        // Still start the health-check listener so Fly.io doesn't think we're dead
        Self::start_health_check_listener(&state);

        let server_config = if let (Some(cert_path), Some(key_path)) = (
            &state.config.server.cert_path,
            &state.config.server.key_path,
        ) {
            info!("Loading custom TLS certificates from {:?}...", cert_path);
            Self::configure_manual_tls(cert_path, key_path)?
        } else {
            info!("ACME disabled or bypassed. Generating self-signed fallback...");
            Self::generate_self_signed_config()?
        };

        info!("Creating QUIC endpoint on {}...", bind_addr);
        let endpoint = Endpoint::server(server_config, bind_addr)
            .map_err(|e| anyhow!("Failed to bind QUIC endpoint to {}: {}", bind_addr, e))?;

        let local_addr = endpoint
            .local_addr()
            .map_err(|e| anyhow!("Failed to get local address: {}", e))?;

        info!("✓ QUIC server bound to UDP socket: {}", local_addr);
        info!("✓ ALPN protocol: 'aura-dave'");

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

    #[allow(dead_code)]
    /// Configure ACME (Let's Encrypt) for automated certificate management.
    fn configure_acme(domain: &str, state: &Arc<ServerState>) -> Result<ServerConfig> {
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

        // Define ALPN for Aura
        let alpn = vec![b"aura-dave".to_vec()];

        // Build ACME config — must set challenge_type to Http01
        let mut acme_builder = AcmeConfig::new([domain])
            .contact([format!("mailto:{}", contact)])
            .cache_with_boxed_err(DirCache::new(cache_path))
            .challenge_type(UseChallenge::Http01);

        if let Some(url) = &state.config.server.acme_directory_url {
            info!("[ACME] Using custom directory URL: {}", url);
            acme_builder = acme_builder.directory(url);
        }

        let mut acme_state = acme_builder.state();
        let resolver = acme_state.resolver();

        // Get the Tower service that automatically responds to HTTP-01 challenges.
        // This handles token lookup and key authorization computation internally.
        let challenge_service = acme_state.http01_challenge_tower_service();

        let bind_addr_str = &state.config.server.bind_address;
        let bind_addr: SocketAddr = bind_addr_str
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:8443".parse().unwrap());
        let acme_port = state.config.server.acme_bind_port.unwrap_or(8080);
        let tcp_bind_addr = SocketAddr::new(bind_addr.ip(), acme_port);

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

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(resolver);

        server_crypto.alpn_protocols = alpn;

        let quinn_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .map_err(|e| anyhow!("Failed to convert rustls config for ACME: {}", e))?;

        Self::apply_transport_config(ServerConfig::with_crypto(Arc::new(quinn_crypto)))
    }

    /// Minimal TCP listener for Fly.io health checks when ACME is disabled.
    fn start_health_check_listener(state: &Arc<ServerState>) {
        let bind_addr_str = &state.config.server.bind_address;
        let bind_addr: SocketAddr = bind_addr_str
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:8443".parse().unwrap());
        let acme_port = state.config.server.acme_bind_port.unwrap_or(443);
        let tcp_bind_addr = SocketAddr::new(bind_addr.ip(), acme_port);

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
        let cert_file = std::fs::File::open(cert_path)
            .map_err(|e| anyhow!("Failed to open certificate file: {}", e))?;
        let mut cert_reader = std::io::BufReader::new(cert_file);
        let cert_chain: Vec<CertificateDer> =
            rustls_pemfile::certs(&mut cert_reader).collect::<std::io::Result<Vec<_>>>()?;

        let key_file = std::fs::File::open(key_path)
            .map_err(|e| anyhow!("Failed to open private key file: {}", e))?;
        let mut key_reader = std::io::BufReader::new(key_file);
        let key = rustls_pemfile::private_key(&mut key_reader)?
            .ok_or_else(|| anyhow!("No private key found in {:?}", key_path))?;

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)?;

        server_crypto.alpn_protocols = vec![b"aura-dave".to_vec()];

        let quinn_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .map_err(|e| anyhow!("Failed to convert manual TLS config: {}", e))?;

        Self::apply_transport_config(ServerConfig::with_crypto(Arc::new(quinn_crypto)))
    }

    /// Generate self-signed TLS certificate for QUIC.
    fn generate_self_signed_config() -> Result<ServerConfig> {
        let cert =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "aura.local".into()])?;
        let cert_der = cert.cert.der().to_vec();
        let key_der = cert.signing_key.serialize_der();

        let cert_chain = vec![CertificateDer::from(cert_der)];
        let key = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(key_der));

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)?;

        server_crypto.alpn_protocols = vec![b"aura-dave".to_vec()];

        let quinn_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .map_err(|e| anyhow!("Failed to convert self-signed config: {}", e))?;

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

                        if let Err(e) = handle_connection(connection, state).await {
                            warn!("[QUIC] Connection error from {}: {}", remote, e);
                        }
                    }
                    Err(e) => {
                        warn!("[QUIC] TLS handshake failed from {}: {}", remote, e);
                    }
                }
            });
        }

        Ok(())
    }
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
                                0x01 => { // Audio
                                    if data.len() > 1 {
                                        ctx.state.route_audio_packet(bytes::Bytes::copy_from_slice(&data[1..])).await;
                                    }
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

                        match ctx.handle_client_message(type_byte).await {
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
/// 2. Client sends AuthRequest with public key, name, signature of challenge
/// 3. Server validates and sends AuthResponse
async fn authenticate_client(
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

    // Step 2: Wait for AuthRequest from client
    info!("[Auth] Waiting for AuthRequest...");
    let msg_type = recv.read_u8().await?;
    info!("[Auth] Received message type: 0x{:02x}", msg_type);

    if msg_type != MSG_AUTH_REQUEST {
        return Err(anyhow!(
            "Expected AuthRequest (0x03), got 0x{:02x}",
            msg_type
        ));
    }

    // Parse auth request
    let key_len = recv.read_u8().await? as usize;
    info!("[Auth] Key length: {}", key_len);
    let mut auth_public_key = vec![0u8; key_len];
    recv.read_exact(&mut auth_public_key).await?;

    let name_len = recv.read_u8().await? as usize;
    info!("[Auth] Name length: {}", name_len);
    let mut name_buf = vec![0u8; name_len];
    recv.read_exact(&mut name_buf).await?;
    let display_name = String::from_utf8(name_buf)?;
    info!("[Auth] Display name: {}", display_name);

    let sig_len = recv.read_u8().await? as usize;
    info!("[Auth] Signature length: {}", sig_len);
    let mut signature = vec![0u8; sig_len];
    recv.read_exact(&mut signature).await?;

    let challenge_len = recv.read_u8().await? as usize;
    info!("[Auth] Challenge length: {}", challenge_len);
    let mut client_challenge = vec![0u8; challenge_len];
    recv.read_exact(&mut client_challenge).await?;

    let password_len = recv.read_u8().await? as usize;
    info!("[Auth] Password length: {}", password_len);
    let server_password = if password_len > 0 {
        let mut pw_buf = vec![0u8; password_len];
        recv.read_exact(&mut pw_buf).await?;
        Some(String::from_utf8(pw_buf)?)
    } else {
        None
    };

    // Verify challenge matches
    if client_challenge != challenge {
        return Err(anyhow!("Challenge mismatch"));
    }
    info!("[Auth] Challenge verified OK");

    // Convert public key
    let pk_array: [u8; 32] = auth_public_key
        .try_into()
        .map_err(|_| anyhow!("Invalid public key length"))?;

    // Authenticate via auth service
    let auth_result = state.auth.authenticate(
        &pk_array,
        &display_name,
        &signature,
        &challenge,
        server_password.as_deref(),
    );

    // Send auth response
    let mut response = BytesMut::new();
    response.put_u8(MSG_AUTH_RESPONSE);

    match auth_result {
        Ok(result) => {
            let user_uuid = result.user_uuid.clone();
            let session_token = result.session_token.clone();
            let verified = result.verified;
            let is_admin = result.is_admin;

            // Register session BEFORE sending auth response so we have a real session_id
            // IMPORTANT: Do NOT prefix rx with '_' — that would cause Rust to drop the
            // receiver immediately, silently discarding all service messages to this session.
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let session_id = state.register_session(user_uuid.to_string(), remote, tx);

            info!(
                "[Auth] Registered session {} for user {}",
                session_id, user_uuid
            );

            let success = true;

            response.put_u8(if success { 1 } else { 0 }); // success
            response.put_u32_le(session_id); // REAL session ID

            debug!(
                "[Auth] Sending AuthResponse: session_id={}, success={}, verified={}, is_admin={}",
                session_id, success, verified, is_admin
            );

            let token_bytes = session_token.as_bytes();
            response.put_u8(token_bytes.len() as u8);
            response.put_slice(token_bytes);

            response.put_u8(if verified { 1 } else { 0 });
            response.put_u8(if is_admin { 1 } else { 0 }); // New field: is_admin
            response.put_u8(0); // no error message

            send.write_all(&response).await?;

            Ok((session_id, user_uuid.to_string(), send, recv, rx))
        }
        Err(e) => {
            response.put_u8(0); // failure
            response.put_u32_le(0);
            response.put_u8(0); // no token
            response.put_u8(0); // not verified

            let error_msg = format!("{:?}", e);
            response.put_u8(error_msg.len() as u8);
            response.put_slice(error_msg.as_bytes());

            send.write_all(&response).await?;

            Err(anyhow!("Authentication failed: {:?}", e))
        }
    }
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

                self.state
                    .broadcast_text_message(self.session_id, packet)
                    .await;
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
                let buf = self.read_frame_payload().await?;
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
        }
        Ok(())
    }

    async fn read_frame_payload(&mut self) -> Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.recv.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;

        if len > MAX_CONTROL_PACKET_SIZE {
            return Err(anyhow!(
                "Incoming frame too large: {} bytes (max {})",
                len,
                MAX_CONTROL_PACKET_SIZE
            ));
        }

        if len == 0 {
            return Ok(Vec::new());
        }

        // Do NOT pre-allocate based on the attacker-controlled length prefix:
        // that lets a client claim 256KB and then send nothing, pinning one
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
mod tests {}
