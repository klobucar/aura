//! Authentication module for TOFU (Trust On First Use) identity verification.
//!
//! Handles hybrid identity (Ed25519 + ML-DSA-65) signature verification,
//! username claiming, and session management.

use crate::config::{Config, VerificationMode};
use crate::db::{AdminPermissions, Database, User};
use aura_identity::{
    auth_transcript, IdentityPublicKey, IdentitySignature, Purpose, AUTH_CHALLENGE_LEN,
};
use dashmap::DashMap;
use rand::RngExt;
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

    #[error("Invalid username: {0}")]
    InvalidUsername(String),

    #[error("Invalid identity public key")]
    InvalidPublicKey,
    /// The account's Ed25519 key matches but its pinned ML-DSA key does not.
    #[error("Identity key does not match the one pinned for this account")]
    IdentityKeyMismatch,

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

impl AuthError {
    /// A short, stable reason code for telemetry: never carries the user,
    /// name or key involved.
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::InvalidPassword => "invalid_password",
            AuthError::UsernameTaken(_) => "username_taken",
            AuthError::InvalidUsername(_) => "invalid_username",
            AuthError::InvalidPublicKey => "invalid_public_key",
            AuthError::IdentityKeyMismatch => "identity_key_mismatch",
            AuthError::InvalidSignature => "invalid_signature",
            AuthError::UserBanned => "banned",
            AuthError::VerificationRequired => "verification_required",
            AuthError::InvalidSession => "invalid_session",
            AuthError::PermissionDenied => "permission_denied",
            AuthError::UserNotFound => "user_not_found",
            AuthError::DatabaseError(_) => "internal_error",
        }
    }
}

/// Session information stored in memory.
#[derive(Debug, Clone)]
pub struct Session {
    pub user_uuid: String,
    pub display_name: String,
    pub verified: bool,
    pub is_admin: bool,
    pub created_at: Instant,
}

/// Authentication result returned to clients.
#[derive(Debug, Clone)]
pub struct AuthResult {
    pub user_uuid: String,
    pub session_token: String,
    pub verified: bool,
    pub display_name: String,
    pub is_admin: bool,
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

    /// Verify a hybrid signature over the login transcript (the challenge,
    /// the claimed display name and the full identity key; see
    /// [`auth_transcript`]). Both halves must verify.
    fn verify_signature(
        identity: &IdentityPublicKey,
        display_name: &str,
        challenge: &[u8; AUTH_CHALLENGE_LEN],
        signature: &[u8],
    ) -> Result<(), AuthError> {
        let signature =
            IdentitySignature::from_bytes(signature).map_err(|_| AuthError::InvalidSignature)?;
        let transcript = auth_transcript(challenge, display_name, identity);
        identity
            .verify(Purpose::Auth, &transcript, &signature)
            .map_err(|_| AuthError::InvalidSignature)
    }

    /// Reject display names that could spoof another user or a role.
    ///
    /// Printable ASCII only, which rules out homoglyphs (Cyrillic `а` for
    /// Latin `a`) and invisible characters (zero-width space, control
    /// characters). Reserved names are matched after dropping everything but
    /// letters and digits, so `a.d.m.i.n` and `R O O T` are caught too.
    fn validate_display_name(display_name: &str) -> Result<(), AuthError> {
        if display_name.is_empty() || display_name.len() > 64 {
            return Err(AuthError::InvalidUsername(
                "must be 1 to 64 characters".to_string(),
            ));
        }

        if display_name != display_name.trim() {
            return Err(AuthError::InvalidUsername(
                "cannot have leading or trailing whitespace".to_string(),
            ));
        }

        if !display_name
            .chars()
            .all(|c| c.is_ascii() && !c.is_ascii_control())
        {
            return Err(AuthError::InvalidUsername(
                "must contain only printable ASCII characters".to_string(),
            ));
        }

        let canonical: String = display_name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();

        if canonical.is_empty() {
            return Err(AuthError::InvalidUsername(
                "must contain at least one letter or digit".to_string(),
            ));
        }

        const RESERVED_NAMES: &[&str] = &["admin", "system", "server", "aura", "root", "moderator"];
        if RESERVED_NAMES.contains(&canonical.as_str()) {
            return Err(AuthError::UsernameTaken(display_name.to_string()));
        }

        Ok(())
    }

