//! Profile signing and verification.
//!
//! Profiles are the one payload Aura deliberately leaves in plaintext: the
//! server stores and serves them so clients can render other people without a
//! round of MLS. That makes them the one payload a hostile server could
//! *forge*, so they carry an Ed25519 signature over their own contents.
//!
//! Both the canonical byte layout and the signing live here rather than in each
//! client, for a reason that matters more than code reuse. Ed25519 interoperates
//! fine between CryptoKit and NSec; what silently diverges between two
//! independent implementations is the payload construction — field order,
//! length framing, how an empty field encodes. A one-byte disagreement produces
//! signatures that fail to verify only across platforms. One implementation
//! makes that impossible.
//!
//! # What a signature does and does not buy you
//!
//! Verifying against the `signing_key` carried *inside* a profile proves
//! nothing: a hostile server swaps the key and the signature together. Callers
//! must verify against the key they have pinned for that `user_id` from an
//! earlier sighting — see [`verify_profile`], which takes the pinned key as a
//! separate argument specifically so this cannot be got wrong by accident.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

/// Domain separation tag. Keeps a profile signature from ever being accepted as
/// a signature over some other Aura structure (an auth challenge, say).
const PROFILE_DOMAIN: &[u8] = b"aura-profile-v1";

/// Length of an Ed25519 secret key seed.
pub const SECRET_KEY_LEN: usize = 32;
/// Length of an Ed25519 public key.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Length of an Ed25519 signature.
pub const SIGNATURE_LEN: usize = 64;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileError {
    #[error("Invalid key length: expected {expected}, got {got}")]
    InvalidKeyLength { expected: usize, got: usize },
    #[error("Invalid signature length: expected {SIGNATURE_LEN}, got {0}")]
    InvalidSignatureLength(usize),
    #[error("Malformed public key")]
    MalformedPublicKey,
    #[error("Signature does not verify against the pinned key for this user")]
    BadSignature,
    /// The profile claims to be for a different user than the one whose pinned
    /// key we are checking against. Blocks replaying one user's signed profile
    /// onto another identity.
    #[error("Profile is for user {claimed}, expected {expected}")]
    UserMismatch { claimed: String, expected: String },
    /// A correctly-signed but *older* profile. Without this check a server can
    /// replay a previous blob to roll a profile back — the signature is still
    /// perfectly valid, which is exactly why version has to be checked
    /// separately.
    #[error("Profile version {offered} is older than the cached version {cached}")]
    StaleVersion { offered: u64, cached: u64 },
}

/// The fields a signature covers.
///
/// `version` is a per-user monotonic counter. It is inside the signed bytes so
/// the server cannot renumber it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFields {
    pub user_id: String,
    pub display_name: String,
    pub bio: String,
    pub avatar_data: Vec<u8>,
    pub banner_data: Vec<u8>,
    pub version: u64,
}

impl ProfileFields {
    /// Canonical bytes to sign.
    ///
    /// Every field is length-prefixed. Without that, `("ab", "c")` and
    /// `("a", "bc")` would serialise identically and a signature over one would
    /// verify against the other — letting a server shuffle content across field
    /// boundaries while keeping the signature intact.
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            PROFILE_DOMAIN.len()
                + 8
                + self.user_id.len()
                + self.display_name.len()
                + self.bio.len()
                + self.avatar_data.len()
                + self.banner_data.len()
                + 5 * 8,
        );

        out.extend_from_slice(PROFILE_DOMAIN);
        out.extend_from_slice(&self.version.to_le_bytes());

        for field in [
            self.user_id.as_bytes(),
            self.display_name.as_bytes(),
            self.bio.as_bytes(),
            self.avatar_data.as_slice(),
            self.banner_data.as_slice(),
        ] {
            out.extend_from_slice(&(field.len() as u64).to_le_bytes());
            out.extend_from_slice(field);
        }

        out
    }

    /// Content hash of the canonical payload.
    ///
    /// Sent in `ServerState` in place of the profile body so clients can decide
    /// what they actually need to fetch, and used as the cache key. Because it
    /// digests the signed bytes, a changed hash always means changed content.
    pub fn content_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.signing_payload());
        hasher.finalize().into()
    }
}

/// Sign a profile with the user's Ed25519 secret key.
pub fn sign_profile(
    fields: &ProfileFields,
    secret_key: &[u8],
) -> Result<[u8; SIGNATURE_LEN], ProfileError> {
    let seed: [u8; SECRET_KEY_LEN] =
        secret_key
            .try_into()
            .map_err(|_| ProfileError::InvalidKeyLength {
                expected: SECRET_KEY_LEN,
                got: secret_key.len(),
            })?;

    let signing_key = SigningKey::from_bytes(&seed);
    Ok(signing_key.sign(&fields.signing_payload()).to_bytes())
}

/// Verify a profile against the key **pinned** for that user.
///
/// `pinned_public_key` must come from the caller's own TOFU store, not from the
/// profile being checked. `expected_user_id` is the identity that key is pinned
/// against; passing it lets this reject a valid signature that belongs to a
/// different user.
pub fn verify_profile(
    fields: &ProfileFields,
    signature: &[u8],
    pinned_public_key: &[u8],
    expected_user_id: &str,
) -> Result<(), ProfileError> {
    if fields.user_id != expected_user_id {
        return Err(ProfileError::UserMismatch {
            claimed: fields.user_id.clone(),
            expected: expected_user_id.to_string(),
        });
    }

    let key_bytes: [u8; PUBLIC_KEY_LEN] =
        pinned_public_key
            .try_into()
            .map_err(|_| ProfileError::InvalidKeyLength {
                expected: PUBLIC_KEY_LEN,
                got: pinned_public_key.len(),
            })?;

    let sig_bytes: [u8; SIGNATURE_LEN] = signature
        .try_into()
        .map_err(|_| ProfileError::InvalidSignatureLength(signature.len()))?;

    let verifying_key =
        VerifyingKey::from_bytes(&key_bytes).map_err(|_| ProfileError::MalformedPublicKey)?;

    verifying_key
        .verify(
            &fields.signing_payload(),
            &Signature::from_bytes(&sig_bytes),
        )
        .map_err(|_| ProfileError::BadSignature)
}

