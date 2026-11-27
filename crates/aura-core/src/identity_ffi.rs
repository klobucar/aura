//! Identity and login functions exported to the Swift and C# clients.
//!
//! Each client keeps one 64-byte identity secret (Ed25519 secret key, then
//! ML-DSA-65 seed; see `aura_identity`) and does all signing through these
//! functions, so the hybrid signature format and the login transcript are
//! defined once, here and in `aura_identity`, rather than in each client.

use aura_identity::{auth_transcript, IdentitySecret, Purpose, AUTH_CHALLENGE_LEN};
use aura_protocol::{AuthRequest as ProtoAuthRequest, AuthResponse as ProtoAuthResponse};
use prost::Message;
use zeroize::Zeroizing;

/// Control-stream message type of the login request.
pub const MSG_AUTH_REQUEST: u8 = 0x03;

/// Identity and login errors surfaced to the clients.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum IdentityError {
    #[error("invalid identity secret: {message}")]
    InvalidSecret { message: String },
    #[error("challenge must be 32 bytes, got {got}")]
    BadChallenge { got: u32 },
    #[error("malformed auth response: {message}")]
    MalformedResponse { message: String },
}

fn secret(bytes: &[u8]) -> Result<IdentitySecret, IdentityError> {
    IdentitySecret::from_bytes(bytes).map_err(|e| IdentityError::InvalidSecret {
        message: e.to_string(),
    })
}

/// A new 64-byte identity secret. Store it like a private key.
#[uniffi::export]
pub fn generate_identity_secret() -> Vec<u8> {
    IdentitySecret::generate().to_bytes().to_vec()
}

/// Upgrade an Ed25519-only identity (its 32-byte secret key) to a hybrid
/// identity secret, keeping the Ed25519 key and adding a fresh ML-DSA-65
/// seed. The Ed25519 public key, and the account it is pinned to, stay the
/// same; the server pins the new ML-DSA key at the next login.
#[uniffi::export]
pub fn upgrade_ed25519_identity(ed25519_secret: Vec<u8>) -> Result<Vec<u8>, IdentityError> {
    let ed: Zeroizing<[u8; 32]> =
        Zeroizing::new(ed25519_secret.as_slice().try_into().map_err(|_| {
            IdentityError::InvalidSecret {
                message: format!(
                    "Ed25519 secret must be 32 bytes, got {}",
                    ed25519_secret.len()
                ),
            }
        })?);
    let fresh = IdentitySecret::generate().to_bytes();
    let seed: Zeroizing<[u8; 32]> =
        Zeroizing::new(fresh[32..].try_into().expect("secret is 64 bytes"));
    Ok(IdentitySecret::from_parts(&ed, &seed).to_bytes().to_vec())
}

/// The 1,984-byte hybrid public key for an identity secret.
#[uniffi::export]
pub fn identity_public_key(identity_secret: Vec<u8>) -> Result<Vec<u8>, IdentityError> {
    Ok(secret(&identity_secret)?.public_key().as_bytes().to_vec())
}

/// The identity's 32-byte Ed25519 public key: what a server's
/// `AURA_BOOTSTRAP_ADMIN_KEY` names, as 64 hex characters.
#[uniffi::export]
pub fn identity_ed25519_public_key(identity_secret: Vec<u8>) -> Result<Vec<u8>, IdentityError> {
    Ok(secret(&identity_secret)?
        .public_key()
        .ed25519_bytes()
        .to_vec())
}

/// The complete login frame, `[0x03][u32 LE length][AuthRequest]`, signed
/// over the server's `challenge` (from its `[0x02][32 bytes]` hello).
#[uniffi::export]
pub fn build_auth_request(
    identity_secret: Vec<u8>,
    display_name: String,
    challenge: Vec<u8>,
    server_password: Option<String>,
) -> Result<Vec<u8>, IdentityError> {
    let identity = secret(&identity_secret)?;
    let challenge: [u8; AUTH_CHALLENGE_LEN] =
        challenge
            .as_slice()
            .try_into()
            .map_err(|_| IdentityError::BadChallenge {
                got: challenge.len() as u32,
            })?;
    let public_key = identity.public_key();
    let transcript = auth_transcript(&challenge, &display_name, &public_key);
    let signature = identity.sign(Purpose::Auth, &transcript);

    let payload = ProtoAuthRequest {
        identity_public_key: public_key.as_bytes().to_vec(),
        display_name,
        signature: signature.as_bytes().to_vec(),
        server_password,
        challenge: challenge.to_vec(),
        client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        protocol_version: Some(aura_protocol::PROTOCOL_VERSION),
    }
    .encode_to_vec();

    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(MSG_AUTH_REQUEST);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// The server's reply to a login request.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AuthResponseRecord {
    pub success: bool,
    /// Session ID for datagrams and presence.
    pub session_id: u32,
    pub session_token: String,
    /// The account's server-assigned id (`U_…`). Pass it to `MlsWrapper`.
    pub user_uuid: String,
    pub verified: bool,
    pub is_admin: bool,
    /// Why login failed (empty on success).
    pub error_message: String,
}

