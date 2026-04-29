//! SQLite database module for persistent storage.
//!
//! Handles user identities, admins, TOFU key pinning, and session tracking.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Current database schema version.
const SCHEMA_VERSION: i64 = 1;

/// Tuple shape returned by `get_all_channels`:
/// `(channel_id, name, comment, icon_type, icon_data, position, channel_type)`.
pub type ChannelRow = (String, String, String, i32, Vec<u8>, i32, i32);

/// Tuple shape returned by `get_user_profile`:
/// `(bio, avatar_data, signature, signing_key)`.
pub type UserProfileRow = (String, Vec<u8>, Vec<u8>, Vec<u8>);

/// Thread-safe database handle.
pub type DbHandle = Arc<Mutex<Connection>>;

/// User record from the database.
#[derive(Debug, Clone)]
pub struct User {
    pub user_uuid: String,
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
    pub manage_channels: bool,
}

impl AdminPermissions {
    /// Full permissions for bootstrap admin.
    pub fn full() -> Self {
        Self {
            verify_users: true,
            ban_users: true,
            grant_admin: true,
            manage_channels: true,
        }
    }
}

/// Admin record from the database.
#[derive(Debug, Clone)]
pub struct Admin {
    pub user_uuid: String,
    pub permissions: AdminPermissions,
    pub granted_at: i64,
    pub granted_by: Option<String>,
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
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
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

            -- Channels table
            CREATE TABLE IF NOT EXISTS channels (
                channel_id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                comment TEXT,
                icon_type INTEGER DEFAULT 0, -- 0=none, 1=emoji, 2=preset, 3=custom
                icon_data BLOB,
                position INTEGER DEFAULT 0,
                channel_type INTEGER DEFAULT 0 -- 0=regular, 1=lobby
            );

            -- User profiles table
            CREATE TABLE IF NOT EXISTS user_profiles (
                user_uuid TEXT PRIMARY KEY,
                bio TEXT,
                avatar_data BLOB,
                signature BLOB,
                signing_key BLOB,
                FOREIGN KEY (user_uuid) REFERENCES users(user_uuid)
            );

            -- Users table with UUID primary key and TOFU key pinning
            CREATE TABLE IF NOT EXISTS users (
                user_uuid TEXT PRIMARY KEY,
                ed25519_public_key BLOB NOT NULL UNIQUE,
                display_name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                created_at INTEGER NOT NULL,
                verified BOOLEAN NOT NULL DEFAULT 0,
                banned BOOLEAN NOT NULL DEFAULT 0
            );

            -- Admins table
            CREATE TABLE IF NOT EXISTS admins (
                user_uuid TEXT PRIMARY KEY,
                permissions TEXT NOT NULL,
                granted_at INTEGER NOT NULL,
                granted_by TEXT,
                FOREIGN KEY (user_uuid) REFERENCES users(user_uuid),
                FOREIGN KEY (granted_by) REFERENCES users(user_uuid)
            );

            -- TOFU identity tracking
            CREATE TABLE IF NOT EXISTS tofu_identities (
                user_uuid TEXT PRIMARY KEY,
                first_seen_key BLOB NOT NULL,
                first_seen_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                FOREIGN KEY (user_uuid) REFERENCES users(user_uuid)
            );

            -- DM groups (ephemeral MLS groups for 1:1 conversations)
            CREATE TABLE IF NOT EXISTS dm_groups (
                dm_group_uuid TEXT PRIMARY KEY,
                user1_uuid TEXT NOT NULL,
                user2_uuid TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                last_message_at INTEGER,
                FOREIGN KEY (user1_uuid) REFERENCES users(user_uuid),
                FOREIGN KEY (user2_uuid) REFERENCES users(user_uuid),
                CHECK (user1_uuid < user2_uuid)  -- Enforce canonical ordering
            );

            -- DM messages (store-and-forward for offline users)
            CREATE TABLE IF NOT EXISTS dm_messages (
                message_uuid TEXT PRIMARY KEY,
                dm_group_uuid TEXT NOT NULL,
                sender_uuid TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                encrypted_content BLOB NOT NULL,
                delivered BOOLEAN NOT NULL DEFAULT 0,
                FOREIGN KEY (dm_group_uuid) REFERENCES dm_groups(dm_group_uuid),
                FOREIGN KEY (sender_uuid) REFERENCES users(user_uuid)
            );

