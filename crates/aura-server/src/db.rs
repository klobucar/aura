//! SQLite database module for persistent storage.
//!
//! Handles user identities, admins, TOFU key pinning, and session tracking.

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current database schema version.
const SCHEMA_VERSION: i64 = 1;

/// Thread-safe database handle.
pub type DbHandle = Arc<Mutex<Connection>>;

/// User record from the database.
#[derive(Debug, Clone)]
pub struct User {
    pub user_id: u32,
    pub ed25519_public_key: [u8; 32],
    pub display_name: String,
    pub created_at: i64,
    pub verified: bool,
    pub banned: bool,
}

/// Admin permissions stored as JSON.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdminPermissions {
    pub verify_users: bool,
    pub ban_users: bool,
    pub grant_admin: bool,
}

impl AdminPermissions {
    /// Full permissions for bootstrap admin.
    pub fn full() -> Self {
        Self {
            verify_users: true,
            ban_users: true,
            grant_admin: true,
        }
    }
}

/// Admin record from the database.
#[derive(Debug, Clone)]
pub struct Admin {
    pub user_id: u32,
    pub permissions: AdminPermissions,
    pub granted_at: i64,
    pub granted_by: Option<u32>,
}

/// Database operations wrapper.
pub struct Database {
    conn: DbHandle,
}

