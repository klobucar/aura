//! Aura Server - Zero-Trust Voice/Text Relay
//!
//! This is the main entry point for the Aura server with:
//! - TOFU (Trust On First Use) authentication
//! - SQLite persistence for user identities
//! - Optional server password for access control
//! - Admin system for user verification and moderation
//! - QUIC transport for low-latency voice

use aura_server::config::Config;
use aura_server::connection::QuicServer;
use aura_server::db::{AdminPermissions, Database, User};
use aura_server::state::{self, ServerState};

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::EnvFilter;

/// Default configuration file path.
const DEFAULT_CONFIG_PATH: &str = "server.toml";

/// Parsed CLI arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CliArgs {
    config_path: String,
    config_explicit: bool,
    bind_address: Option<String>,
    action: CliAction,
}

/// Action requested on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CliAction {
    RunServer,
    Help,
    Version,
    PromoteAdmin(String),
    Unban(String),
    ListUsers,
}

fn parse_cli_args<I, S>(args: I) -> Result<CliArgs>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut config_path = DEFAULT_CONFIG_PATH.to_string();
    let mut config_explicit = false;
    let mut bind_address: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        let s = arg.as_ref();
        match s {
            "-h" | "--help" => {
                return Ok(CliArgs {
                    config_path,
                    config_explicit,
                    bind_address,
                    action: CliAction::Help,
                });
            }
            "-V" | "--version" => {
                return Ok(CliArgs {
                    config_path,
                    config_explicit,
                    bind_address,
                    action: CliAction::Version,
                });
            }
            "--config" => {
                let path = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--config requires a path argument"))?;
                config_path = path.as_ref().to_string();
                config_explicit = true;
            }
            _ if s.starts_with("--config=") => {
                let path = &s["--config=".len()..];
                if path.is_empty() {
                    return Err(anyhow::anyhow!("--config= requires a non-empty path"));
                }
                config_path = path.to_string();
                config_explicit = true;
            }
            "--bind" => {
                let addr = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--bind requires an address argument"))?;
                if addr.as_ref().is_empty() {
                    return Err(anyhow::anyhow!("--bind requires a non-empty address"));
                }
                bind_address = Some(addr.as_ref().to_string());
            }
            _ if s.starts_with("--bind=") => {
                let addr = &s["--bind=".len()..];
                if addr.is_empty() {
                    return Err(anyhow::anyhow!("--bind= requires a non-empty address"));
                }
                bind_address = Some(addr.to_string());
            }
            _ if s.starts_with('-') => {
                return Err(anyhow::anyhow!("Unknown option: {s}"));
            }
            _ => positionals.push(s.to_string()),
        }
    }

    let action = match positionals.as_slice() {
        [] => CliAction::RunServer,
        [cmd] if cmd == "list-users" => CliAction::ListUsers,
        [cmd, target] if cmd == "promote-admin" => CliAction::PromoteAdmin(target.clone()),
        [cmd, target] if cmd == "unban" => CliAction::Unban(target.clone()),
        [cmd] if cmd == "promote-admin" || cmd == "unban" => {
            return Err(anyhow::anyhow!(
                "Subcommand '{cmd}' requires <user_uuid_or_name>"
            ));
        }
        [cmd, ..] => {
            return Err(anyhow::anyhow!(
                "Unknown subcommand or unexpected arguments: '{cmd}' (use --help for usage)"
            ));
        }
    };

    Ok(CliArgs {
        config_path,
        config_explicit,
        bind_address,
        action,
    })
}

fn print_help() {
    println!(
        "aura-server {}\n\n\
         USAGE:\n\
         \taura-server [OPTIONS] [SUBCOMMAND]\n\n\
         OPTIONS:\n\
         \t--config <PATH>    Path to config file (default: server.toml)\n\
         \t--bind <ADDR>      Override bind address from config (e.g. 0.0.0.0:8443)\n\
         \t-h, --help         Print help information\n\
         \t-V, --version      Print version information\n\n\
         SUBCOMMANDS:\n\
         \tpromote-admin <USER_UUID_OR_NAME>    Grant full admin permissions to a user\n\
         \tunban <USER_UUID_OR_NAME>            Remove a ban from a user\n\
         \tlist-users                           List all registered users\n\n\
         NOTE:\n\
         \tOffline promote-admin and unban changes require a server restart if aura-server is running.",
        env!("CARGO_PKG_VERSION")
    );
}

