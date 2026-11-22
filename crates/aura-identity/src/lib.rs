//! Aura's long-term identity: a hybrid Ed25519 + ML-DSA-65 key pair.
//!
//! A user's identity is two keys used together. A signature is two signatures
//! over the same message, and it is valid only if **both** verify (an AND
//! combiner). Forging one therefore takes breaking Ed25519 *and* ML-DSA-65:
//! a quantum computer alone does not suffice, and neither does a classical
//! attack on the newer, less-studied ML-DSA.
//!
//! The construction follows the shape of draft-ietf-lamps-pq-composite-sigs
//! without claiming wire compatibility with it (nothing outside Aura verifies
//! these signatures):
//!
//! ```text
//! M' = PREFIX || len(label) || label || message
//! signature = ML-DSA-65.Sign(sk_mldsa, M', ctx = PREFIX) || Ed25519.Sign(sk_ed, M')
//! ```
//!
//! `label` names the [`Purpose`], so a signature made for one use (say, a
//! login challenge) never verifies as another (a profile or an MLS
//! credential). The ML-DSA context string ties its half to this composite
//! scheme, so it can't be lifted out and presented as a standalone ML-DSA
//! signature.
//!
//! Both halves are deterministic: the same key and message always give the
//! same signature, which keeps the known-answer tests exact.
//!
//! Secrets are 64 bytes: the 32-byte Ed25519 secret key followed by the
//! 32-byte ML-DSA-65 seed (FIPS 204 `ξ`). An existing Ed25519-only identity
//! upgrades by keeping its Ed25519 secret and adding a fresh seed
//! ([`IdentitySecret::from_parts`]), so its Ed25519 public key, and anything
//! derived from it, stays the same.

pub mod credential;

use ed25519_dalek::Signer as _;
use ml_dsa::{EncodedSignature, EncodedVerifyingKey, Keypair as _, MlDsa44, MlDsa65};
use rand::RngExt as _;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub use credential::MemberCredential;

/// Ed25519 public key length.
pub const ED25519_PUBLIC_KEY_LEN: usize = 32;
/// Ed25519 signature length.
pub const ED25519_SIGNATURE_LEN: usize = 64;
/// ML-DSA-65 public key length (FIPS 204).
pub const MLDSA65_PUBLIC_KEY_LEN: usize = 1952;
/// ML-DSA-65 signature length (FIPS 204).
pub const MLDSA65_SIGNATURE_LEN: usize = 3309;
/// Composite public key length: ML-DSA-65 key, then Ed25519 key.
pub const PUBLIC_KEY_LEN: usize = MLDSA65_PUBLIC_KEY_LEN + ED25519_PUBLIC_KEY_LEN;
/// Composite signature length: ML-DSA-65 signature, then Ed25519 signature.
pub const SIGNATURE_LEN: usize = MLDSA65_SIGNATURE_LEN + ED25519_SIGNATURE_LEN;
/// Serialized secret length: Ed25519 secret key, then ML-DSA-65 seed.
pub const SECRET_LEN: usize = 64;

/// Starts every signed message, and is the ML-DSA context string.
const PREFIX: &[u8] = b"aura-identity-v1 MLDSA65+Ed25519";

/// IANA MLS SignatureScheme code point for ML-DSA-44, the signature scheme
/// of Aura's MLS ciphersuite.
pub const MLS_SIGNATURE_SCHEME_MLDSA44: u16 = 0x0904;

/// Labels the derivation of MLS signing seeds from an identity secret.
const MLS_SEED_LABEL: &[u8] = b"aura-identity-v1 mls-signing-seed MLDSA44";

/// An ML-DSA-44 MLS signing key derived from an identity.
pub struct MlsSigningKey {
    /// FIPS 204 seed; what OpenMLS's basic credential signs with.
    pub seed: Zeroizing<[u8; 32]>,
    /// Encoded ML-DSA-44 public key (1,312 bytes).
    pub public_key: Vec<u8>,
}

/// What a signature is for. Each purpose gets its own label inside the
/// signed message, so signatures don't transfer between purposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Answering the server's login challenge.
    Auth,
    /// Signing a user profile.
    Profile,
    /// Certifying the MLS signature key that represents this identity in
    /// MLS groups (see [`MemberCredential`]).
    MlsCredential,
}

impl Purpose {
    fn label(self) -> &'static [u8] {
        match self {
            Purpose::Auth => b"auth",
            Purpose::Profile => b"profile",
            Purpose::MlsCredential => b"mls-credential",
        }
    }
}