/// Parse the payload of a `[0x04][u32 LE length][AuthResponse]` frame (the
/// bytes after the length).
#[uniffi::export]
pub fn decode_auth_response(payload: Vec<u8>) -> Result<AuthResponseRecord, IdentityError> {
    let r = ProtoAuthResponse::decode(payload.as_slice()).map_err(|e| {
        IdentityError::MalformedResponse {
            message: e.to_string(),
        }
    })?;
    Ok(AuthResponseRecord {
        success: r.success,
        session_id: r.session_id,
        session_token: r.session_token,
        user_uuid: r.user_uuid,
        verified: r.verified,
        is_admin: r.is_admin,
        error_message: r.error_message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aura_identity::{IdentityPublicKey, IdentitySignature};

    #[test]
    fn auth_request_frame_carries_a_verifiable_signature() {
        let secret_bytes = generate_identity_secret();
        assert_eq!(secret_bytes.len(), 64);
        let challenge = vec![0x5Au8; 32];
        let frame = build_auth_request(
            secret_bytes.clone(),
            "alice".into(),
            challenge.clone(),
            Some("pw".into()),
        )
        .unwrap();

        assert_eq!(frame[0], MSG_AUTH_REQUEST);
        let len = u32::from_le_bytes(frame[1..5].try_into().unwrap()) as usize;
        assert_eq!(len, frame.len() - 5);
        let req = ProtoAuthRequest::decode(&frame[5..]).unwrap();
        assert_eq!(req.display_name, "alice");
        assert_eq!(req.server_password.as_deref(), Some("pw"));
        assert_eq!(req.challenge, challenge);
        assert_eq!(
            req.identity_public_key,
            identity_public_key(secret_bytes).unwrap()
        );

        let pk = IdentityPublicKey::from_bytes(&req.identity_public_key).unwrap();
        let sig = IdentitySignature::from_bytes(&req.signature).unwrap();
        let transcript = auth_transcript(&[0x5A; 32], "alice", &pk);
        assert_eq!(pk.verify(Purpose::Auth, &transcript, &sig), Ok(()));
    }

    #[test]
    fn rejects_a_wrong_length_challenge_or_secret() {
        assert!(matches!(
            build_auth_request(generate_identity_secret(), "a".into(), vec![0; 31], None),
            Err(IdentityError::BadChallenge { got: 31 })
        ));
        assert!(matches!(
            build_auth_request(vec![0; 63], "a".into(), vec![0; 32], None),
            Err(IdentityError::InvalidSecret { .. })
        ));
    }

    #[test]
    fn upgrade_keeps_the_ed25519_key() {
        let ed_secret = [0x07u8; 32];
        let legacy = ed25519_dalek::SigningKey::from_bytes(&ed_secret);
        let upgraded = upgrade_ed25519_identity(ed_secret.to_vec()).unwrap();
        assert_eq!(upgraded.len(), 64);
        assert_eq!(&upgraded[..32], &ed_secret);
        assert_eq!(
            identity_ed25519_public_key(upgraded).unwrap(),
            legacy.verifying_key().to_bytes()
        );
        assert!(upgrade_ed25519_identity(vec![0; 31]).is_err());
    }

    #[test]
    fn decodes_auth_responses() {
        let payload = ProtoAuthResponse {
            success: true,
            session_id: 7,
            session_token: "tok".into(),
            verified: true,
            error_message: String::new(),
            user_uuid: "U_1".into(),
            is_admin: false,
            server_version: None,
            protocol_version: None,
        }
        .encode_to_vec();
        let r = decode_auth_response(payload).unwrap();
        assert!(r.success && r.verified && !r.is_admin);
        assert_eq!((r.session_id, r.user_uuid.as_str()), (7, "U_1"));
        assert!(decode_auth_response(vec![0xFF, 0xFF]).is_err());
    }
}
