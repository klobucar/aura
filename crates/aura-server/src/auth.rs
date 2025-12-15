//! Authentication module for TOFU (Trust On First Use) identity verification.
//!
//! Handles Ed25519 signature verification, username claiming, and session management.

use crate::config::{Config, VerificationMode};
use crate::db::{AdminPermissions, Database, User};
use dashmap::DashMap;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::Rng;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Authentication errors.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("Invalid server password")]
    InvalidPassword,

    #[error("Username '{0}' is already taken")]
    UsernameTaken(String),

    #[error("Invalid Ed25519 public key")]
    InvalidPublicKey,

    #[error("Invalid signature")]
    InvalidSignature,

    #[error("User is banned")]
    UserBanned,

    #[error("User is not verified (verification required)")]
    VerificationRequired,

    #[error("Invalid session token")]
    InvalidSession,

    #[error("Permission denied")]
    PermissionDenied,

    #[error("User not found")]
    UserNotFound,

    #[error("Database error: {0}")]
    DatabaseError(#[from] anyhow::Error),
}

/// Session information stored in memory.
#[derive(Debug, Clone)]
pub struct Session {
    pub user_id: u32,
    pub display_name: String,
    pub verified: bool,
    pub is_admin: bool,
    pub created_at: Instant,
}

/// Authentication result returned to clients.
#[derive(Debug, Clone)]
pub struct AuthResult {
    pub user_id: u32,
    pub session_token: String,
    pub verified: bool,
    pub display_name: String,
}

/// Authentication service handling TOFU identity verification.
pub struct AuthService {
    db: Arc<Database>,
    config: Config,
    sessions: Arc<DashMap<String, Session>>,
    session_ttl: Duration,
}

impl AuthService {
    /// Create a new authentication service.
    pub fn new(db: Arc<Database>, config: Config) -> Self {
        Self {
            db,
            config,
            sessions: Arc::new(DashMap::new()),
            session_ttl: Duration::from_secs(24 * 60 * 60), // 24 hours
        }
    }

    /// Generate a cryptographically secure session token.
    fn generate_session_token() -> String {
        let mut rng = rand::rng();
        let bytes: [u8; 32] = rng.random();
        hex::encode(bytes)
    }

    /// Generate a challenge for signature verification.
    pub fn generate_challenge() -> Vec<u8> {
        let mut rng = rand::rng();
        let bytes: [u8; 32] = rng.random();
        bytes.to_vec()
    }

    /// Verify an Ed25519 signature.
    fn verify_signature(
        public_key: &[u8; 32],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), AuthError> {
        let verifying_key =
            VerifyingKey::from_bytes(public_key).map_err(|_| AuthError::InvalidPublicKey)?;

        let sig_bytes: [u8; 64] = signature
            .try_into()
            .map_err(|_| AuthError::InvalidSignature)?;

        let signature = Signature::from_bytes(&sig_bytes);

        verifying_key
            .verify(message, &signature)
            .map_err(|_| AuthError::InvalidSignature)
    }

    /// Authenticate a user with TOFU identity verification.
    ///
    /// Flow:
    /// 1. Validate server password (if required)
    /// 2. Verify Ed25519 signature over challenge
    /// 3. Check if public key exists (returning user) or create new user (TOFU)
    /// 4. Enforce verification policy
    /// 5. Create and return session
    pub fn authenticate(
        &self,
        public_key: &[u8; 32],
        display_name: &str,
        signature: &[u8],
        challenge: &[u8],
        server_password: Option<&str>,
    ) -> Result<AuthResult, AuthError> {
        // 1. Validate server password
        if !self.config.validate_password(server_password) {
            return Err(AuthError::InvalidPassword);
        }

        // 2. Verify signature
        Self::verify_signature(public_key, challenge, signature)?;

        // 3. Check existing user or create new
        let user = match self.db.find_user_by_key(public_key)? {
            Some(existing_user) => {
                // Returning user - update last seen
                self.db.update_last_seen(existing_user.user_id)?;
                existing_user
            }
            None => {
                // New user - check if username is available
                if self.db.find_user_by_name(display_name)?.is_some() {
                    return Err(AuthError::UsernameTaken(display_name.to_string()));
                }

                // Create new user with TOFU identity
                let user_id = self.db.create_user(public_key, display_name)?;
                self.db.find_user_by_id(user_id)?.unwrap()
            }
        };

        // 4. Check if user is banned
        if user.banned {
            return Err(AuthError::UserBanned);
        }

        // 5. Enforce verification policy
        if self.config.verification.mode == VerificationMode::Required && !user.verified {
            return Err(AuthError::VerificationRequired);
        }

        // 6. Create session
        let session_token = Self::generate_session_token();
        let is_admin = self.db.is_admin(user.user_id)?;

        let session = Session {
            user_id: user.user_id,
            display_name: user.display_name.clone(),
            verified: user.verified,
            is_admin,
            created_at: Instant::now(),
        };

        self.sessions.insert(session_token.clone(), session);

        Ok(AuthResult {
            user_id: user.user_id,
            session_token,
            verified: user.verified,
            display_name: user.display_name,
        })
    }