/// The exact bytes both halves sign: `PREFIX || len(label) || label || message`.
fn signed_message(purpose: Purpose, message: &[u8]) -> Vec<u8> {
    let label = purpose.label();
    let mut m = Vec::with_capacity(PREFIX.len() + 1 + label.len() + message.len());
    m.extend_from_slice(PREFIX);
    m.push(label.len() as u8);
    m.extend_from_slice(label);
    m.extend_from_slice(message);
    m
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error("{what} must be {expected} bytes, got {got}")]
    BadLength {
        what: &'static str,
        expected: usize,
        got: usize,
    },
    #[error("the Ed25519 public key is not a valid curve point")]
    InvalidEd25519Key,
    #[error("signature verification failed")]
    BadSignature,
    #[error("malformed credential: {0}")]
    MalformedCredential(&'static str),
}

fn check_len(what: &'static str, bytes: &[u8], expected: usize) -> Result<(), IdentityError> {
    if bytes.len() == expected {
        Ok(())
    } else {
        Err(IdentityError::BadLength {
            what,
            expected,
            got: bytes.len(),
        })
    }
}

/// A user's private identity. Wipes its key material on drop.
pub struct IdentitySecret {
    ed25519: ed25519_dalek::SigningKey,
    mldsa: ml_dsa::SigningKey<MlDsa65>,
}

impl IdentitySecret {
    /// A new identity from the operating system's random number generator.
    pub fn generate() -> Self {
        let mut rng = rand::rng();
        let ed25519: Zeroizing<[u8; 32]> = Zeroizing::new(rng.random());
        let seed: Zeroizing<[u8; 32]> = Zeroizing::new(rng.random());
        Self::from_parts(&ed25519, &seed)
    }

    /// Build from an Ed25519 secret key and an ML-DSA-65 seed. Upgrading an
    /// Ed25519-only identity passes its existing secret with a new seed.
    pub fn from_parts(ed25519_secret: &[u8; 32], mldsa_seed: &[u8; 32]) -> Self {
        let seed = ml_dsa::Seed::from(*mldsa_seed);
        Self {
            ed25519: ed25519_dalek::SigningKey::from_bytes(ed25519_secret),
            mldsa: ml_dsa::SigningKey::<MlDsa65>::from_seed(&seed),
        }
    }

    /// Parse the 64-byte form produced by [`Self::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IdentityError> {
        check_len("identity secret", bytes, SECRET_LEN)?;
        let mut ed = Zeroizing::new([0u8; 32]);
        let mut seed = Zeroizing::new([0u8; 32]);
        ed.copy_from_slice(&bytes[..32]);
        seed.copy_from_slice(&bytes[32..]);
        Ok(Self::from_parts(&ed, &seed))
    }

    /// The 64-byte secret: Ed25519 secret key, then ML-DSA-65 seed.
    pub fn to_bytes(&self) -> Zeroizing<[u8; SECRET_LEN]> {
        let mut out = Zeroizing::new([0u8; SECRET_LEN]);
        out[..32].copy_from_slice(&self.ed25519.to_bytes());
        out[32..].copy_from_slice(self.mldsa.to_seed().as_slice());
        out
    }

    /// The matching public key.
    pub fn public_key(&self) -> IdentityPublicKey {
        let mut bytes = Vec::with_capacity(PUBLIC_KEY_LEN);
        bytes.extend_from_slice(self.mldsa.verifying_key().encode().as_slice());
        bytes.extend_from_slice(self.ed25519.verifying_key().as_bytes());
        IdentityPublicKey::from_bytes(&bytes).expect("own public key is well-formed")
    }

    /// The MLS signing key for `generation` (start at 0; bump to rotate).
    ///
    /// Derived rather than stored, so a client keeps one 64-byte secret. The
    /// derivation is one-way: an exposed MLS key reveals nothing about the
    /// identity secret. It is a one-step hash KDF:
    /// `SHA-256(u32 counter = 1 || secret || label || u32 generation)`.
    pub fn mls_signing_key(&self, generation: u32) -> MlsSigningKey {
        let secret = self.to_bytes();
        let mut h = Sha256::new();
        h.update(1u32.to_be_bytes());
        h.update(secret.as_slice());
        h.update(MLS_SEED_LABEL);
        h.update(generation.to_be_bytes());
        let mut seed = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(&h.finalize());

        let key = ml_dsa::SigningKey::<MlDsa44>::from_seed(&ml_dsa::Seed::from(*seed));
        MlsSigningKey {
            seed,
            public_key: key.verifying_key().encode().to_vec(),
        }
    }

    /// Sign `message` for `purpose`. Both halves are deterministic.
    pub fn sign(&self, purpose: Purpose, message: &[u8]) -> IdentitySignature {
        let m = signed_message(purpose, message);
        let mldsa = self
            .mldsa
            .expanded_key()
            .sign_deterministic(&m, PREFIX)
            .expect("PREFIX is a valid ML-DSA context (under 256 bytes)");
        let ed = self.ed25519.sign(&m);
        let mut bytes = Vec::with_capacity(SIGNATURE_LEN);
        bytes.extend_from_slice(mldsa.encode().as_slice());
        bytes.extend_from_slice(&ed.to_bytes());
        IdentitySignature { bytes }
    }
}