    /// Authenticate a user with TOFU identity verification.
    ///
    /// Flow:
    /// 1. Validate server password (if required)
    /// 2. Verify the hybrid signature over the login transcript
    /// 3. Look the account up by the identity's Ed25519 key (returning user),
    ///    pinning its ML-DSA key if it has none yet, or create one (TOFU)
    /// 4. Enforce verification policy
    /// 5. Create and return session
    pub fn authenticate(
        &self,
        identity_public_key: &[u8],
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
        let identity = IdentityPublicKey::from_bytes(identity_public_key)
            .map_err(|_| AuthError::InvalidPublicKey)?;
        let challenge: &[u8; AUTH_CHALLENGE_LEN] = challenge
            .try_into()
            .map_err(|_| AuthError::InvalidSignature)?;
        Self::verify_signature(&identity, display_name, challenge, signature)?;

        // 3. Check existing user or create new
        let ed25519 = identity.ed25519_bytes();
        let user = match self.db.find_user_by_key(&ed25519)? {
            Some(existing_user) => {
                match existing_user.identity_public_key.as_deref() {
                    // An account from before hybrid identities, or a bootstrap
                    // admin. Its pinned Ed25519 key just signed a transcript
                    // that includes the whole identity key, so pin it now.
                    None => self
                        .db
                        .set_identity_public_key(&existing_user.user_uuid, identity.as_bytes())?,
                    Some(pinned) if pinned == identity.as_bytes() => {}
                    Some(_) => return Err(AuthError::IdentityKeyMismatch),
                }
                // Returning user - update last seen
                self.db.update_last_seen(&existing_user.user_uuid)?;
                existing_user
            }
            None => {
                // New user - the name must be valid and not reserved...
                Self::validate_display_name(display_name)?;

                // ...and available
                if self.db.find_user_by_name(display_name)?.is_some() {
                    return Err(AuthError::UsernameTaken(display_name.to_string()));
                }

                // Create new user with TOFU identity
                let user_uuid =
                    self.db
                        .create_user(&ed25519, Some(identity.as_bytes()), display_name)?;
                self.db.find_user_by_uuid(&user_uuid)?.unwrap()
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
        let is_admin = self.db.is_admin(&user.user_uuid)?;

        let session = Session {
            user_uuid: user.user_uuid.clone(),
            display_name: user.display_name.clone(),
            verified: user.verified,
            is_admin,
            created_at: Instant::now(),
        };

        self.sessions.insert(session_token.clone(), session);

        Ok(AuthResult {
            user_uuid: user.user_uuid,
            session_token,
            verified: user.verified,
            display_name: user.display_name,
            is_admin,
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
        self.sessions
            .retain(|_, session| session.created_at.elapsed() < self.session_ttl);
    }

    /// Get session count (for monitoring).
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    // =========================================================================
    // Admin Operations
    // =========================================================================

    /// Verify a user (admin only).
    pub fn verify_user(&self, admin_token: &str, target_user_uuid: &str) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        // Check admin permissions
        let perms = self
            .db
            .get_admin_permissions(&session.user_uuid)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.verify_users {
            return Err(AuthError::PermissionDenied);
        }

        // Verify the target user exists
        if self.db.find_user_by_uuid(target_user_uuid)?.is_none() {
            return Err(AuthError::UserNotFound);
        }

        self.db.set_user_verified(target_user_uuid, true)?;
        Ok(())
    }

    /// Ban a user (admin only).
    pub fn ban_user(&self, admin_token: &str, target_user_uuid: &str) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        // Check admin permissions
        let perms = self
            .db
            .get_admin_permissions(&session.user_uuid)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.ban_users {
            return Err(AuthError::PermissionDenied);
        }

        // Prevent self-ban
        if session.user_uuid == target_user_uuid {
            return Err(AuthError::PermissionDenied);
        }

        // Verify the target user exists
        if self.db.find_user_by_uuid(target_user_uuid)?.is_none() {
            return Err(AuthError::UserNotFound);
        }

        self.db.set_user_banned(target_user_uuid, true)?;

        // Invalidate any active sessions for banned user
        self.sessions.retain(|_, s| s.user_uuid != target_user_uuid);

        Ok(())
    }

    /// Unban a user (admin only).
    pub fn unban_user(&self, admin_token: &str, target_user_uuid: &str) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        let perms = self
            .db
            .get_admin_permissions(&session.user_uuid)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.ban_users {
            return Err(AuthError::PermissionDenied);
        }

        if self.db.find_user_by_uuid(target_user_uuid)?.is_none() {
            return Err(AuthError::UserNotFound);
        }

        self.db.set_user_banned(target_user_uuid, false)?;
        Ok(())
    }

    /// Grant admin privileges to a user (admin only).
    pub fn grant_admin(
        &self,
        admin_token: &str,
        target_user_uuid: &str,
        permissions: &AdminPermissions,
    ) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        let perms = self
            .db
            .get_admin_permissions(&session.user_uuid)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.grant_admin {
            return Err(AuthError::PermissionDenied);
        }

        // Prevent self-promotion (already admin if here, but prevent changing own perms)
        if session.user_uuid == target_user_uuid {
            return Err(AuthError::PermissionDenied);
        }

        if self.db.find_user_by_uuid(target_user_uuid)?.is_none() {
            return Err(AuthError::UserNotFound);
        }

        self.db
            .grant_admin(target_user_uuid, permissions, Some(&session.user_uuid))?;
        Ok(())
    }