/// Reject a profile that is older than one already held.
///
/// Separate from signature verification because a rolled-back profile is
/// correctly signed — only the version reveals it.
pub fn check_version(offered: u64, cached: Option<u64>) -> Result<(), ProfileError> {
    match cached {
        Some(cached) if offered < cached => Err(ProfileError::StaleVersion { offered, cached }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair(seed_byte: u8) -> ([u8; 32], [u8; 32]) {
        let seed = [seed_byte; 32];
        let signing = SigningKey::from_bytes(&seed);
        (seed, signing.verifying_key().to_bytes())
    }

    fn fields() -> ProfileFields {
        ProfileFields {
            user_id: "user-abc".into(),
            display_name: "mira".into(),
            bio: "**runs the server**".into(),
            avatar_data: vec![1, 2, 3],
            banner_data: vec![4, 5],
            version: 1,
        }
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let (secret, public) = keypair(7);
        let f = fields();
        let sig = sign_profile(&f, &secret).expect("sign");
        assert_eq!(verify_profile(&f, &sig, &public, "user-abc"), Ok(()));
    }

    #[test]
    fn any_field_edit_breaks_the_signature() {
        let (secret, public) = keypair(7);
        let original = fields();
        let sig = sign_profile(&original, &secret).expect("sign");

        let mut mutations = vec![original.clone(); 6];
        mutations[0].display_name = "not-mira".into();
        mutations[1].bio = "something else".into();
        mutations[2].avatar_data = vec![9, 9, 9];
        mutations[3].banner_data = vec![];
        mutations[4].version = 2;
        mutations[5].user_id = "user-xyz".into();

        for (i, m) in mutations.iter().enumerate() {
            assert!(
                verify_profile(m, &sig, &public, &m.user_id).is_err(),
                "mutation {i} should not verify"
            );
        }
    }

    /// A different key must not verify — this is the case that matters when the
    /// server substitutes its own keypair along with a forged profile.
    #[test]
    fn a_substituted_key_does_not_verify() {
        let (server_secret, server_public) = keypair(99);
        let (_user_secret, user_public) = keypair(7);

        let mut forged = fields();
        forged.bio = "profile written by the server".into();
        let forged_sig = sign_profile(&forged, &server_secret).expect("sign");

        // Self-consistent against the server's own key...
        assert_eq!(
            verify_profile(&forged, &forged_sig, &server_public, "user-abc"),
            Ok(())
        );
        // ...but rejected against the key the client actually pinned.
        assert_eq!(
            verify_profile(&forged, &forged_sig, &user_public, "user-abc"),
            Err(ProfileError::BadSignature)
        );
    }

    /// One user's validly-signed profile must not be accepted for another user.
    #[test]
    fn a_profile_cannot_be_replayed_onto_another_identity() {
        let (secret, public) = keypair(7);
        let f = fields();
        let sig = sign_profile(&f, &secret).expect("sign");

        assert_eq!(
            verify_profile(&f, &sig, &public, "someone-else"),
            Err(ProfileError::UserMismatch {
                claimed: "user-abc".into(),
                expected: "someone-else".into(),
            })
        );
    }

    /// Length prefixes must stop content being shuffled across field boundaries
    /// while keeping the same signed bytes.
    #[test]
    fn field_boundaries_are_unambiguous() {
        let a = ProfileFields {
            user_id: "ab".into(),
            display_name: "c".into(),
            bio: String::new(),
            avatar_data: vec![],
            banner_data: vec![],
            version: 1,
        };
        let b = ProfileFields {
            user_id: "a".into(),
            display_name: "bc".into(),
            ..a.clone()
        };

        assert_ne!(a.signing_payload(), b.signing_payload());
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn signing_payload_is_deterministic() {
        let f = fields();
        assert_eq!(f.signing_payload(), f.clone().signing_payload());
        assert_eq!(f.content_hash(), f.clone().content_hash());
    }

    /// A correctly-signed older profile must be refused, or the server can roll
    /// a profile back by replaying a blob it kept.
    #[test]
    fn rollback_is_refused_even_though_the_signature_is_valid() {
        let (secret, public) = keypair(7);

        let mut v1 = fields();
        v1.version = 1;
        v1.bio = "old bio".into();
        let v1_sig = sign_profile(&v1, &secret).expect("sign v1");

        // The signature on the old blob is genuinely valid...
        assert_eq!(verify_profile(&v1, &v1_sig, &public, "user-abc"), Ok(()));
        // ...so only the version check catches the replay.
        assert_eq!(
            check_version(v1.version, Some(4)),
            Err(ProfileError::StaleVersion {
                offered: 1,
                cached: 4
            })
        );
        assert_eq!(check_version(5, Some(4)), Ok(()));
        assert_eq!(check_version(1, None), Ok(()));
    }

    #[test]
    fn wrong_length_inputs_are_rejected() {
        let f = fields();
        assert_eq!(
            sign_profile(&f, &[0u8; 16]),
            Err(ProfileError::InvalidKeyLength {
                expected: 32,
                got: 16
            })
        );
        assert_eq!(
            verify_profile(&f, &[0u8; 10], &[0u8; 32], "user-abc"),
            Err(ProfileError::InvalidSignatureLength(10))
        );
    }
}
