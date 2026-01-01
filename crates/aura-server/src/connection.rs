//! QUIC connection handler module.
//!
//! Handles incoming QUIC connections, authentication, and stream routing.

use crate::auth::AuthService;
use crate::state::{ServerState, ServiceMessage};
use anyhow::{anyhow, Result};
use bytes::{BufMut, BytesMut};
use prost::Message;
use quinn::{Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::CertificateDer;
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
const MSG_TEXT_PACKET: u8 = 0x30;
const MSG_CREATE_CHANNEL: u8 = 0x40;
const MSG_UPDATE_CHANNEL: u8 = 0x41;
const MSG_UPDATE_PROFILE: u8 = 0x42;

// MLS Protocol messages
const MSG_MLS_JOIN: u8 = 0x50;           // Client sends key package on channel join
const MSG_MLS_COMMIT_WELCOME: u8 = 0x51; // Client sends commit + welcome after adding member

// Security limits
const MAX_PACKET_SIZE: usize = 64 * 1024; // 64KB

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

        // Authenticate the client - returns session_id directly now
        let (session_id, user_uuid, control_send, control_recv, mut rx) = match authenticate_client(control_send_initial, control_recv_initial, &state, remote).await {
            Ok(result) => result,
            Err(e) => {
                warn!("[{}] Authentication failed: {}", remote, e);
                return Err(e.into());
            }
        };

        // Session already registered in authenticate_client
        info!("[{}] Session {} authenticated for user {}", remote, session_id, user_uuid);
        
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
                                    0x00 => { // Keepalive
                                        let _ = ctx.conn.send_datagram(bytes::Bytes::from_static(&[0x00]));
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
            ctx.state.remove_from_voice_group(channel_id.clone(), session_id).await;
            ctx.state.remove_from_text_group(channel_id.clone(), session_id).await;
            ctx.state.broadcast_user_left(channel_id, session_id).await;
        }
        
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
/// Returns (session_id, user_uuid, SendStream, RecvStream, rx) for reuse after auth.
async fn authenticate_client(
    mut send: SendStream,
    mut recv: RecvStream,
    state: &Arc<ServerState>,
    remote: SocketAddr,
) -> Result<(u32, String, SendStream, RecvStream, tokio::sync::mpsc::UnboundedReceiver<ServiceMessage>)> {
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
            let user_uuid = result.user_uuid.clone();
            let session_token = result.session_token.clone();
            let verified = result.verified;
            let is_admin = result.is_admin;

            // Register session BEFORE sending auth response so we have a real session_id
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let session_id = state.register_session(user_uuid.to_string(), remote, tx);

            info!("[Auth] Registered session {} for user {}", session_id, user_uuid);

            let success = true;

            response.put_u8(if success { 1 } else { 0 }); // success
            response.put_u32_le(session_id); // REAL session ID

            debug!("[Auth] Sending AuthResponse: session_id={}, success={}, verified={}, is_admin={}", session_id, success, verified, is_admin);

            let token_bytes = session_token.as_bytes();
            response.put_u8(token_bytes.len() as u8);
            response.put_slice(token_bytes);

            response.put_u8(if verified { 1 } else { 0 });
            response.put_u8(if is_admin { 1 } else { 0 }); // New field: is_admin
            response.put_u8(0); // no error message

            send.write_all(&response).await?;

            Ok((session_id, user_uuid.to_string(), send, recv, _rx))
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
                // [0x10] [len u8] [channel_id string]
                let len = self.recv.read_u8().await? as usize;
                let mut buf = vec![0u8; len];
                self.recv.read_exact(&mut buf).await?;
                let channel_id = String::from_utf8(buf)?;
                
                info!("[{}] Joining channel {}", self.remote, channel_id);
                
                // Leave previous channel if any
                if let Some(old_id) = self.current_channel_id.clone() {
                    self.state.remove_from_voice_group(old_id.clone(), self.session_id).await;
                    self.state.remove_from_text_group(old_id.clone(), self.session_id).await;
                    self.state.broadcast_user_left(old_id, self.session_id).await;
                }
                
                // Join new channel
                self.current_channel_id = Some(channel_id.clone());
                self.state.add_to_voice_group(channel_id.clone(), self.session_id).await;
                self.state.add_to_text_group(channel_id.clone(), self.session_id).await;
                
                // Broadcast join
                if let Some(profile) = self.state.get_profile(&self.user_uuid) {
                    self.state.broadcast_user_joined(channel_id, self.session_id, profile.display_name).await;
                }
            }
            0x20 => { // MSG_AUDIO (legacy reliable path)
                 // Format: [20] [Length u32] [Payload...]
                 let mut len_buf = [0u8; 4];
                 self.recv.read_exact(&mut len_buf).await?;
                 let packet_len = u32::from_le_bytes(len_buf) as usize;
                 
                 info!("[{}] Received audio packet: {} bytes", self.remote, packet_len);
                 
                 // Limit audio packet size too
                 if packet_len > MAX_PACKET_SIZE {
                     warn!("[{}] Audio packet too large: {}", self.remote, packet_len);
                     return Ok(false); // Disconnect
                 }
                 
                 let mut packet_buf = vec![0u8; packet_len];
                 self.recv.read_exact(&mut packet_buf).await?;
                 self.state.route_audio_packet(bytes::Bytes::from(packet_buf)).await;
                 info!("[{}] Routed audio packet from session {}", self.remote, self.session_id);
            }
            MSG_TEXT_PACKET => {
                // [0x30] [Length u32] [BinaryPacket]
                let mut len_buf = [0u8; 4];
                self.recv.read_exact(&mut len_buf).await?;
                let packet_len = u32::from_le_bytes(len_buf) as usize;
                
                if packet_len > MAX_PACKET_SIZE {
                    warn!("[{}] Text packet too large: {}", self.remote, packet_len);
                    return Ok(false); // Disconnect
                }
                
                let mut packet_buf = vec![0u8; packet_len];
                self.recv.read_exact(&mut packet_buf).await?;
                
                match parse_text_packet(&packet_buf) {
                    Ok(text_packet) => {
                         let message_id = text_packet.message_id.clone();
                         info!("[{}] Text packet from session {} in channel {} (msgId: {})", 
                            self.remote, text_packet.sender_session_id, text_packet.channel_id, message_id);
                         
                         let needs_ratchet = self.state.broadcast_text_message(self.session_id, text_packet).await;
                         if needs_ratchet {
                             info!("[{}] Text group needs ratcheting", self.remote);
                         }
                    }
                    Err(e) => {
                        warn!("[{}] Invalid text packet: {}", self.remote, e);
                    }
                }
            }
            MSG_CREATE_CHANNEL => {
                let mut len_buf = [0u8; 4];
                self.recv.read_exact(&mut len_buf).await?;
                let len = u32::from_le_bytes(len_buf) as usize;
                
                let mut buf = vec![0u8; len];
                self.recv.read_exact(&mut buf).await?;
                
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

                match self.state.create_channel_persistent(req.name, req.comment, req.icon).await {
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
                let mut len_buf = [0u8; 4];
                self.recv.read_exact(&mut len_buf).await?;
                let len = u32::from_le_bytes(len_buf) as usize;
                
                let mut buf = vec![0u8; len];
                self.recv.read_exact(&mut buf).await?;
                
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

                match self.state.update_channel_persistent(req.channel_id, req.name, req.comment, req.icon, req.position).await {
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
                let mut len_buf = [0u8; 4];
                self.recv.read_exact(&mut len_buf).await?;
                let len = u32::from_le_bytes(len_buf) as usize;
                
                let mut buf = vec![0u8; len];
                self.recv.read_exact(&mut buf).await?;
                
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

                    match self.state.update_profile_persistent(self.session_id, profile).await {
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
            MSG_MLS_JOIN => {
                // [0x50] [channel_id_len: u8] [channel_id: string] [is_voice: u8] [kp_len: u32] [key_package]
                let id_len = self.recv.read_u8().await? as usize;
                let mut id_buf = vec![0u8; id_len];
                self.recv.read_exact(&mut id_buf).await?;
                let channel_id = String::from_utf8(id_buf)?;
                
                let is_voice = self.recv.read_u8().await? != 0;
                
                let mut len_buf = [0u8; 4];
                self.recv.read_exact(&mut len_buf).await?;
                let kp_len = u32::from_le_bytes(len_buf) as usize;
                
                if kp_len > MAX_PACKET_SIZE {
                    warn!("[{}] MLS key package too large: {}", self.remote, kp_len);
                    return Ok(false);
                }
                
                let mut key_package = vec![0u8; kp_len];
                self.recv.read_exact(&mut key_package).await?;
                
                info!("[{}] MLS join for {} channel {} ({} bytes KP)",
                      self.remote, if is_voice { "voice" } else { "text" }, channel_id, kp_len);
                
                self.state.handle_mls_join(
                    channel_id,
                    is_voice,
                    self.session_id,
                    self.user_uuid.clone(),
                    key_package,
                ).await;
            }
            MSG_MLS_COMMIT_WELCOME => {
                // [0x51] [channel_id_len: u8] [channel_id: string] [is_voice: u8] [new_member_session_id: u32]
                //        [commit_len: u32] [commit] [welcome_len: u32] [welcome]
                let id_len = self.recv.read_u8().await? as usize;
                let mut id_buf = vec![0u8; id_len];
                self.recv.read_exact(&mut id_buf).await?;
                let channel_id = String::from_utf8(id_buf)?;
                
                let is_voice = self.recv.read_u8().await? != 0;
                
                let mut len_buf = [0u8; 4];
                self.recv.read_exact(&mut len_buf).await?;
                let new_member_session_id = u32::from_le_bytes(len_buf);
                
                self.recv.read_exact(&mut len_buf).await?;
                let commit_len = u32::from_le_bytes(len_buf) as usize;
                if commit_len > MAX_PACKET_SIZE {
                    return Ok(false);
                }
                let mut commit = vec![0u8; commit_len];
                self.recv.read_exact(&mut commit).await?;
                
                self.recv.read_exact(&mut len_buf).await?;
                let welcome_len = u32::from_le_bytes(len_buf) as usize;
                if welcome_len > MAX_PACKET_SIZE {
                    return Ok(false);
                }
                let mut welcome = vec![0u8; welcome_len];
                self.recv.read_exact(&mut welcome).await?;
                
                info!("[{}] MLS commit/welcome for {} channel {} (new member: {})",
                      self.remote, if is_voice { "voice" } else { "text" }, channel_id, new_member_session_id);
                
                self.state.handle_mls_commit_welcome(
                    channel_id,
                    is_voice,
                    self.session_id,
                    new_member_session_id,
                    commit,
                    welcome,
                ).await;
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
                
                if self.conn.send_datagram(bytes::Bytes::from(dgram_data)).is_err() {
                    // Fallback to reliable stream if datagrams fail
                    self.send.write_u8(0x20).await?;  // MSG_AUDIO
                    self.send.write_u32_le(packet.len() as u32).await?;
                    self.send.write_all(&packet).await?;
                    self.send.flush().await?;
                }
            }
            ServiceMessage::UserJoined { channel_id, session_id: joined_id, display_name } => {
                let id_bytes = channel_id.as_bytes();
                let name_bytes = display_name.as_bytes();
                let mut msg = vec![0x11u8];
                msg.push(id_bytes.len() as u8);
                msg.extend_from_slice(id_bytes);
                msg.extend_from_slice(&joined_id.to_le_bytes());
                msg.push(name_bytes.len() as u8);
                msg.extend_from_slice(name_bytes);
                self.send.write_all(&msg).await?;
                self.send.flush().await?;
            }
            ServiceMessage::UserLeft { channel_id, session_id: left_id } => {
                let id_bytes = channel_id.as_bytes();
                let mut msg = vec![0x12u8];
                msg.push(id_bytes.len() as u8);
                msg.extend_from_slice(id_bytes);
                msg.extend_from_slice(&left_id.to_le_bytes());
                self.send.write_all(&msg).await?;
                self.send.flush().await?;
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
                let packet_bytes = serialize_text_packet(&text_packet);
                let mut msg = vec![MSG_TEXT_PACKET];
                msg.extend_from_slice(&(packet_bytes.len() as u32).to_le_bytes());
                msg.extend_from_slice(&packet_bytes);
                self.send.write_all(&msg).await?;
                self.send.flush().await?;
            }
            
            // --- MLS Protocol Messages ---
            
            ServiceMessage::MlsCreateGroup { channel_id, is_voice } => {
                let id_bytes = channel_id.as_bytes();
                let mut msg = vec![0x52];
                msg.push(id_bytes.len() as u8);
                msg.extend_from_slice(id_bytes);
                msg.push(if is_voice { 1 } else { 0 });
                self.send.write_all(&msg).await?;
                self.send.flush().await?;
                info!("[{}] Sent MlsCreateGroup for channel {}", self.remote, channel_id);
            }
            ServiceMessage::MlsAddMemberRequest { channel_id, is_voice, joiner_session_id, joiner_uuid, key_package } => {
                let id_bytes = channel_id.as_bytes();
                let uuid_bytes = joiner_uuid.as_bytes();
                let mut msg = vec![0x53];
                msg.push(id_bytes.len() as u8);
                msg.extend_from_slice(id_bytes);
                msg.push(if is_voice { 1 } else { 0 });
                msg.extend_from_slice(&joiner_session_id.to_le_bytes());
                msg.push(uuid_bytes.len() as u8);
                msg.extend_from_slice(uuid_bytes);
                msg.extend_from_slice(&(key_package.len() as u32).to_le_bytes());
                msg.extend_from_slice(&key_package);
                self.send.write_all(&msg).await?;
                self.send.flush().await?;
            }
            ServiceMessage::MlsCommit { channel_id, is_voice, commit } => {
                let id_bytes = channel_id.as_bytes();
                let mut msg = vec![0x54];
                msg.push(id_bytes.len() as u8);
                msg.extend_from_slice(id_bytes);
                msg.push(if is_voice { 1 } else { 0 });
                msg.extend_from_slice(&(commit.len() as u32).to_le_bytes());
                msg.extend_from_slice(&commit);
                self.send.write_all(&msg).await?;
                self.send.flush().await?;
            }
            ServiceMessage::MlsWelcome { channel_id, is_voice, welcome } => {
                let id_bytes = channel_id.as_bytes();
                let mut msg = vec![0x55];
                msg.push(id_bytes.len() as u8);
                msg.extend_from_slice(id_bytes);
                msg.push(if is_voice { 1 } else { 0 });
                msg.extend_from_slice(&(welcome.len() as u32).to_le_bytes());
                msg.extend_from_slice(&welcome);
                self.send.write_all(&msg).await?;
                self.send.flush().await?;
            }
        }
        Ok(())
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

fn parse_text_packet(packet_buf: &[u8]) -> Result<aura_protocol::EncryptedTextPacket> {
    // binary format:
    // sender(4) + channel(4) + epoch(8) + message_id_len(1) + message_id + content_len(4) + content + nonce(24) + tag(16)
    // min size: 4+4+8+1+0+4+0+24+16 = 61
    
    if packet_buf.len() < 61 {
        return Err(anyhow!("Packet too short: {} bytes", packet_buf.len()));
    }
    
    let mut offset = 0;
    
    fn read_u32(buf: &[u8], offset: &mut usize) -> Result<u32> {
        if *offset + 4 > buf.len() { return Err(anyhow!("Unexpected EOF parsing u32")); }
        let val = u32::from_le_bytes(buf[*offset..*offset+4].try_into()?);
        *offset += 4;
        Ok(val)
    }
    
    fn read_u64(buf: &[u8], offset: &mut usize) -> Result<u64> {
        if *offset + 8 > buf.len() { return Err(anyhow!("Unexpected EOF parsing u64")); }
        let val = u64::from_le_bytes(buf[*offset..*offset+8].try_into()?);
        *offset += 8;
        Ok(val)
    }
    
    let sender_session_id = read_u32(packet_buf, &mut offset)?;
    
    // Channel ID
    if offset + 1 > packet_buf.len() { return Err(anyhow!("Unexpected EOF parsing channel_id_len")); }
    let channel_id_len = packet_buf[offset] as usize;
    offset += 1;
    if offset + channel_id_len > packet_buf.len() { return Err(anyhow!("Unexpected EOF parsing channel_id")); }
    let channel_id = String::from_utf8(packet_buf[offset..offset+channel_id_len].to_vec())
        .map_err(|_| anyhow!("Invalid UTF-8 in channel_id"))?;
    offset += channel_id_len;

    let epoch = read_u64(packet_buf, &mut offset)?;
    
    // Message ID
    if offset + 1 > packet_buf.len() { return Err(anyhow!("Unexpected EOF parsing msg_id_len")); }
    let msg_id_len = packet_buf[offset] as usize;
    offset += 1;
    
    if offset + msg_id_len > packet_buf.len() { return Err(anyhow!("Unexpected EOF parsing msg_id")); }
    let message_id = String::from_utf8(packet_buf[offset..offset+msg_id_len].to_vec())
        .map_err(|_| anyhow!("Invalid UTF-8 in message_id"))?;
    offset += msg_id_len;
    
    // Content
    let content_len = read_u32(packet_buf, &mut offset)? as usize;
    if offset + content_len > packet_buf.len() { return Err(anyhow!("Unexpected EOF parsing content")); }
    
    let ciphertext = packet_buf[offset..offset+content_len].to_vec();
    offset += content_len;
    
    // Nonce & Tag
    if offset + 24 > packet_buf.len() { return Err(anyhow!("Unexpected EOF parsing nonce")); }
    let nonce = packet_buf[offset..offset+24].to_vec();
    offset += 24;
    
    if offset + 16 > packet_buf.len() { return Err(anyhow!("Unexpected EOF parsing tag")); }
    let tag = packet_buf[offset..offset+16].to_vec();
    offset += 16;
    
    // Reply To ID (Optional)
    let mut reply_to_id = String::new();
    if offset < packet_buf.len() {
        let reply_len = packet_buf[offset] as usize;
        offset += 1;
        if reply_len > 0 {
            if offset + reply_len > packet_buf.len() { return Err(anyhow!("Unexpected EOF parsing reply_id")); }
            reply_to_id = String::from_utf8(packet_buf[offset..offset+reply_len].to_vec())
                .unwrap_or_default();
        }
    }
    
    Ok(aura_protocol::EncryptedTextPacket {
        sender_session_id,
        channel_id,
        epoch,
        message_id,
        ciphertext,
        nonce,
        tag,
        reply_to_id
    })
}

fn serialize_text_packet(packet: &aura_protocol::EncryptedTextPacket) -> Vec<u8> {
    let mut size = 62 + packet.message_id.len() + packet.ciphertext.len();
    if !packet.reply_to_id.is_empty() {
        size += packet.reply_to_id.len();
    }
    
    let mut buf = Vec::with_capacity(size);
    buf.extend_from_slice(&packet.sender_session_id.to_le_bytes());
    
    let channel_id_bytes = packet.channel_id.as_bytes();
    buf.push(channel_id_bytes.len().min(255) as u8);
    buf.extend_from_slice(&channel_id_bytes[..channel_id_bytes.len().min(255)]);

    buf.extend_from_slice(&packet.epoch.to_le_bytes());
    
    let msg_id_bytes = packet.message_id.as_bytes();
    buf.push(msg_id_bytes.len().min(255) as u8);
    buf.extend_from_slice(&msg_id_bytes[..msg_id_bytes.len().min(255)]);
    
    buf.extend_from_slice(&(packet.ciphertext.len() as u32).to_le_bytes());
    buf.extend_from_slice(&packet.ciphertext);
    
    buf.extend_from_slice(&packet.nonce);
    buf.extend_from_slice(&packet.tag);
    
    if !packet.reply_to_id.is_empty() {
        let reply_bytes = packet.reply_to_id.as_bytes();
        buf.push(reply_bytes.len().min(255) as u8);
        buf.extend_from_slice(&reply_bytes[..reply_bytes.len().min(255)]);
    } else {
        buf.push(0);
    }
    
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_packet_roundtrip() {
        let packet = aura_protocol::EncryptedTextPacket {
            sender_session_id: 123,
            channel_id: "C_channel_123".to_string(),
            epoch: 789,
            message_id: "M_msg_uuid_1234".to_string(),
            ciphertext: vec![1, 2, 3, 4],
            nonce: vec![5; 24],
            tag: vec![6; 16],
            reply_to_id: "M_msg_reply_5678".to_string(),
        };
        
        let bytes = serialize_text_packet(&packet);
        let parsed = parse_text_packet(&bytes).expect("Failed to parse packet");
        
        assert_eq!(parsed.sender_session_id, packet.sender_session_id);
        assert_eq!(parsed.channel_id, packet.channel_id);
        assert_eq!(parsed.epoch, packet.epoch);
        assert_eq!(parsed.message_id, packet.message_id);
        assert_eq!(parsed.ciphertext, packet.ciphertext);
        assert_eq!(parsed.nonce, packet.nonce);
        assert_eq!(parsed.tag, packet.tag);
        assert_eq!(parsed.reply_to_id, packet.reply_to_id);
    }
    
    #[test]
    fn test_packet_too_short() {
        let bytes = vec![0u8; 60];
        let result = parse_text_packet(&bytes);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_max_packet_size_logic() {
        assert_eq!(MAX_PACKET_SIZE, 65536);
        let packet = aura_protocol::EncryptedTextPacket {
            sender_session_id: 1,
            channel_id: "C_1".to_string(),
            epoch: 1,
            message_id: "M_1".to_string(),
            ciphertext: vec![0u8; 70000],
            nonce: vec![0u8; 24],
            tag: vec![0u8; 16],
            reply_to_id: "".to_string(),
        };
        
        let bytes = serialize_text_packet(&packet);
        assert!(bytes.len() > MAX_PACKET_SIZE);
        let result = parse_text_packet(&bytes);
        assert!(result.is_ok());
    }
    
    #[test]
    fn test_invalid_utf8_message_id() {
         let packet = aura_protocol::EncryptedTextPacket {
            sender_session_id: 1,
            channel_id: "C_1".to_string(),
            epoch: 1,
            message_id: "valid".to_string(),
            ciphertext: vec![],
            nonce: vec![0u8; 24],
            tag: vec![0u8; 16],
            reply_to_id: "".to_string(),
        };
        
        let mut bytes = serialize_text_packet(&packet);
        bytes[17] = 0xFF;
        let result = parse_text_packet(&bytes);
        assert!(result.is_err());
    }
}