    /// Revoke admin privileges (admin only).
    pub fn revoke_admin(&self, admin_token: &str, target_user_uuid: &str) -> Result<(), AuthError> {
        let session = self.validate_session(admin_token)?;

        let perms = self
            .db
            .get_admin_permissions(&session.user_uuid)?
            .ok_or(AuthError::PermissionDenied)?;

        if !perms.grant_admin {
            return Err(AuthError::PermissionDenied);
        }

        // Prevent self-revocation
        if session.user_uuid == target_user_uuid {
            return Err(AuthError::PermissionDenied);
        }

        self.db.revoke_admin(target_user_uuid)?;
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
    use aura_identity::IdentitySecret;

    fn create_test_auth_service() -> (AuthService, IdentitySecret) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let config = Config::default();
        (AuthService::new(db, config), IdentitySecret::generate())
    }

    /// What a client sends to log in as `name`: its identity public key and
    /// its hybrid signature over the login transcript.
    fn login(id: &IdentitySecret, name: &str, challenge: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let pk = id.public_key();
        let challenge: &[u8; AUTH_CHALLENGE_LEN] = challenge.try_into().unwrap();
        let sig = id.sign(Purpose::Auth, &auth_transcript(challenge, name, &pk));
        (pk.as_bytes().to_vec(), sig.as_bytes().to_vec())
    }

    #[test]
    fn test_new_user_authentication() {
        let (auth, id) = create_test_auth_service();
        let challenge = AuthService::generate_challenge();
        let (pk, sig) = login(&id, "Alice", &challenge);

        let result = auth
            .authenticate(&pk, "Alice", &sig, &challenge, None)
            .unwrap();

        assert!(!result.user_uuid.is_empty());
        assert_eq!(result.display_name, "Alice");
        assert!(!result.verified);
        let user = auth
            .db
            .find_user_by_uuid(&result.user_uuid)
            .unwrap()
            .unwrap();
        assert_eq!(user.identity_public_key.as_deref(), Some(pk.as_slice()));
    }

    #[test]
    fn test_returning_user_authentication() {
        let (auth, id) = create_test_auth_service();

        let challenge1 = AuthService::generate_challenge();
        let (pk, sig1) = login(&id, "Alice", &challenge1);
        let result1 = auth
            .authenticate(&pk, "Alice", &sig1, &challenge1, None)
            .unwrap();

        let challenge2 = AuthService::generate_challenge();
        let (_, sig2) = login(&id, "Alice", &challenge2);
        let result2 = auth
            .authenticate(&pk, "Alice", &sig2, &challenge2, None)
            .unwrap();

        assert_eq!(result1.user_uuid, result2.user_uuid);
    }