/// Length of the server's login challenge.
pub const AUTH_CHALLENGE_LEN: usize = 32;

/// What a client signs, for [`Purpose::Auth`], to log in:
/// `challenge (32) || len(display_name) (u16 BE) || display_name || identity public key`.
///
/// Binding the display name stops a relay from replaying the response under
/// another name. Binding the whole identity key matters when an account that
/// predates ML-DSA logs in: the server pins the ML-DSA key on the strength of
/// the account's existing Ed25519 key having signed this transcript.
pub fn auth_transcript(
    challenge: &[u8; AUTH_CHALLENGE_LEN],
    display_name: &str,
    identity: &IdentityPublicKey,
) -> Vec<u8> {
    let name = display_name.as_bytes();
    let mut t = Vec::with_capacity(AUTH_CHALLENGE_LEN + 2 + name.len() + PUBLIC_KEY_LEN);
    t.extend_from_slice(challenge);
    t.extend_from_slice(&(name.len().min(u16::MAX as usize) as u16).to_be_bytes());
    t.extend_from_slice(&name[..name.len().min(u16::MAX as usize)]);
    t.extend_from_slice(identity.as_bytes());
    t
}

/// A user's public identity: ML-DSA-65 key followed by Ed25519 key.
#[derive(Clone, PartialEq, Eq)]
pub struct IdentityPublicKey {
    bytes: Vec<u8>,
    ed25519: ed25519_dalek::VerifyingKey,
}

impl std::fmt::Debug for IdentityPublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "IdentityPublicKey(ed25519={:02x?}…)",
            &self.ed25519.as_bytes()[..4]
        )
    }
}

impl IdentityPublicKey {
    /// Parse a composite public key. Rejects a wrong length or an Ed25519
    /// key that is not a valid curve point.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IdentityError> {
        check_len("identity public key", bytes, PUBLIC_KEY_LEN)?;
        let ed: [u8; 32] = bytes[MLDSA65_PUBLIC_KEY_LEN..]
            .try_into()
            .expect("length checked above");
        let ed25519 = ed25519_dalek::VerifyingKey::from_bytes(&ed)
            .map_err(|_| IdentityError::InvalidEd25519Key)?;
        Ok(Self {
            bytes: bytes.to_vec(),
            ed25519,
        })
    }

    /// The 1,984-byte encoding.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The Ed25519 half. Aura derives a user's stable id from it, so it is
    /// what links an upgraded identity to its Ed25519-only past.
    pub fn ed25519_bytes(&self) -> [u8; 32] {
        self.ed25519.to_bytes()
    }

    /// Accept only if both the ML-DSA-65 and the Ed25519 signature verify.
    pub fn verify(
        &self,
        purpose: Purpose,
        message: &[u8],
        signature: &IdentitySignature,
    ) -> Result<(), IdentityError> {
        let m = signed_message(purpose, message);

        let encoded_vk: &EncodedVerifyingKey<MlDsa65> = self.bytes[..MLDSA65_PUBLIC_KEY_LEN]
            .try_into()
            .expect("length checked at construction");
        let vk = ml_dsa::VerifyingKey::<MlDsa65>::decode(encoded_vk);
        let encoded_sig: &EncodedSignature<MlDsa65> = signature.bytes[..MLDSA65_SIGNATURE_LEN]
            .try_into()
            .expect("length checked at construction");
        let mldsa_ok = ml_dsa::Signature::<MlDsa65>::decode(encoded_sig)
            .is_some_and(|sig| vk.verify_with_context(&m, PREFIX, &sig));

        let ed_sig: [u8; ED25519_SIGNATURE_LEN] = signature.bytes[MLDSA65_SIGNATURE_LEN..]
            .try_into()
            .expect("length checked at construction");
        let ed_ok = self
            .ed25519
            .verify_strict(&m, &ed25519_dalek::Signature::from_bytes(&ed_sig))
            .is_ok();

        // Evaluate both before deciding, so a failure doesn't reveal which half.
        if mldsa_ok & ed_ok {
            Ok(())
        } else {
            Err(IdentityError::BadSignature)
        }
    }
}