fn parse_level_name(s: &str) -> Option<LevelFilter> {
    match s.trim().to_ascii_lowercase().as_str() {
        "trace" => Some(LevelFilter::TRACE),
        "debug" => Some(LevelFilter::DEBUG),
        "info" => Some(LevelFilter::INFO),
        "warn" | "warning" => Some(LevelFilter::WARN),
        "error" => Some(LevelFilter::ERROR),
        "off" => Some(LevelFilter::OFF),
        _ => None,
    }
}

/// Resolve the effective log level from `RUST_LOG` (if set) or `config.server.log_level`.
fn resolve_log_level(config_level: &str, rust_log_env: Option<&str>) -> LevelFilter {
    if let Some(env_val) = rust_log_env.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(level) = parse_level_name(env_val) {
            return level;
        }
        // Support simple `target=level` / comma-separated RUST_LOG directives by
        // picking the most verbose bare or targeted level specified.
        let mut parsed: Option<LevelFilter> = None;
        for part in env_val.split(',') {
            let level_str = part.split('=').next_back().unwrap_or(part);
            if let Some(lvl) = parse_level_name(level_str) {
                parsed = Some(match parsed {
                    Some(prev) if prev > lvl => prev,
                    _ => lvl,
                });
            }
        }
        if let Some(level) = parsed {
            return level;
        }
    }

    parse_level_name(config_level).unwrap_or(LevelFilter::INFO)
}

fn resolve_user(db: &Database, uuid_or_name: &str) -> Result<User> {
    if let Some(user) = db.find_user_by_uuid(uuid_or_name)? {
        return Ok(user);
    }
    if let Some(user) = db.find_user_by_name(uuid_or_name)? {
        return Ok(user);
    }
    Err(anyhow::anyhow!("User not found: '{uuid_or_name}'"))
}

fn promote_admin_in_db(db: &Database, uuid_or_name: &str) -> Result<User> {
    let user = resolve_user(db, uuid_or_name)?;
    db.grant_admin(&user.user_uuid, &AdminPermissions::full(), None)?;
    Ok(user)
}

fn open_existing_db(path: &Path) -> Result<Database> {
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "Database file not found: {}",
            path.display()
        ));
    }
    Database::open(path)
}

fn unban_user_in_db(db: &Database, uuid_or_name: &str) -> Result<User> {
    let user = resolve_user(db, uuid_or_name)?;
    db.set_user_banned(&user.user_uuid, false)?;
    Ok(user)
}

fn list_users_in_db(db: &Database) -> Result<Vec<(User, bool)>> {
    let admins = db.admin_user_uuids()?;
    let mut all = Vec::new();
    let mut offset = 0u32;
    const PAGE_SIZE: u32 = 1000;
    loop {
        let page = db.list_users(offset, PAGE_SIZE)?;
        let count = page.len() as u32;
        for user in page {
            let is_admin = admins.contains(&user.user_uuid);
            all.push((user, is_admin));
        }
        if count < PAGE_SIZE {
            break;
        }
        offset = offset.saturating_add(count);
    }
    Ok(all)
}

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
            info!(
                "No bootstrap admin configured. Set {} to create first admin.",
                ENV_BOOTSTRAP_ADMIN_KEY
            );
            return Ok(());
        }
    };

    // Parse hex public key
    let key_bytes = hex::decode(&key_hex)
        .map_err(|e| anyhow::anyhow!("Invalid bootstrap admin key (not valid hex): {}", e))?;

    if key_bytes.len() != 32 {
        return Err(anyhow::anyhow!(
            "Bootstrap admin key must be 32 bytes (64 hex chars), got {}",
            key_bytes.len()
        ));
    }

    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(&key_bytes);

    // Get display name (default to "Admin")
    let display_name =
        std::env::var(ENV_BOOTSTRAP_ADMIN_NAME).unwrap_or_else(|_| "Admin".to_string());

    // Create bootstrap admin
    db.create_bootstrap_admin(&public_key, &display_name)?;
    info!(
        "Created bootstrap admin '{}' with key {}...",
        display_name,
        &key_hex[..16]
    );

    Ok(())
}