    /// An account from before hybrid identities (Ed25519 key only) keeps its
    /// id: its first hybrid login pins the ML-DSA key.
    #[test]
    fn test_legacy_account_pins_its_identity_key_on_login() {
        let (auth, _) = create_test_auth_service();
        let ed_secret = [0x31u8; 32];
        let legacy_pk = ed25519_dalek::SigningKey::from_bytes(&ed_secret)
            .verifying_key()
            .to_bytes();
        let uuid = auth.db.create_user(&legacy_pk, None, "Alice").unwrap();

        let upgraded = IdentitySecret::from_parts(&ed_secret, &[0x32; 32]);
        let challenge = AuthService::generate_challenge();
        let (pk, sig) = login(&upgraded, "Alice", &challenge);
        let result = auth
            .authenticate(&pk, "Alice", &sig, &challenge, None)
            .unwrap();
        assert_eq!(result.user_uuid, uuid);
        let user = auth.db.find_user_by_uuid(&uuid).unwrap().unwrap();
        assert_eq!(user.identity_public_key.as_deref(), Some(pk.as_slice()));
    }

    /// Once pinned, a different ML-DSA key under the same Ed25519 key is refused.
    #[test]
    fn test_pinned_identity_key_cannot_be_swapped() {
        let (auth, _) = create_test_auth_service();
        let ed_secret = [0x41u8; 32];
        let first = IdentitySecret::from_parts(&ed_secret, &[0x01; 32]);
        let challenge = AuthService::generate_challenge();
        let (pk, sig) = login(&first, "Alice", &challenge);
        auth.authenticate(&pk, "Alice", &sig, &challenge, None)
            .unwrap();

        let swapped = IdentitySecret::from_parts(&ed_secret, &[0x02; 32]);
        let challenge2 = AuthService::generate_challenge();
        let (pk2, sig2) = login(&swapped, "Alice", &challenge2);
        let result = auth.authenticate(&pk2, "Alice", &sig2, &challenge2, None);
        assert!(matches!(result, Err(AuthError::IdentityKeyMismatch)));
    }

    #[test]
    fn test_username_claiming() {
        let (auth, id1) = create_test_auth_service();
        let id2 = IdentitySecret::generate();

        let challenge = AuthService::generate_challenge();
        let (pk1, sig) = login(&id1, "Alice", &challenge);
        auth.authenticate(&pk1, "Alice", &sig, &challenge, None)
            .unwrap();

        let challenge2 = AuthService::generate_challenge();
        let (pk2, sig2) = login(&id2, "Alice", &challenge2);
        let result = auth.authenticate(&pk2, "Alice", &sig2, &challenge2, None);
        assert!(matches!(result, Err(AuthError::UsernameTaken(_))));

        let challenge3 = AuthService::generate_challenge();
        let (_, sig3) = login(&id2, "Bob", &challenge3);
        auth.authenticate(&pk2, "Bob", &sig3, &challenge3, None)
            .unwrap();
    }

    #[test]
    fn test_reserved_usernames() {
        let (auth, id) = create_test_auth_service();

        for name in ["admin", "System", "SERVER", "aura"] {
            let challenge = AuthService::generate_challenge();
            let (pk, sig) = login(&id, name, &challenge);
            let result = auth.authenticate(&pk, name, &sig, &challenge, None);

            assert!(
                matches!(result, Err(AuthError::UsernameTaken(_))),
                "Name {} should have been rejected as reserved",
                name
            );
        }
    }

