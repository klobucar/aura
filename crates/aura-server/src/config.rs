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
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from("server.toml")
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
