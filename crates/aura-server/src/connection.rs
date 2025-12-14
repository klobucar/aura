//! QUIC connection handler module.
//!
//! Handles incoming QUIC connections, authentication, and stream routing.

use crate::auth::{AuthError, AuthService};
use crate::config::Config;
use crate::db::Database;
use crate::state::ServerState;
use anyhow::{anyhow, Result};
use bytes::{Buf, BufMut, BytesMut};
use quinn::{Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::{Certificate, PrivateKey};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info, warn};

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
        // Generate self-signed certificate using rcgen
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "aura.local".into()])?;
        let cert_der = cert.serialize_der()?;
        let key_der = cert.serialize_private_key_der();
        
        let cert_chain = vec![Certificate(cert_der)];
        let key = PrivateKey(key_der);
        
        let mut server_crypto = rustls::ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)?;
        
        server_crypto.alpn_protocols = vec![b"aura-dave".to_vec()];
        
        let mut server_config = ServerConfig::with_crypto(Arc::new(server_crypto));
        
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
async fn handle_connection(connection: Connection, state: Arc<ServerState>) -> Result<()> {
    let remote = connection.remote_address();
    
    // For Apple Network.framework compatibility, the server opens the bidirectional stream
    // Apple's NWConnection creates an implicit stream that Quinn doesn't recognize via accept_bi()
    info!("[{}] Opening bidirectional stream for auth...", remote);
    let (send, recv) = connection.open_bi().await.map_err(|e| anyhow!("Failed to open stream: {}", e))?;
    info!("[{}] Bidirectional stream opened, starting authentication", remote);
    
    // Authenticate the client - get back the streams for reuse
    let (session, mut control_send, mut control_recv) = match authenticate_client(send, recv, &state).await {
        Ok(result) => result,
        Err(e) => {
            warn!("Authentication failed from {}: {}", remote, e);
            return Err(e.into());
        }
    };
    
    let session_id = session.session_id;
    let user_id = session.user_id;
    info!("Client {} authenticated as user {} (session {})", remote, user_id, session_id);
    
    // Handle additional streams and datagrams
    loop {
        tokio::select! {
            // Monitor control stream for messages or disconnection
            control_msg = control_recv.read_u8() => {
                match control_msg {
                    Ok(msg_type) => {
                        // Handle control messages (join channel, audio, etc.)
                        match msg_type {
                            0x10 => { // MSG_JOIN_CHANNEL
                                let mut buf = [0u8; 4];
                                if control_recv.read_exact(&mut buf).await.is_ok() {
                                    let channel_id = u32::from_le_bytes(buf);
                                    info!("[{}] User {} joining channel {}", remote, user_id, channel_id);
                                }
                            }
                            0x20 => { // MSG_AUDIO
                                // Format: session_id(4) + seq(2) + payload_len(2) + payload
                                let mut header = [0u8; 8];
                                if control_recv.read_exact(&mut header).await.is_ok() {
                                    let session_id = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                                    let seq = u16::from_le_bytes([header[4], header[5]]);
                                    let payload_len = u16::from_le_bytes([header[6], header[7]]) as usize;
                                    
                                    // Read payload
                                    let mut payload = vec![0u8; payload_len];
                                    if control_recv.read_exact(&mut payload).await.is_ok() {
                                        if seq % 50 == 0 {
                                            info!("[{}] Received audio: session={}, seq={}, {} bytes", 
                                                  remote, session_id, seq, payload_len);
                                        }
                                        // TODO: Route to other clients
                                    }
                                }
                            }
                            _ => {
                                info!("[{}] Unknown control message: 0x{:02x}", remote, msg_type);
                            }
                        }
                    }
                    Err(e) => {
                        info!("Client {} control stream disconnected: {}", remote, e);
                        break;
                    }
                }
            }

            // Receive QUIC datagrams (unreliable audio packets)
            datagram = connection.read_datagram() => {
                match datagram {
                    Ok(data) => {
                        // Route audio datagram to other clients
                        info!("[{}] Received audio datagram: {} bytes", remote, data.len());
                        state.route_audio_packet(data);
                    }
                    Err(e) => {
                        // Datagram receive error - connection may be closing
                        info!("Client {} datagram error: {}", remote, e);
                        break;
                    }
                }
            }
            
            // Accept bidirectional streams (control messages)
            result = connection.accept_bi() => {
                match result {
                    Ok((send, recv)) => {
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            if let Err(e) = handle_control_stream(send, recv, session_id, &state).await {
                                warn!("Control stream error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        info!("Client {} disconnected: {}", remote, e);
                        break;
                    }
                }
            }
            
            // Accept unidirectional streams (fallback audio data)
            result = connection.accept_uni() => {
                match result {
                    Ok(recv) => {
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            if let Err(e) = handle_audio_stream(recv, session_id, &state).await {
                                warn!("Audio stream error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        info!("Client {} disconnected: {}", remote, e);
                        break;
                    }
                }
            }
        }
    }
    
    // Cleanup session
    state.remove_session(session_id);
    info!("Session {} cleaned up", session_id);
    
    Ok(())
}

/// Client session after authentication.
struct ClientSession {
    session_id: u32,
    user_id: u32,
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
) -> Result<(ClientSession, SendStream, RecvStream)> {
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
            response.put_u32_le(result.user_id);
            
            let token_bytes = result.session_token.as_bytes();
            response.put_u8(token_bytes.len() as u8);
            response.put_slice(token_bytes);
            
            response.put_u8(if result.verified { 1 } else { 0 });
            response.put_u8(0); // no error message
            
            send.write_all(&response).await?;
            
            // Register session
            let session_id = state.register_session(result.user_id, "127.0.0.1:0".parse()?);
            
            Ok((ClientSession {
                session_id,
                user_id: result.user_id,
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

/// Handle control stream messages (join channel, etc.)
async fn handle_control_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    session_id: u32,
    state: &ServerState,
) -> Result<()> {
    loop {
        let msg_type = match recv.read_u8().await {
            Ok(t) => t,
            Err(_) => break, // Stream closed
        };
        
        match msg_type {
            MSG_JOIN_CHANNEL => {
                let channel_id = recv.read_u32_le().await?;
                info!("Session {} joining channel {}", session_id, channel_id);
                
                // Update session's channel
                if let Some(mut session) = state.sessions.get_mut(&session_id) {
                    session.voice_group_id = Some(channel_id);
                    session.text_group_id = Some(channel_id);
                }
                
                // Send success response
                send.write_all(&[0x11, 0x01]).await?; // JoinChannelResponse, success
            }
            _ => {
                warn!("Unknown control message type: {}", msg_type);
            }
        }
    }
    
    Ok(())
}

/// Handle incoming audio stream.
async fn handle_audio_stream(
    mut recv: RecvStream,
    session_id: u32,
    state: &ServerState,
) -> Result<()> {
    let mut packet_count = 0u64;
    
    loop {
        // Read length-prefixed packet
        let length = match recv.read_u32_le().await {
            Ok(l) => l as usize,
            Err(_) => break,
        };
        
        if length < 32 || length > 4096 {
            warn!("Invalid packet length: {}", length);
            continue;
        }
        
        let mut packet = vec![0u8; length];
        recv.read_exact(&mut packet).await?;
        
        packet_count += 1;
        
        if packet_count % 100 == 0 {
            info!("Session {} received {} audio packets", session_id, packet_count);
        }
        
        // TODO: Route to other clients in the same channel
        // For now, just count packets
    }
    
    info!("Audio stream from session {} ended ({} packets)", session_id, packet_count);
    Ok(())
}
