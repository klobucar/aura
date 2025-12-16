//! QUIC connection handler module.
//!
//! Handles incoming QUIC connections, authentication, and stream routing.

use crate::auth::AuthService;
use crate::state::{ServerState, ServiceMessage};
use anyhow::{anyhow, Result};
use bytes::{BufMut, BytesMut};
use quinn::{Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

// Protocol message types
const MSG_CHALLENGE_REQUEST: u8 = 0x01;
const MSG_CHALLENGE_RESPONSE: u8 = 0x02;
const MSG_AUTH_REQUEST: u8 = 0x03;
const MSG_AUTH_RESPONSE: u8 = 0x04;
const MSG_JOIN_CHANNEL: u8 = 0x10;
const MSG_AUDIO_STREAM: u8 = 0x20;

/// QUIC server for handling client connections.
pub struct QuicServer {
    endpoint: Endpoint,
    state: Arc<ServerState>,
}

impl QuicServer {
    /// Create a new QUIC server with self-signed certificate.
    pub fn new(bind_addr: SocketAddr, state: Arc<ServerState>) -> Result<Self> {
        info!("Generating self-signed TLS certificate...");
        let server_config = Self::generate_server_config()?;
        
        info!("Creating QUIC endpoint on {}...", bind_addr);
        let endpoint = Endpoint::server(server_config, bind_addr)
            .map_err(|e| anyhow!("Failed to bind QUIC endpoint to {}: {}", bind_addr, e))?;
        
        let local_addr = endpoint.local_addr()
            .map_err(|e| anyhow!("Failed to get local address: {}", e))?;
        
        info!("✓ QUIC server bound to UDP socket: {}", local_addr);
        info!("✓ TLS certificate generated (self-signed)");
        info!("✓ ALPN protocol: 'aura-dave'");
        
        Ok(Self { endpoint, state })
    }
    
    /// Generate self-signed TLS certificate for QUIC.
    fn generate_server_config() -> Result<ServerConfig> {
        // Generate self-signed certificate using rcgen 0.13
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "aura.local".into()])?;
        
        let cert_der = cert.cert.der().to_vec();
        let key_der = cert.signing_key.serialize_der();
        
        let cert_chain = vec![CertificateDer::from(cert_der)];
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(key_der).into();

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)?;
        
        
        server_crypto.alpn_protocols = vec![b"aura-dave".to_vec()];
        
        let quinn_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .map_err(|e| anyhow!("Failed to convert rustls config: {}", e))?;
            
        let mut server_config = ServerConfig::with_crypto(Arc::new(quinn_crypto));
        
        // Configure transport for low-latency voice
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
        let (control_send_initial, control_recv_initial) = conn.open_bi().await
            .map_err(|e| anyhow!("Failed to open control stream: {}", e))?;

        info!("[{}] Control stream opened", remote);

        // Authenticate the client - get back the streams for reuse
        let (auth_session, mut control_send, mut control_recv) = match authenticate_client(control_send_initial, control_recv_initial, &state, remote).await {
            Ok(result) => result,
            Err(e) => {
                warn!("[{}] Authentication failed: {}", remote, e);
                return Err(e.into());
            }
        };

        // Session already registered in authenticate_client
        let user_uuid = auth_session.user_uuid.clone();
        
        // Create internal channel for receiving messages from state
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        
        // Register session with state
        let session_id = state.register_session(user_uuid.clone(), remote, tx);
        
        info!("[{}] Session {} registered for user {}", remote, session_id, user_uuid);
        
        // Send initial state of all channels to the new user
        state.send_all_channel_states(session_id).await;

        // Keepalive interval
        let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(10));
        let mut last_activity = std::time::Instant::now();
        let keepalive_timeout = std::time::Duration::from_secs(30);

        // Main loop - handle streams, datagrams, and internal messages
        loop {
            tokio::select! {
                // Handle QUIC datagrams (unreliable fast path for audio)
                datagram = conn.read_datagram() => {
                    match datagram {
                        Ok(data) => {
                            last_activity = std::time::Instant::now();
                            // Route audio packet (first byte indicates type)
                            if !data.is_empty() {
                                match data[0] {
                                    0x01 => {
                                        // Audio packet (skip type byte)
                                        if data.len() > 1 {
                                            state.route_audio_packet(bytes::Bytes::copy_from_slice(&data[1..])).await;
                                        }
                                    }
                                    0x00 => {
                                        // Keepalive ping - respond with pong
                                        let _ = conn.send_datagram(bytes::Bytes::from_static(&[0x00]));
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

                // Monitor control stream
                control_msg = control_recv.read_u8() => {
                    match control_msg {
                        Ok(msg_type) => {
                            last_activity = std::time::Instant::now();
                             match msg_type {
                                0x00 => { // Keepalive ping via control stream
                                    debug!("[{}] Keepalive ping received", remote);
                                    // Activity already updated above, nothing else needed
                                }
                                0x10 => { // MSG_JOIN_CHANNEL
                                    let mut buf = [0u8; 4];
                                    if control_recv.read_exact(&mut buf).await.is_ok() {
                                        let channel_id = u32::from_le_bytes(buf);
                                        state.create_channel(channel_id); // Ensure exists
                                        
                                        // Get display name for broadcast
                                        let display_name = state.db.find_user_by_uuid(&user_uuid)
                                            .ok()
                                            .flatten()
                                            .map(|u| u.display_name)
                                            .unwrap_or_else(|| format!("User {}", session_id));
                                        
                                        // Check if switching from another channel
                                        let old_channel_id = state.sessions.get(&session_id)
                                            .and_then(|s| s.voice_group_id);
                                        
                                        // Leave old channel if different
                                        if let Some(old_id) = old_channel_id {
                                            if old_id != channel_id {
                                                // Broadcast user left to old channel
                                                state.broadcast_user_left(old_id, session_id).await;
                                                
                                                // Remove from old voice group
                                                if let Some(vg) = state.voice_groups.get(&old_id) {
                                                    vg.value().write().await.members.remove(&session_id);
                                                }
                                            }
                                        }
                                        
                                        // Update session
                                        if let Some(mut sess) = state.sessions.get_mut(&session_id) {
                                            sess.voice_group_id = Some(channel_id);
                                            sess.text_group_id = Some(channel_id);
                                            
                                            // Add to voice group
                                            if let Some(vg) = state.voice_groups.get(&channel_id) {
                                                vg.value().write().await.members.insert(session_id);
                                            }
                                        }
                                        
                                        // Broadcast user joined to channel
                                        state.broadcast_user_joined(channel_id, session_id, display_name).await;
                                        
                                        info!("[{}] User {} joined channel {}", remote, user_uuid, channel_id);
                                    }
                                }
                                0x20 => { // MSG_AUDIO (legacy reliable path)
                                    // Format: [20] [Length u32] [Payload...]
                                    let mut len_buf = [0u8; 4];
                                    if control_recv.read_exact(&mut len_buf).await.is_ok() {
                                        let packet_len = u32::from_le_bytes(len_buf) as usize;
                                        
                                        // Read payload
                                        let mut packet_buf = vec![0u8; packet_len];
                                        if control_recv.read_exact(&mut packet_buf).await.is_ok() {
                                            state.route_audio_packet(bytes::Bytes::from(packet_buf)).await;
                                        }
                                    }
                                }
                                _ => {
                                     // Unknown
                                }
                            }
                        }
                        Err(_) => break, // Disconnected
                    }
                }
                
                // Monitor internal messages (relay)
                Some(msg) = rx.recv() => {
                    match msg {
                        ServiceMessage::RelayAudio(packet) => {
                            // Try datagram first (fast path), fall back to stream
                            let mut dgram_data = vec![0x01u8]; // Audio type
                            dgram_data.extend_from_slice(&packet);
                            if conn.send_datagram(bytes::Bytes::from(dgram_data)).is_err() {
                                // Fallback to reliable stream
                                let _ = control_send.write_u8(MSG_AUDIO_STREAM).await;
                                let _ = control_send.write_all(&packet).await;
                                let _ = control_send.flush().await;
                            }
                        }
                        ServiceMessage::UserJoined { channel_id, session_id: joined_id, display_name } => {
                            // Send user joined message: [0x11] [channel_id u32] [session_id u32] [name_len u8] [name...]
                            let name_bytes = display_name.as_bytes();
                            let mut msg = vec![0x11u8];
                            msg.extend_from_slice(&channel_id.to_le_bytes());
                            msg.extend_from_slice(&joined_id.to_le_bytes());
                            msg.push(name_bytes.len() as u8);
                            msg.extend_from_slice(name_bytes);
                            let _ = control_send.write_all(&msg).await;
                            let _ = control_send.flush().await;
                        }
                        ServiceMessage::UserLeft { channel_id, session_id: left_id } => {
                            // Send user left message: [0x12] [channel_id u32] [session_id u32]
                            let mut msg = vec![0x12u8];
                            msg.extend_from_slice(&channel_id.to_le_bytes());
                            msg.extend_from_slice(&left_id.to_le_bytes());
                            let _ = control_send.write_all(&msg).await;
                            let _ = control_send.flush().await;
                        }
                        ServiceMessage::ChannelState { channel_id, users } => {
                            // Send channel state: [0x13] [channel_id u32] [user_count u8] [users...]
                            let mut msg = vec![0x13u8];
                            msg.extend_from_slice(&channel_id.to_le_bytes());
                            msg.push(users.len().min(255) as u8);
                            for user in users.iter().take(255) {
                                msg.extend_from_slice(&user.session_id.to_le_bytes());
                                let name_bytes = user.display_name.as_bytes();
                                msg.push(name_bytes.len().min(255) as u8);
                                msg.extend_from_slice(&name_bytes[..name_bytes.len().min(255)]);
                            }
                            let _ = control_send.write_all(&msg).await;
                            let _ = control_send.flush().await;
                        }
                    }
                }

                // Keepalive timer
                _ = keepalive.tick() => {
                    // Check for timeout
                    if last_activity.elapsed() > keepalive_timeout {
                        warn!("[{}] Session {} timed out", remote, session_id);
                        break;
                    }
                    // Send keepalive ping via datagram
                    let _ = conn.send_datagram(bytes::Bytes::from_static(&[0x00]));
                }
            }
        }

        // Cleanup
        state.remove_session(session_id).await;
        info!("[{}] Session {} disconnected", remote, session_id);
        Ok(())
    }


/// Client session after authentication.
struct AuthSession {
    session_id: u32,
    user_uuid: String,
    session_token: String,
}

/// Authenticate a client using TOFU protocol.
/// Server-first protocol for Apple Network.framework compatibility:
/// 1. Server sends ServerHello with challenge
/// 2. Client sends AuthRequest with public key, name, signature of challenge
/// 3. Server validates and sends AuthResponse
/// Returns (ClientSession, SendStream, RecvStream) for reuse after auth.
async fn authenticate_client(
    mut send: SendStream,
    mut recv: RecvStream,
    state: &ServerState,
    remote: SocketAddr,
) -> Result<(AuthSession, SendStream, RecvStream)> {
    // Step 1: Server sends challenge first (ServerHello)
    let challenge = AuthService::generate_challenge();
    info!("[Auth] Sending ServerHello with challenge: {}...", hex::encode(&challenge[..8]));
    
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
        return Err(anyhow!("Expected AuthRequest (0x03), got 0x{:02x}", msg_type));
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
    let pk_array: [u8; 32] = auth_public_key.try_into().map_err(|_| anyhow!("Invalid public key length"))?;
    
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
            response.put_u8(1); // success
            response.put_u32_le(0); // Temporary - will be replaced with actual session_id after registration
            
            let token_bytes = result.session_token.as_bytes();
            response.put_u8(token_bytes.len() as u8);
            response.put_slice(token_bytes);
            
            response.put_u8(if result.verified { 1 } else { 0 });
            response.put_u8(0); // no error message
            
            send.write_all(&response).await?;
            
            Ok((AuthSession {
                session_id: 0, // Will be set after registration in main handler
                user_uuid: result.user_uuid,
                session_token: result.session_token,
            }, send, recv))
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