            -- Display name change history
            CREATE TABLE IF NOT EXISTS display_name_history (
                user_uuid TEXT NOT NULL,
                old_name TEXT NOT NULL,
                new_name TEXT NOT NULL,
                changed_at INTEGER NOT NULL,
                FOREIGN KEY (user_uuid) REFERENCES users(user_uuid)
            );

            -- Indices for fast lookups
            CREATE INDEX IF NOT EXISTS idx_users_public_key ON users(ed25519_public_key);
            CREATE INDEX IF NOT EXISTS idx_users_display_name ON users(display_name);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_dm_groups_users ON dm_groups(user1_uuid, user2_uuid);
            CREATE INDEX IF NOT EXISTS idx_dm_messages_group ON dm_messages(dm_group_uuid, timestamp);
            CREATE INDEX IF NOT EXISTS idx_dm_messages_undelivered ON dm_messages(delivered) WHERE delivered = 0;
            "#,
        )?;

        tracing::info!("Database schema created (version {})", SCHEMA_VERSION);
        Ok(())
    }

    /// Run database migrations from old version to current.
    fn migrate(&self, _conn: &Connection, _from_version: i64) -> Result<()> {
        // No migrations yet for version 1
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
            "SELECT user_uuid, ed25519_public_key, display_name, created_at, verified, banned
             FROM users WHERE ed25519_public_key = ?",
            params![public_key.as_slice()],
            |row| {
                let key_blob: Vec<u8> = row.get(1)?;
                let mut key = [0u8; 32];
                key.copy_from_slice(&key_blob);
                Ok(User {
                    user_uuid: row.get(0)?,
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
            "SELECT user_uuid, ed25519_public_key, display_name, created_at, verified, banned
             FROM users WHERE display_name = ? COLLATE NOCASE",
            params![display_name],
            |row| {
                let key_blob: Vec<u8> = row.get(1)?;
                let mut key = [0u8; 32];
                key.copy_from_slice(&key_blob);
                Ok(User {
                    user_uuid: row.get(0)?,
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

    /// Find a user by their UUID.
    pub fn find_user_by_uuid(&self, user_uuid: &str) -> Result<Option<User>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT user_uuid, ed25519_public_key, display_name, created_at, verified, banned
             FROM users WHERE user_uuid = ?",
            params![user_uuid],
            |row| {
                let key_blob: Vec<u8> = row.get(1)?;
                let mut key = [0u8; 32];
                key.copy_from_slice(&key_blob);
                Ok(User {
                    user_uuid: row.get(0)?,
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
    /// Returns the new user_uuid on success.
    pub fn create_user(&self, public_key: &[u8; 32], display_name: &str) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let now = Self::now();
        let user_uuid = format!("U_{}", Uuid::new_v4());

        // Insert user
        conn.execute(
            "INSERT INTO users (user_uuid, ed25519_public_key, display_name, created_at, verified, banned)
             VALUES (?, ?, ?, ?, 0, 0)",
            params![&user_uuid, public_key.as_slice(), display_name, now],
        )?;

        // Record TOFU identity
        conn.execute(
            "INSERT INTO tofu_identities (user_uuid, first_seen_key, first_seen_at, last_seen_at)
             VALUES (?, ?, ?, ?)",
            params![&user_uuid, public_key.as_slice(), now, now],
        )?;

        tracing::info!(
            "Created new user: uuid={}, name={}",
            user_uuid,
            display_name
        );
        Ok(user_uuid)
    }

    /// Update the last_seen timestamp for a user's TOFU identity.
    pub fn update_last_seen(&self, user_uuid: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE tofu_identities SET last_seen_at = ? WHERE user_uuid = ?",
            params![Self::now(), user_uuid],
        )?;
        Ok(())
    }

    /// Set a user's verified status.
    pub fn set_user_verified(&self, user_uuid: &str, verified: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET verified = ? WHERE user_uuid = ?",
            params![verified, user_uuid],
        )?;
        tracing::info!("User {} verified status set to {}", user_uuid, verified);
        Ok(())
    }

    /// Set a user's banned status.
    pub fn set_user_banned(&self, user_uuid: &str, banned: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET banned = ? WHERE user_uuid = ?",
            params![banned, user_uuid],
        )?;
        tracing::info!("User {} banned status set to {}", user_uuid, banned);
        Ok(())
    }

    /// List users with pagination.
    pub fn list_users(&self, offset: u32, limit: u32) -> Result<Vec<User>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_uuid, ed25519_public_key, display_name, created_at, verified, banned
             FROM users ORDER BY created_at LIMIT ? OFFSET ?",
        )?;

        let users = stmt
            .query_map(params![limit, offset], |row| {
                let key_blob: Vec<u8> = row.get(1)?;
                let mut key = [0u8; 32];
                key.copy_from_slice(&key_blob);
                Ok(User {
                    user_uuid: row.get(0)?,
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
    pub fn is_admin(&self, user_uuid: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM admins WHERE user_uuid = ?",
            params![user_uuid],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get admin permissions for a user.
    pub fn get_admin_permissions(&self, user_uuid: &str) -> Result<Option<AdminPermissions>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT permissions FROM admins WHERE user_uuid = ?",
            params![user_uuid],
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
        user_uuid: &str,
        permissions: &AdminPermissions,
        granted_by: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let permissions_json = serde_json::to_string(permissions)?;

        conn.execute(
            "INSERT OR REPLACE INTO admins (user_uuid, permissions, granted_at, granted_by)
             VALUES (?, ?, ?, ?)",
            params![user_uuid, permissions_json, Self::now(), granted_by],
        )?;

        tracing::info!("Granted admin to user {}", user_uuid);
        Ok(())
    }

    /// Revoke admin status from a user.
    pub fn revoke_admin(&self, user_uuid: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM admins WHERE user_uuid = ?", params![user_uuid])?;
        tracing::info!("Revoked admin from user {}", user_uuid);
        Ok(())
    }

    /// Check if db has any users (for bootstrap admin check).
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.user_count()? == 0)
    }

    /// Create bootstrap admin from public key.
    /// This is used for first-time server setup via environment variable.
    pub fn create_bootstrap_admin(
        &self,
        public_key: &[u8; 32],
        display_name: &str,
    ) -> Result<String> {
        let user_uuid = self.create_user(public_key, display_name)?;
        self.grant_admin(&user_uuid, &AdminPermissions::full(), None)?;
        self.set_user_verified(&user_uuid, true)?;
        tracing::info!(
            "Created bootstrap admin: uuid={}, name={}",
            user_uuid,
            display_name
        );
        Ok(user_uuid)
    }

    /// Get all channels from the database.
    pub fn get_all_channels(&self) -> Result<Vec<ChannelRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT channel_id, name, comment, icon_type, icon_data, position, channel_type 
             FROM channels ORDER BY position, channel_id",
        )?;

        let channels = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    row.get(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                    row.get(5)?,
                    row.get(6)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(channels)
    }

    /// Upsert a channel.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_channel(
        &self,
        id: Option<String>,
        name: &str,
        comment: &str,
        icon_type: i32,
        icon_data: &[u8],
        position: i32,
        channel_type: i32,
    ) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        if let Some(id) = id {
            conn.execute(
                "INSERT OR REPLACE INTO channels (channel_id, name, comment, icon_type, icon_data, position, channel_type)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![id, name, comment, icon_type, icon_data, position, channel_type],
            )?;
            Ok(id)
        } else {
            let channel_id = format!("C_{}", Uuid::new_v4());
            conn.execute(
                "INSERT INTO channels (channel_id, name, comment, icon_type, icon_data, position, channel_type)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![channel_id, name, comment, icon_type, icon_data, position, channel_type],
            )?;
            Ok(channel_id)
        }
    }

    /// Delete a channel.
    pub fn delete_channel(&self, channel_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM channels WHERE channel_id = ?",
            params![channel_id],
        )?;
        Ok(())
    }

    /// Get a user profile.
    pub fn get_user_profile(&self, user_uuid: &str) -> Result<Option<UserProfileRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT bio, avatar_data, signature, signing_key FROM user_profiles WHERE user_uuid = ?",
            params![user_uuid],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Update a user profile.
    pub fn upsert_user_profile(
        &self,
        user_uuid: &str,
        bio: &str,
        avatar_data: &[u8],
        signature: &[u8],
        signing_key: &[u8],
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO user_profiles (user_uuid, bio, avatar_data, signature, signing_key)
             VALUES (?, ?, ?, ?, ?)",
            params![user_uuid, bio, avatar_data, signature, signing_key],
        )?;
        Ok(())
    }

    /// Delete a user and their profile.
    pub fn delete_user(&self, user_uuid: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM user_profiles WHERE user_uuid = ?",
            params![user_uuid],
        )?;
        conn.execute("DELETE FROM admins WHERE user_uuid = ?", params![user_uuid])?;
        conn.execute("DELETE FROM users WHERE user_uuid = ?", params![user_uuid])?;
        Ok(())
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
        let user_uuid = db.create_user(&key, "Alice").unwrap();
        assert!(!user_uuid.is_empty());

        // Find by key
        let user = db.find_user_by_key(&key).unwrap().unwrap();
        assert_eq!(user.user_uuid, user_uuid);
        assert_eq!(user.display_name, "Alice");
        assert!(!user.verified);
        assert!(!user.banned);

        // Find by name (case insensitive)
        let user = db.find_user_by_name("alice").unwrap().unwrap();
        assert_eq!(user.user_uuid, user_uuid);

        // Find by UUID
        let user = db.find_user_by_uuid(&user_uuid).unwrap().unwrap();
        assert_eq!(user.display_name, "Alice");
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
        let user_uuid = db.create_user(&key, "Admin").unwrap();

        // Not admin initially
        assert!(!db.is_admin(&user_uuid).unwrap());

        // Grant admin
        let perms = AdminPermissions {
            verify_users: true,
            ban_users: true,
            grant_admin: false,
            manage_channels: true,
        };
        db.grant_admin(&user_uuid, &perms, None).unwrap();

        // Now is admin
        assert!(db.is_admin(&user_uuid).unwrap());

        // Check permissions
        let stored_perms = db.get_admin_permissions(&user_uuid).unwrap().unwrap();
        assert!(stored_perms.verify_users);
        assert!(stored_perms.ban_users);
        assert!(!stored_perms.grant_admin);

        // Revoke admin
        db.revoke_admin(&user_uuid).unwrap();
        assert!(!db.is_admin(&user_uuid).unwrap());
    }

    #[test]
    fn test_user_verification_and_ban() {
        let db = Database::open_in_memory().unwrap();
        let key = [0x42u8; 32];
        let user_uuid = db.create_user(&key, "TestUser").unwrap();

        // Not verified initially
        let user = db.find_user_by_uuid(&user_uuid).unwrap().unwrap();
        assert!(!user.verified);

        // Verify
        db.set_user_verified(&user_uuid, true).unwrap();
        let user = db.find_user_by_uuid(&user_uuid).unwrap().unwrap();
        assert!(user.verified);

        // Ban
        db.set_user_banned(&user_uuid, true).unwrap();
        let user = db.find_user_by_uuid(&user_uuid).unwrap().unwrap();
        assert!(user.banned);
    }

    #[test]
    fn test_bootstrap_admin() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.is_empty().unwrap());

        let key = [0x42u8; 32];
        let user_uuid = db.create_bootstrap_admin(&key, "RootAdmin").unwrap();

        assert!(!db.is_empty().unwrap());
        assert!(db.is_admin(&user_uuid).unwrap());

        let user = db.find_user_by_uuid(&user_uuid).unwrap().unwrap();
        assert!(user.verified);

        let perms = db.get_admin_permissions(&user_uuid).unwrap().unwrap();
        assert!(perms.verify_users);
        assert!(perms.ban_users);
        assert!(perms.grant_admin);
    }
}
