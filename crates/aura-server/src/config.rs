//! Configuration loading and parsing for the Aura server.
//!
//! Loads configuration from `server.toml` with sensible defaults.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Root configuration structure for the Aura server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub verification: VerificationConfig,
}

/// Server network and general settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    /// Address to bind the server to (e.g., "0.0.0.0:8443")
    pub bind_address: String,

    /// Maximum number of concurrent connections
    pub max_connections: usize,

    /// Logging level (trace, debug, info, warn, error)
    pub log_level: String,

    /// Optional server password for access control.
    /// If set, clients must provide this password to connect.
    #[serde(default)]
    pub password: Option<String>,

    /// Optional path to a TLS certificate in PEM format.
    #[serde(default)]
    pub cert_path: Option<PathBuf>,

    /// Optional path to a TLS private key in PEM format.
    #[serde(default)]
    pub key_path: Option<PathBuf>,

    /// Optional domain for automated Let's Encrypt (ACME) certificates.
    #[serde(default)]
    pub acme_domain: Option<String>,

    /// Optional email contact for ACME registration.
    #[serde(default)]
    pub acme_contact: Option<String>,

    /// Optional path for the ACME certificate cache.
    #[serde(default)]
    pub acme_cache_path: Option<PathBuf>,

    /// Optional custom ACME directory URL (e.g., for Let's Encrypt staging or Pebble).
    #[serde(default)]
    pub acme_directory_url: Option<String>,

    /// Optional bind port for ACME ALPN challenges (defaults to 443).
    /// Useful for running as non-root behind a proxy (e.g., Fly.io).
    #[serde(default)]
    pub acme_bind_port: Option<u16>,

    /// Maximum new handshake attempts permitted per source IP per minute.
    #[serde(default = "default_handshake_per_minute")]
    pub handshake_per_minute: u32,

    /// Instantaneous burst capacity for the per-IP handshake limiter.
    #[serde(default = "default_handshake_burst")]
    pub handshake_burst: u32,
}

fn default_handshake_per_minute() -> u32 {
    60
}

fn default_handshake_burst() -> u32 {
    20
}

/// Database configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DatabaseConfig {
    /// Path to the SQLite database file
    pub path: PathBuf,
}

/// Verification policy configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VerificationConfig {
    /// The verification mode for users
    pub mode: VerificationMode,
}

/// Determines how user verification is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationMode {
    /// No verification required - anyone can join
    None,
    /// Users can be verified by admins (cosmetic badge only)
    Optional,
    /// Only verified users can join voice channels
    Required,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                bind_address: "0.0.0.0:8443".to_string(),
                max_connections: 1000,
                log_level: "info".to_string(),
                password: None,
                cert_path: None,
                key_path: None,
                acme_domain: None,
                acme_contact: None,
                acme_cache_path: Some(PathBuf::from("data/acme")),
                acme_directory_url: None,
                acme_bind_port: Some(443),
                handshake_per_minute: default_handshake_per_minute(),
                handshake_burst: default_handshake_burst(),
            },
            database: DatabaseConfig {
                path: PathBuf::from("aura.db"),
            },
            verification: VerificationConfig {
                mode: VerificationMode::Optional,
            },
        }
    }
}

impl Config {
    /// Load configuration from `server.toml`, falling back to defaults.
    ///
    /// If the file doesn't exist, returns default configuration.
    /// If the file exists but is invalid, returns an error.
    /// Load configuration from `server.toml` with environment variable overrides.
    pub fn load() -> anyhow::Result<Self> {
        let mut config = Self::load_from("server.toml")?;

        // Environment variable overrides
        if let Ok(addr) = std::env::var("AURA_BIND_ADDRESS") {
            config.server.bind_address = addr;
        }
        if let Ok(db_path) = std::env::var("AURA_DATABASE_PATH") {
            config.database.path = PathBuf::from(db_path);
        }
        if let Ok(password) = std::env::var("AURA_PASSWORD") {
            config.server.password = Some(password);
        }
        if let Ok(cert_path) = std::env::var("AURA_CERT_PATH") {
            config.server.cert_path = Some(PathBuf::from(cert_path));
        }
        if let Ok(key_path) = std::env::var("AURA_KEY_PATH") {
            config.server.key_path = Some(PathBuf::from(key_path));
        }
        if let Ok(domain) = std::env::var("AURA_ACME_DOMAIN") {
            config.server.acme_domain = Some(domain);
        }
        if let Ok(contact) = std::env::var("AURA_ACME_CONTACT") {
            config.server.acme_contact = Some(contact);
        }
        if let Ok(cache_path) = std::env::var("AURA_ACME_CACHE_PATH") {
            config.server.acme_cache_path = Some(PathBuf::from(cache_path));
        }
        if let Ok(dir_url) = std::env::var("AURA_ACME_DIRECTORY_URL") {
            config.server.acme_directory_url = Some(dir_url);
        }
        if let Ok(port_str) = std::env::var("AURA_ACME_BIND_PORT") {
            if let Ok(port) = port_str.parse() {
                config.server.acme_bind_port = Some(port);
            }
        }
        if let Ok(v) = std::env::var("AURA_HANDSHAKE_PER_MINUTE") {
            if let Ok(n) = v.parse() {
                config.server.handshake_per_minute = n;
            }
        }
        if let Ok(v) = std::env::var("AURA_HANDSHAKE_BURST") {
            if let Ok(n) = v.parse() {
                config.server.handshake_burst = n;
            }
        }

        Ok(config)
    }

    /// Load configuration from a specific path.
    pub fn load_from(path: &str) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(config_str) => {
                let config: Config = toml::from_str(&config_str)?;
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("No config file found at {}, using defaults", path);
                Ok(Self::default())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Whether a server password is required to connect.
    pub fn requires_password(&self) -> bool {
        self.server.password.is_some()
    }

    /// Validate the provided password against the configured password.
    /// Returns true if no password is required or if the password matches.
    pub fn validate_password(&self, provided: Option<&str>) -> bool {
        match (&self.server.password, provided) {
            (None, _) => true, // No password required
            (Some(expected), Some(provided)) => expected == provided,
            (Some(_), None) => false, // Password required but not provided
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.bind_address, "0.0.0.0:8443");
        assert_eq!(config.server.max_connections, 1000);
        assert!(config.server.password.is_none());
        assert_eq!(config.verification.mode, VerificationMode::Optional);
    }

    #[test]
    fn test_password_validation() {
        let mut config = Config::default();

        // No password required
        assert!(config.validate_password(None));
        assert!(config.validate_password(Some("anything")));

        // Password required
        config.server.password = Some("secret".to_string());
        assert!(!config.validate_password(None));
        assert!(!config.validate_password(Some("wrong")));
        assert!(config.validate_password(Some("secret")));
    }

    #[test]
    fn test_parse_toml() {
        let toml_str = r#"
[server]
bind_address = "127.0.0.1:9000"
max_connections = 500
log_level = "debug"
password = "test-password"

[database]
path = "test.db"

[verification]
mode = "required"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.server.bind_address, "127.0.0.1:9000");
        assert_eq!(config.server.max_connections, 500);
        assert_eq!(config.server.password, Some("test-password".to_string()));
        assert_eq!(config.verification.mode, VerificationMode::Required);
    }
}