impl Database {
    /// Open or create the database at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.initialize()?;
        Ok(db)
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.initialize()?;
        Ok(db)
    }

    /// Get a clone of the database handle for sharing.
    pub fn handle(&self) -> DbHandle {
        Arc::clone(&self.conn)
    }

    /// Initialize the database schema.
    fn initialize(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        // Enable foreign keys
        conn.execute("PRAGMA foreign_keys = ON", [])?;

        // Check schema version
        let version: i64 = conn
            .query_row(
                "SELECT version FROM schema_version LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if version == 0 {
            // Fresh database, create all tables
            self.create_tables(&conn)?;
        } else if version < SCHEMA_VERSION {
            // Run migrations
            self.migrate(&conn, version)?;
        }

        Ok(())
    }

    /// Create all database tables.
    fn create_tables(&self, conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            -- Schema version tracking
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
            );
            INSERT OR REPLACE INTO schema_version (version) VALUES (1);

            -- Users table with TOFU key pinning
            CREATE TABLE IF NOT EXISTS users (
                user_id INTEGER PRIMARY KEY AUTOINCREMENT,
                ed25519_public_key BLOB NOT NULL UNIQUE,
                display_name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                created_at INTEGER NOT NULL,
                verified BOOLEAN NOT NULL DEFAULT 0,
                banned BOOLEAN NOT NULL DEFAULT 0
            );

            -- Admins table
            CREATE TABLE IF NOT EXISTS admins (
                user_id INTEGER PRIMARY KEY,
                permissions TEXT NOT NULL,
                granted_at INTEGER NOT NULL,
                granted_by INTEGER,
                FOREIGN KEY (user_id) REFERENCES users(user_id)
            );

            -- TOFU identity tracking
            CREATE TABLE IF NOT EXISTS tofu_identities (
                user_id INTEGER PRIMARY KEY,
                first_seen_key BLOB NOT NULL,
                first_seen_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(user_id)
            );

            -- Indices for fast lookups
            CREATE INDEX IF NOT EXISTS idx_users_public_key ON users(ed25519_public_key);
            CREATE INDEX IF NOT EXISTS idx_users_display_name ON users(display_name);
            "#,
        )?;

        tracing::info!("Database schema created (version {})", SCHEMA_VERSION);
        Ok(())
    }

    /// Run database migrations from old version to current.
    fn migrate(&self, conn: &Connection, from_version: i64) -> Result<()> {
        tracing::info!(
            "Migrating database from version {} to {}",
            from_version,
            SCHEMA_VERSION
        );

        // Add migration steps here as schema evolves
        // Example:
        // if from_version < 2 {
        //     conn.execute("ALTER TABLE users ADD COLUMN avatar_url TEXT", [])?;
        // }

        conn.execute(
            "UPDATE schema_version SET version = ?",
            params![SCHEMA_VERSION],
        )?;

        Ok(())
    }

    // =========================================================================
    // User Operations
    // =========================================================================

    /// Get the current Unix timestamp.
    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// Find a user by their Ed25519 public key.
    pub fn find_user_by_key(&self, public_key: &[u8; 32]) -> Result<Option<User>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT user_id, ed25519_public_key, display_name, created_at, verified, banned
             FROM users WHERE ed25519_public_key = ?",
            params![public_key.as_slice()],
            |row| {
                let key_blob: Vec<u8> = row.get(1)?;
                let mut key = [0u8; 32];
                key.copy_from_slice(&key_blob);
                Ok(User {
                    user_id: row.get(0)?,
                    ed25519_public_key: key,
                    display_name: row.get(2)?,
                    created_at: row.get(3)?,
                    verified: row.get(4)?,
                    banned: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Find a user by their display name (case-insensitive).
    pub fn find_user_by_name(&self, display_name: &str) -> Result<Option<User>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT user_id, ed25519_public_key, display_name, created_at, verified, banned
             FROM users WHERE display_name = ? COLLATE NOCASE",
            params![display_name],
            |row| {
                let key_blob: Vec<u8> = row.get(1)?;
                let mut key = [0u8; 32];
                key.copy_from_slice(&key_blob);
                Ok(User {
                    user_id: row.get(0)?,
                    ed25519_public_key: key,
                    display_name: row.get(2)?,
                    created_at: row.get(3)?,
                    verified: row.get(4)?,
                    banned: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Find a user by their ID.
    pub fn find_user_by_id(&self, user_id: u32) -> Result<Option<User>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT user_id, ed25519_public_key, display_name, created_at, verified, banned
             FROM users WHERE user_id = ?",
            params![user_id],
            |row| {
                let key_blob: Vec<u8> = row.get(1)?;
                let mut key = [0u8; 32];
                key.copy_from_slice(&key_blob);
                Ok(User {
                    user_id: row.get(0)?,
                    ed25519_public_key: key,
                    display_name: row.get(2)?,
                    created_at: row.get(3)?,
                    verified: row.get(4)?,
                    banned: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Create a new user with TOFU identity.
    /// Returns the new user_id on success.
    pub fn create_user(&self, public_key: &[u8; 32], display_name: &str) -> Result<u32> {
        let conn = self.conn.lock().unwrap();
        let now = Self::now();

        // Insert user
        conn.execute(
            "INSERT INTO users (ed25519_public_key, display_name, created_at, verified, banned)
             VALUES (?, ?, ?, 0, 0)",
            params![public_key.as_slice(), display_name, now],
        )?;

        let user_id = conn.last_insert_rowid() as u32;

        // Record TOFU identity
        conn.execute(
            "INSERT INTO tofu_identities (user_id, first_seen_key, first_seen_at, last_seen_at)
             VALUES (?, ?, ?, ?)",
            params![user_id, public_key.as_slice(), now, now],
        )?;

        tracing::info!(
            "Created new user: id={}, name={}",
            user_id,
            display_name
        );
        Ok(user_id)
    }

    /// Update the last_seen timestamp for a user's TOFU identity.
    pub fn update_last_seen(&self, user_id: u32) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE tofu_identities SET last_seen_at = ? WHERE user_id = ?",
            params![Self::now(), user_id],
        )?;
        Ok(())
    }

    /// Set a user's verified status.
    pub fn set_user_verified(&self, user_id: u32, verified: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET verified = ? WHERE user_id = ?",
            params![verified, user_id],
        )?;
        tracing::info!("User {} verified status set to {}", user_id, verified);
        Ok(())
    }

    /// Set a user's banned status.
    pub fn set_user_banned(&self, user_id: u32, banned: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET banned = ? WHERE user_id = ?",
            params![banned, user_id],
        )?;
        tracing::info!("User {} banned status set to {}", user_id, banned);
        Ok(())
    }

    /// List users with pagination.
    pub fn list_users(&self, offset: u32, limit: u32) -> Result<Vec<User>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_id, ed25519_public_key, display_name, created_at, verified, banned
             FROM users ORDER BY user_id LIMIT ? OFFSET ?",
        )?;

        let users = stmt
            .query_map(params![limit, offset], |row| {
                let key_blob: Vec<u8> = row.get(1)?;
                let mut key = [0u8; 32];
                key.copy_from_slice(&key_blob);
                Ok(User {
                    user_id: row.get(0)?,
                    ed25519_public_key: key,
                    display_name: row.get(2)?,
                    created_at: row.get(3)?,
                    verified: row.get(4)?,
                    banned: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(users)
    }

    /// Get total user count.
    pub fn user_count(&self) -> Result<u32> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(count as u32)
    }

    // =========================================================================
    // Admin Operations
    // =========================================================================

    /// Check if a user is an admin.
    pub fn is_admin(&self, user_id: u32) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM admins WHERE user_id = ?",
            params![user_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get admin permissions for a user.
    pub fn get_admin_permissions(&self, user_id: u32) -> Result<Option<AdminPermissions>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT permissions FROM admins WHERE user_id = ?",
            params![user_id],
            |row| {
                let json: String = row.get(0)?;
                Ok(serde_json::from_str(&json).unwrap_or_default())
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Grant admin status to a user.
    pub fn grant_admin(
        &self,
        user_id: u32,
        permissions: &AdminPermissions,
        granted_by: Option<u32>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let permissions_json = serde_json::to_string(permissions)?;

        conn.execute(
            "INSERT OR REPLACE INTO admins (user_id, permissions, granted_at, granted_by)
             VALUES (?, ?, ?, ?)",
            params![user_id, permissions_json, Self::now(), granted_by],
        )?;

        tracing::info!("Granted admin to user {}", user_id);
        Ok(())
    }

    /// Revoke admin status from a user.
    pub fn revoke_admin(&self, user_id: u32) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM admins WHERE user_id = ?", params![user_id])?;
        tracing::info!("Revoked admin from user {}", user_id);
        Ok(())
    }

    /// Check if db has any users (for bootstrap admin check).
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.user_count()? == 0)
    }

    /// Create bootstrap admin from public key.
    /// This is used for first-time server setup via environment variable.
    pub fn create_bootstrap_admin(&self, public_key: &[u8; 32], display_name: &str) -> Result<u32> {
        let user_id = self.create_user(public_key, display_name)?;
        self.grant_admin(user_id, &AdminPermissions::full(), None)?;
        self.set_user_verified(user_id, true)?;
        tracing::info!("Created bootstrap admin: id={}, name={}", user_id, display_name);
        Ok(user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_find_user() {
        let db = Database::open_in_memory().unwrap();
        let key = [0x42u8; 32];

        // Create user
        let user_id = db.create_user(&key, "Alice").unwrap();
        assert_eq!(user_id, 1);

        // Find by key
        let user = db.find_user_by_key(&key).unwrap().unwrap();
        assert_eq!(user.user_id, 1);
        assert_eq!(user.display_name, "Alice");
        assert!(!user.verified);
        assert!(!user.banned);

        // Find by name (case insensitive)
        let user = db.find_user_by_name("alice").unwrap().unwrap();
        assert_eq!(user.user_id, 1);
    }

    #[test]
    fn test_unique_username() {
        let db = Database::open_in_memory().unwrap();
        let key1 = [0x01u8; 32];
        let key2 = [0x02u8; 32];

        // First user claims "Alice"
        db.create_user(&key1, "Alice").unwrap();

        // Second user tries to claim same name
        let result = db.create_user(&key2, "Alice");
        assert!(result.is_err());

        // Second user can use different name
        db.create_user(&key2, "Bob").unwrap();
    }

    #[test]
    fn test_admin_operations() {
        let db = Database::open_in_memory().unwrap();
        let key = [0x42u8; 32];
        let user_id = db.create_user(&key, "Admin").unwrap();

        // Not admin initially
        assert!(!db.is_admin(user_id).unwrap());

        // Grant admin
        let perms = AdminPermissions {
            verify_users: true,
            ban_users: true,
            grant_admin: false,
        };
        db.grant_admin(user_id, &perms, None).unwrap();

        // Now is admin
        assert!(db.is_admin(user_id).unwrap());

        // Check permissions
        let stored_perms = db.get_admin_permissions(user_id).unwrap().unwrap();
        assert!(stored_perms.verify_users);
        assert!(stored_perms.ban_users);
        assert!(!stored_perms.grant_admin);

        // Revoke admin
        db.revoke_admin(user_id).unwrap();
        assert!(!db.is_admin(user_id).unwrap());
    }

    #[test]
    fn test_user_verification_and_ban() {
        let db = Database::open_in_memory().unwrap();
        let key = [0x42u8; 32];
        let user_id = db.create_user(&key, "TestUser").unwrap();

        // Not verified initially
        let user = db.find_user_by_id(user_id).unwrap().unwrap();
        assert!(!user.verified);

        // Verify
        db.set_user_verified(user_id, true).unwrap();
        let user = db.find_user_by_id(user_id).unwrap().unwrap();
        assert!(user.verified);

        // Ban
        db.set_user_banned(user_id, true).unwrap();
        let user = db.find_user_by_id(user_id).unwrap().unwrap();
        assert!(user.banned);
    }

    #[test]
    fn test_bootstrap_admin() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.is_empty().unwrap());

        let key = [0x42u8; 32];
        let user_id = db.create_bootstrap_admin(&key, "RootAdmin").unwrap();

        assert!(!db.is_empty().unwrap());
        assert!(db.is_admin(user_id).unwrap());

        let user = db.find_user_by_id(user_id).unwrap().unwrap();
        assert!(user.verified);

        let perms = db.get_admin_permissions(user_id).unwrap().unwrap();
        assert!(perms.verify_users);
        assert!(perms.ban_users);
        assert!(perms.grant_admin);
    }
}
