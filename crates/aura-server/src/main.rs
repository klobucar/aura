//! Aura Server - Zero-Trust Voice/Text Relay
//!
//! This is the main entry point for the Aura server with:
//! - TOFU (Trust On First Use) authentication
//! - SQLite persistence for user identities
//! - Optional server password for access control
//! - Admin system for user verification and moderation
//! - QUIC transport for low-latency voice

mod auth;
mod config;
mod connection;
mod db;
mod state;

use anyhow::Result;
use std::sync::Arc;
use tracing::{info, error};

use crate::config::Config;
use crate::connection::QuicServer;
use crate::db::Database;
use crate::state::ServerState;

/// Environment variable for bootstrap admin public key (hex encoded).
const ENV_BOOTSTRAP_ADMIN_KEY: &str = "AURA_BOOTSTRAP_ADMIN_KEY";
/// Environment variable for bootstrap admin display name.
const ENV_BOOTSTRAP_ADMIN_NAME: &str = "AURA_BOOTSTRAP_ADMIN_NAME";

/// Initialize the bootstrap admin if database is empty and env var is set.
fn initialize_bootstrap_admin(db: &Database) -> Result<()> {
    // Only create bootstrap admin if database is empty
    if !db.is_empty()? {
        return Ok(());
    }

    let key_hex = match std::env::var(ENV_BOOTSTRAP_ADMIN_KEY) {
        Ok(k) => k,
        Err(_) => {
            info!("No bootstrap admin configured. Set {} to create first admin.", ENV_BOOTSTRAP_ADMIN_KEY);
            return Ok(());
        }
    };

    // Parse hex public key
    let key_bytes = hex::decode(&key_hex).map_err(|e| {
        anyhow::anyhow!("Invalid bootstrap admin key (not valid hex): {}", e)
    })?;

    if key_bytes.len() != 32 {
        return Err(anyhow::anyhow!(
            "Bootstrap admin key must be 32 bytes (64 hex chars), got {}",
            key_bytes.len()
        ));
    }

    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(&key_bytes);

    // Get display name (default to "Admin")
    let display_name = std::env::var(ENV_BOOTSTRAP_ADMIN_NAME)
        .unwrap_or_else(|_| "Admin".to_string());

    // Create bootstrap admin
    db.create_bootstrap_admin(&public_key, &display_name)?;
    info!(
        "Created bootstrap admin '{}' with key {}...",
        display_name,
        &key_hex[..16]
    );

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();
    info!("Starting Aura Zero-Trust Relay...");

    // Load configuration
    let config = Config::load()?;
    info!("Loaded configuration:");
    info!("  Bind address: {}", config.server.bind_address);
    info!("  Max connections: {}", config.server.max_connections);
    info!("  Database: {:?}", config.database.path);
    info!("  Verification mode: {:?}", config.verification.mode);
    info!(
        "  Server password: {}",
        if config.requires_password() { "enabled" } else { "disabled" }
    );

    // Initialize database
    let db = Arc::new(Database::open(&config.database.path)?);
    info!("Database initialized at {:?}", config.database.path);

    // Initialize bootstrap admin if needed
    if let Err(e) = initialize_bootstrap_admin(&db) {
        error!("Failed to create bootstrap admin: {}", e);
    }

    // Create server state with persistence
    let state = Arc::new(ServerState::new(Arc::clone(&db), config.clone()));

    // Create initial channel for testing if none exist in DB
    if db.get_all_channels()?.is_empty() {
        let channel_id = 1u32;
        let name = "Lounge";
        let comment = "Default voice lounge";
        let icon_type = 1; // Emoji
        let icon_data = "🛋️".as_bytes();
        
        // Persist to DB
        db.upsert_channel(Some(channel_id), name, comment, icon_type, icon_data, 0)?;
        
        // Initialize in memory
        state.create_channel(channel_id);
        state.channel_metadata.insert(channel_id, state::ChannelMetadata {
            id: channel_id,
            name: name.to_string(),
            comment: comment.to_string(),
            icon_type,
            icon_data: icon_data.to_vec(),
            position: 0,
        });
        
        info!("Created default channel '{}' (ID {})", name, channel_id);
    }

    // Log user count
    let user_count = db.user_count()?;
    info!("Database contains {} registered users", user_count);

    // Parse bind address
    let bind_addr: std::net::SocketAddr = config.server.bind_address.parse()?;
    
    // Create and run QUIC server
    let quic_server = QuicServer::new(bind_addr, Arc::clone(&state))?;
    
    info!("Server Ready.");
    info!("");
    info!("To create a bootstrap admin on first run:");
    info!("  export {}=<64-char-hex-public-key>", ENV_BOOTSTRAP_ADMIN_KEY);
    info!("  export {}=<admin-display-name>", ENV_BOOTSTRAP_ADMIN_NAME);

    // Run server (this blocks until shutdown)
    tokio::select! {
        result = quic_server.run() => {
            if let Err(e) = result {
                error!("Server error: {}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Shutting down...");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VerificationMode;
    use aura_protocol::{FastAudioPacket, NONCE_SIZE};
    use bytes::{Bytes, BytesMut};

    #[test]
    fn test_fast_audio_packet_with_epoch_hint() {
        let nonce = [0x42u8; NONCE_SIZE];

        let mut buf = BytesMut::new();
        let packet = FastAudioPacket {
            session_id: 0xDEADBEEF,
            epoch_hint: 42,
            sequence: 100,
            nonce,
            payload: Bytes::from_static(b"EncryptedOpus"),
        };
        packet.write(&mut buf);

        assert_eq!(FastAudioPacket::HEADER_SIZE, 32);

        let parsed = FastAudioPacket::parse(buf.freeze()).expect("Failed to parse");
        assert_eq!(parsed.session_id, 0xDEADBEEF);
        assert_eq!(parsed.epoch_hint, 42);
        assert_eq!(parsed.sequence, 100);
        assert_eq!(parsed.nonce, nonce);
        assert_eq!(parsed.payload, Bytes::from_static(b"EncryptedOpus"));
    }

    #[test]
    fn test_config_loading() {
        let config = Config::default();
        assert_eq!(config.server.bind_address, "0.0.0.0:8443");
        assert!(config.server.password.is_none());
        assert_eq!(config.verification.mode, VerificationMode::Optional);
    }

    #[test]
    fn test_database_persistence() {
        let db = Database::open_in_memory().unwrap();

        // Create a user
        let key = [0x42u8; 32];
        let user_uuid = db.create_user(&key, "TestUser").unwrap();

        // User should exist
        let user = db.find_user_by_uuid(&user_uuid).unwrap().unwrap();
        assert_eq!(user.display_name, "TestUser");
        assert!(!user.verified);

        // Verify user
        db.set_user_verified(&user_uuid, true).unwrap();
        let user = db.find_user_by_uuid(&user_uuid).unwrap().unwrap();
        assert!(user.verified);
    }

    #[test]
    fn test_username_tofu() {
        let db = Database::open_in_memory().unwrap();

        // First user claims "Alice"
        let key1 = [0x01u8; 32];
        db.create_user(&key1, "Alice").unwrap();

        // Second user tries to claim "Alice" - should fail
        let key2 = [0x02u8; 32];
        let result = db.create_user(&key2, "Alice");
        assert!(result.is_err());

        // Case-insensitive check
        let result = db.create_user(&key2, "alice");
        assert!(result.is_err());

        // Different name works
        db.create_user(&key2, "Bob").unwrap();
    }
}