/// A composite signature: ML-DSA-65 signature followed by Ed25519 signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentitySignature {
    bytes: Vec<u8>,
}

impl IdentitySignature {
    /// Parse a composite signature (length check only; [`IdentityPublicKey::verify`]
    /// does the rest).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IdentityError> {
        check_len("identity signature", bytes, SIGNATURE_LEN)?;
        Ok(Self {
            bytes: bytes.to_vec(),
        })
    }

    /// The 3,373-byte encoding.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn fixed() -> IdentitySecret {
        IdentitySecret::from_parts(&[0x01; 32], &[0x02; 32])
    }

    #[test]
    fn sizes_match_the_constants() {
        let id = fixed();
        assert_eq!(id.public_key().as_bytes().len(), PUBLIC_KEY_LEN);
        assert_eq!(PUBLIC_KEY_LEN, 1984);
        assert_eq!(id.sign(Purpose::Auth, b"x").as_bytes().len(), SIGNATURE_LEN);
        assert_eq!(SIGNATURE_LEN, 3373);
    }

    #[test]
    fn sign_and_verify_round_trip_for_every_purpose() {
        let id = IdentitySecret::generate();
        let pk = id.public_key();
        for purpose in [Purpose::Auth, Purpose::Profile, Purpose::MlsCredential] {
            let sig = id.sign(purpose, b"hello");
            assert_eq!(pk.verify(purpose, b"hello", &sig), Ok(()));
        }
    }

    #[test]
    fn signatures_do_not_transfer_between_purposes() {
        let id = fixed();
        let sig = id.sign(Purpose::Auth, b"challenge");
        let pk = id.public_key();
        assert_eq!(
            pk.verify(Purpose::Profile, b"challenge", &sig),
            Err(IdentityError::BadSignature)
        );
        assert_eq!(
            pk.verify(Purpose::MlsCredential, b"challenge", &sig),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn rejects_a_changed_message() {
        let id = fixed();
        let sig = id.sign(Purpose::Auth, b"challenge");
        assert_eq!(
            id.public_key().verify(Purpose::Auth, b"challengf", &sig),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn both_halves_are_required() {
        let a = fixed();
        let b = IdentitySecret::from_parts(&[0x03; 32], &[0x04; 32]);
        let sig_a = a.sign(Purpose::Auth, b"m").as_bytes().to_vec();
        let sig_b = b.sign(Purpose::Auth, b"m").as_bytes().to_vec();

        // A's ML-DSA half with B's Ed25519 half, and the reverse.
        let mut mixed = sig_a[..MLDSA65_SIGNATURE_LEN].to_vec();
        mixed.extend_from_slice(&sig_b[MLDSA65_SIGNATURE_LEN..]);
        let mixed = IdentitySignature::from_bytes(&mixed).unwrap();
        assert_eq!(
            a.public_key().verify(Purpose::Auth, b"m", &mixed),
            Err(IdentityError::BadSignature)
        );

        let mut mixed = sig_b[..MLDSA65_SIGNATURE_LEN].to_vec();
        mixed.extend_from_slice(&sig_a[MLDSA65_SIGNATURE_LEN..]);
        let mixed = IdentitySignature::from_bytes(&mixed).unwrap();
        assert_eq!(
            a.public_key().verify(Purpose::Auth, b"m", &mixed),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn upgrade_keeps_the_ed25519_key() {
        let ed_secret = [0x05u8; 32];
        let legacy = ed25519_dalek::SigningKey::from_bytes(&ed_secret);
        let upgraded = IdentitySecret::from_parts(&ed_secret, &[0x06; 32]);
        assert_eq!(
            upgraded.public_key().ed25519_bytes(),
            legacy.verifying_key().to_bytes()
        );
    }

    #[test]
    fn secret_round_trips_through_bytes() {
        let id = IdentitySecret::generate();
        let again = IdentitySecret::from_bytes(id.to_bytes().as_slice()).unwrap();
        assert_eq!(id.public_key(), again.public_key());
        assert_eq!(
            again.sign(Purpose::Profile, b"p"),
            id.sign(Purpose::Profile, b"p")
        );
    }

    #[test]
    fn rejects_malformed_inputs() {
        assert!(matches!(
            IdentitySecret::from_bytes(&[0u8; 63]),
            Err(IdentityError::BadLength { .. })
        ));
        assert!(matches!(
            IdentityPublicKey::from_bytes(&[0u8; PUBLIC_KEY_LEN - 1]),
            Err(IdentityError::BadLength { .. })
        ));
        assert!(matches!(
            IdentitySignature::from_bytes(&[0u8; SIGNATURE_LEN + 1]),
            Err(IdentityError::BadLength { .. })
        ));

        // y = 2 has no x on the Ed25519 curve, so this is not a valid point.
        let mut bad = fixed().public_key().as_bytes().to_vec();
        bad[MLDSA65_PUBLIC_KEY_LEN..].copy_from_slice(&{
            let mut y = [0u8; 32];
            y[0] = 2;
            y
        });
        assert_eq!(
            IdentityPublicKey::from_bytes(&bad),
            Err(IdentityError::InvalidEd25519Key)
        );
    }

    #[test]
    fn auth_transcript_binds_challenge_name_and_key() {
        let id = fixed();
        let pk = id.public_key();
        let challenge = [0x11u8; AUTH_CHALLENGE_LEN];
        let t = auth_transcript(&challenge, "alice", &pk);
        assert_eq!(t.len(), 32 + 2 + 5 + PUBLIC_KEY_LEN);
        let sig = id.sign(Purpose::Auth, &t);
        assert_eq!(pk.verify(Purpose::Auth, &t, &sig), Ok(()));

        // Any change to the challenge, the name or the key breaks it.
        let other_challenge = auth_transcript(&[0x12; AUTH_CHALLENGE_LEN], "alice", &pk);
        let other_name = auth_transcript(&challenge, "mallory", &pk);
        let other_key = auth_transcript(
            &challenge,
            "alice",
            &IdentitySecret::from_parts(&[0x01; 32], &[0x09; 32]).public_key(),
        );
        for t2 in [other_challenge, other_name, other_key] {
            assert_eq!(
                pk.verify(Purpose::Auth, &t2, &sig),
                Err(IdentityError::BadSignature)
            );
        }
    }

    #[test]
    fn mls_signing_keys_are_derived_per_identity_and_generation() {
        let id = fixed();
        let k0 = id.mls_signing_key(0);
        assert_eq!(k0.public_key.len(), 1312);
        assert_eq!(*k0.seed, *id.mls_signing_key(0).seed, "deterministic");
        assert_eq!(k0.public_key, id.mls_signing_key(0).public_key);
        assert_ne!(
            *k0.seed,
            *id.mls_signing_key(1).seed,
            "generation rotates it"
        );
        let other = IdentitySecret::from_parts(&[0x03; 32], &[0x04; 32]);
        assert_ne!(*k0.seed, *other.mls_signing_key(0).seed, "per identity");

        // The derived key works as a certified MLS key.
        let cred = MemberCredential::issue(&id, "u", MLS_SIGNATURE_SCHEME_MLDSA44, &k0.public_key)
            .unwrap();
        assert_eq!(
            cred.verify(MLS_SIGNATURE_SCHEME_MLDSA44, &k0.public_key),
            Ok(())
        );
    }

    /// Pins the key derivation and signature format. If this changes, every
    /// stored identity and every signature already issued stops matching.
    #[test]
    fn known_answer() {
        let id = fixed();
        let pk = id.public_key();
        let sig = id.sign(Purpose::Auth, b"aura known-answer");
        assert_eq!(
            hex::encode(pk.ed25519_bytes()),
            "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
        );
        assert_eq!(
            hex::encode(Sha256::digest(pk.as_bytes())),
            "4727be130c3213e300a0611952557e8dc6db700af0440413770bc5c093cb6727"
        );
        assert_eq!(
            hex::encode(Sha256::digest(sig.as_bytes())),
            "2aa2bfb7795e92404c0bb6b04edb2dc6d1f36d87800d949cc7af7f3abe2958a0"
        );
        assert_eq!(pk.verify(Purpose::Auth, b"aura known-answer", &sig), Ok(()));
        assert_eq!(
            hex::encode(Sha256::digest(&id.mls_signing_key(0).public_key)),
            "5eb68568ac95a5bbc7ca443e78073c68316b4fe79edb1c11ab71494ed453b642"
        );
    }
}