    #[test]
    fn test_spoofing_usernames_rejected() {
        let (auth, id) = create_test_auth_service();

        let invalid_names = [
            "\u{0430}dmin",  // Cyrillic 'а'
            "\u{0410}lice",  // Cyrillic 'А'
            "admin\u{200B}", // zero-width space
            " admin",        // leading space
            "admin\n",       // control character
            "---",           // no letters or digits
            "",              // empty
            &"a".repeat(65), // too long
        ];
        for name in invalid_names {
            let challenge = AuthService::generate_challenge();
            let (pk, sig) = login(&id, name, &challenge);
            let result = auth.authenticate(&pk, name, &sig, &challenge, None);
            assert!(
                matches!(result, Err(AuthError::InvalidUsername(_))),
                "name {name:?} should have been rejected as invalid"
            );
        }

        // Reserved names disguised with punctuation or spaces
        for name in ["a.d.m.i.n", "_system_", "s-e-r-v-e-r", "R O O T"] {
            let challenge = AuthService::generate_challenge();
            let (pk, sig) = login(&id, name, &challenge);
            let result = auth.authenticate(&pk, name, &sig, &challenge, None);
            assert!(
                matches!(result, Err(AuthError::UsernameTaken(_))),
                "disguised reserved name {name:?} should have been rejected"
            );
        }

        // Ordinary names with punctuation and digits are fine (a fresh
        // identity each, since one identity registers one name)
        for name in ["alice_1", "Bob Smith", "c.d-e"] {
            let id = IdentitySecret::generate();
            let challenge = AuthService::generate_challenge();
            let (pk, sig) = login(&id, name, &challenge);
            let result = auth.authenticate(&pk, name, &sig, &challenge, None);
            assert!(
                result.is_ok(),
                "name {name:?} should be allowed: {result:?}"
            );
        }
    }

    #[test]
    fn test_server_password() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let mut config = Config::default();
        config.server.password = Some("secret123".to_string());
        let auth = AuthService::new(db, config);
        let id = IdentitySecret::generate();

        let challenge = AuthService::generate_challenge();
        let (pk, sig) = login(&id, "Alice", &challenge);
        let result = auth.authenticate(&pk, "Alice", &sig, &challenge, Some("wrong"));
        assert!(matches!(result, Err(AuthError::InvalidPassword)));

        let challenge2 = AuthService::generate_challenge();
        let (_, sig2) = login(&id, "Alice", &challenge2);
        let result = auth.authenticate(&pk, "Alice", &sig2, &challenge2, None);
        assert!(matches!(result, Err(AuthError::InvalidPassword)));

        let challenge3 = AuthService::generate_challenge();
        let (_, sig3) = login(&id, "Alice", &challenge3);
        auth.authenticate(&pk, "Alice", &sig3, &challenge3, Some("secret123"))
            .unwrap();
    }

    #[test]
    fn test_session_validation() {
        let (auth, id) = create_test_auth_service();
        let challenge = AuthService::generate_challenge();
        let (pk, sig) = login(&id, "Alice", &challenge);

        let result = auth
            .authenticate(&pk, "Alice", &sig, &challenge, None)
            .unwrap();

        let session = auth.validate_session(&result.session_token).unwrap();
        assert_eq!(session.user_uuid, result.user_uuid);

        assert!(auth.validate_session("invalid-token").is_err());

        auth.invalidate_session(&result.session_token);
        assert!(auth.validate_session(&result.session_token).is_err());
    }

    #[test]
    fn test_invalid_signature() {
        let (auth, id) = create_test_auth_service();
        let challenge = AuthService::generate_challenge();
        let (pk, _) = login(&id, "Alice", &challenge);

        let wrong_sig = [0u8; aura_identity::SIGNATURE_LEN];
        let result = auth.authenticate(&pk, "Alice", &wrong_sig, &challenge, None);
        assert!(matches!(result, Err(AuthError::InvalidSignature)));

        // A signature for another display name doesn't transfer.
        let (_, sig_for_bob) = login(&id, "Bob", &challenge);
        let result = auth.authenticate(&pk, "Alice", &sig_for_bob, &challenge, None);
        assert!(matches!(result, Err(AuthError::InvalidSignature)));
    }

    #[test]
    fn test_rejects_bare_ed25519_key() {
        let (auth, id) = create_test_auth_service();
        let challenge = AuthService::generate_challenge();
        let (_, sig) = login(&id, "Alice", &challenge);
        let ed_only = id.public_key().ed25519_bytes();
        let result = auth.authenticate(&ed_only, "Alice", &sig, &challenge, None);
        assert!(matches!(result, Err(AuthError::InvalidPublicKey)));
    }
}