fn load_cli_config(cli: &CliArgs) -> Result<Config> {
    let mut config = if cli.config_explicit {
        Config::load_with_env_strict(&cli.config_path)?
    } else {
        Config::load_with_env(&cli.config_path)?
    };
    if let Some(ref addr) = cli.bind_address {
        config.server.bind_address = addr.clone();
    }
    Ok(config)
}

fn build_env_filter(config_level: &str) -> EnvFilter {
    if let Ok(env_val) = std::env::var("RUST_LOG") {
        if !env_val.trim().is_empty() {
            if let Ok(filter) = EnvFilter::try_from_default_env() {
                return filter;
            }
        }
    }
    let level = parse_level_name(config_level).unwrap_or(LevelFilter::INFO);
    EnvFilter::new(level.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = parse_cli_args(std::env::args().skip(1))?;
    match &cli.action {
        CliAction::Help => {
            print_help();
            return Ok(());
        }
        CliAction::Version => {
            println!("aura-server {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        CliAction::PromoteAdmin(_) | CliAction::Unban(_) | CliAction::ListUsers => {
            let config = load_cli_config(&cli)?;
            match cli.action {
                CliAction::PromoteAdmin(target) => {
                    let db = Database::open(&config.database.path)?;
                    let user = promote_admin_in_db(&db, &target)?;
                    println!(
                        "Promoted '{}' ({}) to admin.",
                        user.display_name, user.user_uuid
                    );
                }
                CliAction::Unban(target) => {
                    let db = Database::open(&config.database.path)?;
                    let user = unban_user_in_db(&db, &target)?;
                    println!("Unbanned '{}' ({}).", user.display_name, user.user_uuid);
                }
                CliAction::ListUsers => {
                    let db = open_existing_db(&config.database.path)?;
                    let users = list_users_in_db(&db)?;
                    if users.is_empty() {
                        println!("No users registered.");
                    } else {
                        for (user, is_admin) in users {
                            println!(
                                "{}\t{}\tadmin={}\tverified={}\tbanned={}",
                                user.user_uuid,
                                user.display_name.escape_default(),
                                is_admin,
                                user.verified,
                                user.banned
                            );
                        }
                    }
                }
                _ => unreachable!(),
            }
            return Ok(());
        }
        CliAction::RunServer => {}
    }

    // Load configuration before initializing logging so `server.log_level` is honored.
    let config = load_cli_config(&cli)?;
    let rust_log = std::env::var("RUST_LOG").ok();
    let log_level = resolve_log_level(&config.server.log_level, rust_log.as_deref());
    let env_filter = build_env_filter(&config.server.log_level);

    // Process-wide rustls provider (aws-lc-rs, hybrid PQ key exchange first).
    // The QUIC listener pins it explicitly; this covers implicit users such as
    // the ACME client.
    aura_server::tls::install_default_provider();

    // Logging, plus OpenTelemetry tracing when an OTLP endpoint is configured
    // (OTEL_EXPORTER_OTLP_ENDPOINT). Only Aura's own spans are exported, never
    // log lines; see docs/OBSERVABILITY.md. Kept alive until main returns so
    // buffered spans are flushed.
    let telemetry = aura_telemetry::Config::from_env("aura-server", env!("CARGO_PKG_VERSION"))
        .map(aura_telemetry::Telemetry::start)
        .transpose()?;
    {
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;
        use tracing_subscriber::Layer as _;
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_filter(env_filter))
            .with(telemetry.as_ref().map(|t| t.layer()))
            .init();
    }
    info!("Starting Aura Zero-Trust Relay...");
    if telemetry.is_some() {
        info!("OpenTelemetry tracing enabled (OTLP/HTTP)");
    }

    info!("Loaded configuration ({}):", cli.config_path);
    info!("  Bind address: {}", config.server.bind_address);
    info!("  Max connections: {}", config.server.max_connections);
    info!(
        "  Log level: {} (effective: {})",
        config.server.log_level, log_level
    );
    info!("  Database: {:?}", config.database.path);
    info!("  Verification mode: {:?}", config.verification.mode);
    info!(
        "  Server password: {}",
        if config.requires_password() {
            "enabled"
        } else {
            "disabled"
        }
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

    // Sweep expired replay-protection entries so the set stays bounded.
    Arc::clone(&state.seen_messages).spawn_cleanup(state::SEEN_MESSAGE_SWEEP_INTERVAL);

    // Forward secrecy for long-lived voice groups: ask founders to ratchet
    // groups that have been on one epoch too long.
    let ratchet_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(state::VOICE_RATCHET_CHECK_INTERVAL);
        interval.tick().await; // the first tick is immediate; skip it
        loop {
            interval.tick().await;
            ratchet_state.request_due_voice_ratchets().await;
        }
    });

    // Prometheus metrics (aggregate counts only), when AURA_METRICS_ADDR is set.
    if let Some(addr) = config.server.metrics_address {
        let metrics_state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = aura_server::metrics::serve(addr, metrics_state).await {
                error!("Metrics endpoint on {} failed: {}", addr, e);
            }
        });
        info!("Serving Prometheus metrics on http://{}/metrics", addr);
    }

    // Create initial channel for testing if none exist in DB
    if db.get_all_channels()?.is_empty() {
        let channel_id = format!("C_{}", uuid::Uuid::new_v4());
        let name = "Lounge";
        let comment = "Default voice lounge";
        let icon_type = 1; // Emoji
        let icon_data = "🛋️".as_bytes();

        // Persist to DB
        db.upsert_channel(
            Some(channel_id.clone()),
            name,
            comment,
            icon_type,
            icon_data,
            0,
            1,
        )?; // 1 = Lobby

        // Initialize in memory
        state.create_channel(channel_id.clone());
        state.channel_metadata.insert(
            channel_id.clone(),
            state::ChannelMetadata {
                id: channel_id.clone(),
                name: name.to_string(),
                comment: comment.to_string(),
                icon_type,
                icon_data: icon_data.to_vec(),
                position: 0,
                channel_type: 1, // Lobby
            },
        );

        info!("Created default channel '{}' (ID {})", name, channel_id);
    }

    // Log user count
    let user_count = db.user_count()?;
    info!("Database contains {} registered users", user_count);

    // Resolve bind address (may be a hostname, e.g. fly-global-services:8443)
    let bind_addr = config.resolve_bind_address().await?;

    // Create and run QUIC server
    let quic_server = QuicServer::new(bind_addr, Arc::clone(&state))?;

    info!("Server Ready.");
    info!("");
    info!("To create a bootstrap admin on first run:");
    info!(
        "  export {}=<64-char-hex-public-key>",
        ENV_BOOTSTRAP_ADMIN_KEY
    );
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
    use aura_protocol::{FastAudioPacket, NONCE_SIZE};
    use aura_server::config::VerificationMode;
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
        let user_uuid = db.create_user(&key, None, "TestUser").unwrap();

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
        db.create_user(&key1, None, "Alice").unwrap();

        // Second user tries to claim "Alice" - should fail
        let key2 = [0x02u8; 32];
        let result = db.create_user(&key2, None, "Alice");
        assert!(result.is_err());

        // Case-insensitive check
        let result = db.create_user(&key2, None, "alice");
        assert!(result.is_err());

        // Different name works
        db.create_user(&key2, None, "Bob").unwrap();
    }

    #[test]
    fn test_parse_cli_args_defaults_and_flags() {
        let parsed = parse_cli_args::<[&str; 0], &str>([]).unwrap();
        assert_eq!(parsed.config_path, "server.toml");
        assert!(!parsed.config_explicit);
        assert_eq!(parsed.bind_address, None);
        assert_eq!(parsed.action, CliAction::RunServer);

        let parsed = parse_cli_args(["--config", "/etc/aura/server.toml"]).unwrap();
        assert_eq!(parsed.config_path, "/etc/aura/server.toml");
        assert!(parsed.config_explicit);
        assert_eq!(parsed.action, CliAction::RunServer);

        let parsed = parse_cli_args(["--config=custom.toml", "--help"]).unwrap();
        assert_eq!(parsed.config_path, "custom.toml");
        assert!(parsed.config_explicit);
        assert_eq!(parsed.action, CliAction::Help);

        let parsed = parse_cli_args(["--bind", "0.0.0.0:8443"]).unwrap();
        assert_eq!(parsed.bind_address.as_deref(), Some("0.0.0.0:8443"));
        assert_eq!(parsed.action, CliAction::RunServer);

        let parsed = parse_cli_args(["--bind=127.0.0.1:9443"]).unwrap();
        assert_eq!(parsed.bind_address.as_deref(), Some("127.0.0.1:9443"));

        let parsed = parse_cli_args(["-V"]).unwrap();
        assert_eq!(parsed.action, CliAction::Version);
    }

    #[test]
    fn test_parse_cli_args_subcommands_and_errors() {
        let parsed = parse_cli_args(["--config", "alt.toml", "promote-admin", "Alice"]).unwrap();
        assert_eq!(parsed.config_path, "alt.toml");
        assert!(parsed.config_explicit);
        assert_eq!(parsed.action, CliAction::PromoteAdmin("Alice".to_string()));

        let parsed = parse_cli_args(["unban", "user-123"]).unwrap();
        assert_eq!(parsed.action, CliAction::Unban("user-123".to_string()));

        let parsed = parse_cli_args(["list-users"]).unwrap();
        assert_eq!(parsed.action, CliAction::ListUsers);

        assert!(parse_cli_args(["--config"]).is_err());
        assert!(parse_cli_args(["--config="]).is_err());
        assert!(parse_cli_args(["--bind"]).is_err());
        assert!(parse_cli_args(["--bind="]).is_err());
        assert!(parse_cli_args(["--bogus"]).is_err());
        assert!(parse_cli_args(["promote-admin"]).is_err());
        assert!(parse_cli_args(["unban"]).is_err());
        assert!(parse_cli_args(["unknown-cmd"]).is_err());
    }

    #[test]
    fn test_load_cli_config_bind_override_and_strict_config() {
        let cli = parse_cli_args(["--bind", "127.0.0.1:9999"]).unwrap();
        let config = load_cli_config(&cli).unwrap();
        assert_eq!(config.server.bind_address, "127.0.0.1:9999");

        let cli_missing =
            parse_cli_args(["--config", "/tmp/aura_missing_cli_cfg_98765.toml"]).unwrap();
        assert!(load_cli_config(&cli_missing).is_err());

        assert!(open_existing_db(Path::new("/tmp/aura_missing_db_98765.db")).is_err());
    }

    #[test]
    fn test_resolve_log_level_config_and_rust_log_override() {
        assert_eq!(resolve_log_level("trace", None), LevelFilter::TRACE);
        assert_eq!(resolve_log_level("DEBUG", None), LevelFilter::DEBUG);
        assert_eq!(resolve_log_level("warn", None), LevelFilter::WARN);
        assert_eq!(resolve_log_level("error", None), LevelFilter::ERROR);
        assert_eq!(resolve_log_level("invalid", None), LevelFilter::INFO);

        // RUST_LOG overrides config
        assert_eq!(resolve_log_level("info", Some("debug")), LevelFilter::DEBUG);
        assert_eq!(
            resolve_log_level("error", Some("aura_server=trace,warn")),
            LevelFilter::TRACE
        );
        // Empty RUST_LOG falls back to config
        assert_eq!(resolve_log_level("warn", Some("  ")), LevelFilter::WARN);
    }

    #[test]
    fn test_db_admin_helpers() {
        let db = Database::open_in_memory().unwrap();
        let alice_uuid = db.create_user(&[0x11u8; 32], None, "Alice").unwrap();
        let bob_uuid = db.create_user(&[0x22u8; 32], None, "Bob").unwrap();
        db.set_user_banned(&bob_uuid, true).unwrap();

        // Promote by display name
        let promoted = promote_admin_in_db(&db, "Alice").unwrap();
        assert_eq!(promoted.user_uuid, alice_uuid);
        assert!(db.is_admin(&alice_uuid).unwrap());

        // Unban by UUID
        let unbanned = unban_user_in_db(&db, &bob_uuid).unwrap();
        assert_eq!(unbanned.user_uuid, bob_uuid);
        assert!(!db.find_user_by_uuid(&bob_uuid).unwrap().unwrap().banned);

        // List users includes admin flag
        let listed = list_users_in_db(&db).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .any(|(u, admin)| u.user_uuid == alice_uuid && *admin));
        assert!(listed
            .iter()
            .any(|(u, admin)| u.user_uuid == bob_uuid && !*admin));

        // Missing user errors cleanly
        assert!(promote_admin_in_db(&db, "Nobody").is_err());
        assert!(unban_user_in_db(&db, "Nobody").is_err());
    }
}