    /// Validate a session token and return the session.
    pub fn validate_session(&self, token: &str) -> Result<Session, AuthError> {
        let session = self
            .sessions
            .get(token)
            .map(|s| s.clone())
            .ok_or(AuthError::InvalidSession)?;

        // Check TTL
        if session.created_at.elapsed() > self.session_ttl {
            self.sessions.remove(token);
            return Err(AuthError::InvalidSession);
        }

        Ok(session)
    }

    /// Invalidate a session (logout).
    pub fn invalidate_session(&self, token: &str) {
        self.sessions.remove(token);
    }

    /// Clean up expired sessions.
    pub fn cleanup_expired_sessions(&self) {
        self.sessions.retain(|_, session| session.created_at.elapsed() < self.session_ttl);
    }

    /// Get session count (for monitoring).
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    // =========================================================================
    // Admin Operations
    // =========================================================================

    /// Verify a user (admin only).
    pub fn verify_user(&self, admin_token: &str, target_user_id: u32) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        // Check admin permissions
        let perms = self
            .db
            .get_admin_permissions(session.user_id)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.verify_users {
            return Err(AuthError::PermissionDenied);
        }

        // Verify the target user exists
        if self.db.find_user_by_id(target_user_id)?.is_none() {
            return Err(AuthError::UserNotFound);
        }

        self.db.set_user_verified(target_user_id, true)?;
        Ok(())
    }

    /// Ban a user (admin only).
    pub fn ban_user(&self, admin_token: &str, target_user_id: u32) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        // Check admin permissions
        let perms = self
            .db
            .get_admin_permissions(session.user_id)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.ban_users {
            return Err(AuthError::PermissionDenied);
        }

        // Prevent self-ban
        if session.user_id == target_user_id {
            return Err(AuthError::PermissionDenied);
        }

        // Verify the target user exists
        if self.db.find_user_by_id(target_user_id)?.is_none() {
            return Err(AuthError::UserNotFound);
        }

        self.db.set_user_banned(target_user_id, true)?;

        // Invalidate any active sessions for banned user
        self.sessions
            .retain(|_, s| s.user_id != target_user_id);

        Ok(())
    }

    /// Unban a user (admin only).
    pub fn unban_user(&self, admin_token: &str, target_user_id: u32) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        let perms = self
            .db
            .get_admin_permissions(session.user_id)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.ban_users {
            return Err(AuthError::PermissionDenied);
        }

        if self.db.find_user_by_id(target_user_id)?.is_none() {
            return Err(AuthError::UserNotFound);
        }

        self.db.set_user_banned(target_user_id, false)?;
        Ok(())
    }

    /// Grant admin privileges to a user (admin only).
    pub fn grant_admin(
        &self,
        admin_token: &str,
        target_user_id: u32,
        permissions: &AdminPermissions,
    ) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        let perms = self
            .db
            .get_admin_permissions(session.user_id)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.grant_admin {
            return Err(AuthError::PermissionDenied);
        }

        // Prevent self-promotion (already admin if here, but prevent changing own perms)
        if session.user_id == target_user_id {
            return Err(AuthError::PermissionDenied);
        }

        if self.db.find_user_by_id(target_user_id)?.is_none() {
            return Err(AuthError::UserNotFound);
        }

        self.db
            .grant_admin(target_user_id, permissions, Some(session.user_id))?;
        Ok(())
    }

    /// Revoke admin privileges (admin only).
    pub fn revoke_admin(&self, admin_token: &str, target_user_id: u32) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        let perms = self
            .db
            .get_admin_permissions(session.user_id)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.grant_admin {
            return Err(AuthError::PermissionDenied);
        }

        // Prevent self-revocation
        if session.user_id == target_user_id {
            return Err(AuthError::PermissionDenied);
        }

        self.db.revoke_admin(target_user_id)?;
        Ok(())
    }

    /// List users (admin only).
    pub fn list_users(
        &self,
        admin_token: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<User>, AuthError> {
        let session = self.validate_session(admin_token)?;

        // Any admin can list users
        if !session.is_admin {
            return Err(AuthError::PermissionDenied);
        }

        let users = self.db.list_users(offset, limit)?;
        Ok(users)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn create_test_auth_service() -> (AuthService, SigningKey) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let config = Config::default();
        let auth = AuthService::new(db, config);
        
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        
        (auth, signing_key)
    }

    fn sign_challenge(signing_key: &SigningKey, challenge: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        signing_key.sign(challenge).to_bytes()
    }

    #[test]
    fn test_new_user_authentication() {
        let (auth, signing_key) = create_test_auth_service();
        let public_key: [u8; 32] = signing_key.verifying_key().to_bytes();
        let challenge = AuthService::generate_challenge();
        let signature = sign_challenge(&signing_key, &challenge);

        let result = auth
            .authenticate(&public_key, "Alice", &signature, &challenge, None)
            .unwrap();

        assert_eq!(result.user_id, 1);
        assert_eq!(result.display_name, "Alice");
        assert!(!result.verified);
    }

    #[test]
    fn test_returning_user_authentication() {
        let (auth, signing_key) = create_test_auth_service();
        let public_key: [u8; 32] = signing_key.verifying_key().to_bytes();

        // First auth
        let challenge1 = AuthService::generate_challenge();
        let sig1 = sign_challenge(&signing_key, &challenge1);
        let result1 = auth
            .authenticate(&public_key, "Alice", &sig1, &challenge1, None)
            .unwrap();

        // Second auth (returning)
        let challenge2 = AuthService::generate_challenge();
        let sig2 = sign_challenge(&signing_key, &challenge2);
        let result2 = auth
            .authenticate(&public_key, "Alice", &sig2, &challenge2, None)
            .unwrap();

        // Same user_id
        assert_eq!(result1.user_id, result2.user_id);
    }

    #[test]
    fn test_username_claiming() {
        let (auth, signing_key1) = create_test_auth_service();
        let mut rng = rand::thread_rng();
        let signing_key2 = SigningKey::generate(&mut rng);

        let pk1: [u8; 32] = signing_key1.verifying_key().to_bytes();
        let pk2: [u8; 32] = signing_key2.verifying_key().to_bytes();

        // User 1 claims "Alice"
        let challenge = AuthService::generate_challenge();
        let sig = sign_challenge(&signing_key1, &challenge);
        auth.authenticate(&pk1, "Alice", &sig, &challenge, None)
            .unwrap();

        // User 2 tries to claim "Alice"
        let challenge2 = AuthService::generate_challenge();
        let sig2 = sign_challenge(&signing_key2, &challenge2);
        let result = auth.authenticate(&pk2, "Alice", &sig2, &challenge2, None);

        assert!(matches!(result, Err(AuthError::UsernameTaken(_))));

        // User 2 can claim different name
        let challenge3 = AuthService::generate_challenge();
        let sig3 = sign_challenge(&signing_key2, &challenge3);
        auth.authenticate(&pk2, "Bob", &sig3, &challenge3, None)
            .unwrap();
    }

    #[test]
    fn test_server_password() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let mut config = Config::default();
        config.server.password = Some("secret123".to_string());
        let auth = AuthService::new(db, config);

        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        let public_key: [u8; 32] = signing_key.verifying_key().to_bytes();
        let challenge = AuthService::generate_challenge();
        let sig = sign_challenge(&signing_key, &challenge);

        // Wrong password
        let result = auth.authenticate(&public_key, "Alice", &sig, &challenge, Some("wrong"));
        assert!(matches!(result, Err(AuthError::InvalidPassword)));

        // No password
        let challenge2 = AuthService::generate_challenge();
        let sig2 = sign_challenge(&signing_key, &challenge2);
        let result = auth.authenticate(&public_key, "Alice", &sig2, &challenge2, None);
        assert!(matches!(result, Err(AuthError::InvalidPassword)));

        // Correct password
        let challenge3 = AuthService::generate_challenge();
        let sig3 = sign_challenge(&signing_key, &challenge3);
        auth.authenticate(&public_key, "Alice", &sig3, &challenge3, Some("secret123"))
            .unwrap();
    }

    #[test]
    fn test_session_validation() {
        let (auth, signing_key) = create_test_auth_service();
        let public_key: [u8; 32] = signing_key.verifying_key().to_bytes();
        let challenge = AuthService::generate_challenge();
        let sig = sign_challenge(&signing_key, &challenge);

        let result = auth
            .authenticate(&public_key, "Alice", &sig, &challenge, None)
            .unwrap();

        // Valid session
        let session = auth.validate_session(&result.session_token).unwrap();
        assert_eq!(session.user_id, result.user_id);

        // Invalid session
        assert!(auth.validate_session("invalid-token").is_err());

        // Invalidated session
        auth.invalidate_session(&result.session_token);
        assert!(auth.validate_session(&result.session_token).is_err());
    }

    #[test]
    fn test_invalid_signature() {
        let (auth, signing_key) = create_test_auth_service();
        let public_key: [u8; 32] = signing_key.verifying_key().to_bytes();
        let challenge = AuthService::generate_challenge();
        let wrong_sig = [0u8; 64]; // Invalid signature

        let result = auth.authenticate(&public_key, "Alice", &wrong_sig, &challenge, None);
        assert!(matches!(result, Err(AuthError::InvalidSignature)));
    }
}
