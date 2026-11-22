//! The credential an Aura member presents in an MLS group.
//!
//! MLS signs with a separate key (ML-DSA-44, the scheme of Aura's MLS
//! ciphersuite). A [`MemberCredential`] ties that key to the member's
//! long-term identity: it carries the identity public key and the identity's
//! signature over the user id, the MLS signature scheme and the MLS public
//! key. Peers verify it before trusting a group member, so the identity key,
//! not the MLS key, is what verification and trust decisions rest on. The
//! MLS key can then be rotated, or the MLS ciphersuite changed, without
//! touching the identity.
//!
//! Encoding (all lengths big-endian):
//!
//! ```text
//! version (u8 = 1) || len(user_id) (u8) || user_id (UTF-8)
//!   || identity public key (1,984 bytes) || certificate (3,373 bytes)
//! ```
//!
//! The certificate signs, for [`Purpose::MlsCredential`]:
//!
//! ```text
//! signature_scheme (u16) || len(user_id) (u8) || user_id || len(mls_key) (u16) || mls_key
//! ```

use crate::{
    IdentityError, IdentityPublicKey, IdentitySecret, IdentitySignature, Purpose, PUBLIC_KEY_LEN,
    SIGNATURE_LEN,
};

const VERSION: u8 = 1;

/// A member's identity plus its certificate over their MLS signature key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberCredential {
    user_id: String,
    identity: IdentityPublicKey,
    certificate: IdentitySignature,
}

fn certified_message(user_id: &str, signature_scheme: u16, mls_public_key: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(5 + user_id.len() + mls_public_key.len());
    m.extend_from_slice(&signature_scheme.to_be_bytes());
    m.push(user_id.len() as u8);
    m.extend_from_slice(user_id.as_bytes());
    m.extend_from_slice(&(mls_public_key.len() as u16).to_be_bytes());
    m.extend_from_slice(mls_public_key);
    m
}

fn check_user_id(user_id: &str) -> Result<(), IdentityError> {
    if user_id.is_empty() || user_id.len() > u8::MAX as usize {
        return Err(IdentityError::MalformedCredential(
            "user id must be 1-255 bytes",
        ));
    }
    Ok(())
}

impl MemberCredential {
    /// Certify `mls_public_key` (an MLS signature key of `signature_scheme`,
    /// the IANA MLS SignatureScheme code point) as belonging to `identity`.
    pub fn issue(
        identity: &IdentitySecret,
        user_id: &str,
        signature_scheme: u16,
        mls_public_key: &[u8],
    ) -> Result<Self, IdentityError> {
        check_user_id(user_id)?;
        if mls_public_key.len() > u16::MAX as usize {
            return Err(IdentityError::MalformedCredential("MLS key too long"));
        }
        let message = certified_message(user_id, signature_scheme, mls_public_key);
        Ok(Self {
            user_id: user_id.to_owned(),
            identity: identity.public_key(),
            certificate: identity.sign(Purpose::MlsCredential, &message),
        })
    }

    /// Check that this credential's identity certified `mls_public_key` for
    /// `signature_scheme` under this credential's user id.
    pub fn verify(
        &self,
        signature_scheme: u16,
        mls_public_key: &[u8],
    ) -> Result<(), IdentityError> {
        if mls_public_key.len() > u16::MAX as usize {
            return Err(IdentityError::BadSignature);
        }
        let message = certified_message(&self.user_id, signature_scheme, mls_public_key);
        self.identity
            .verify(Purpose::MlsCredential, &message, &self.certificate)
    }

    /// The member's user id (a UUID in Aura).
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The member's long-term identity key.
    pub fn identity(&self) -> &IdentityPublicKey {
        &self.identity
    }

    /// Serialize for an MLS `BasicCredential`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + self.user_id.len() + PUBLIC_KEY_LEN + SIGNATURE_LEN);
        out.push(VERSION);
        out.push(self.user_id.len() as u8);
        out.extend_from_slice(self.user_id.as_bytes());
        out.extend_from_slice(self.identity.as_bytes());
        out.extend_from_slice(self.certificate.as_bytes());
        out
    }

    /// Parse [`Self::to_bytes`] output. Does not verify the certificate;
    /// call [`Self::verify`] with the MLS key the credential is attached to.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IdentityError> {
        let (&version, rest) = bytes
            .split_first()
            .ok_or(IdentityError::MalformedCredential("empty"))?;
        if version != VERSION {
            return Err(IdentityError::MalformedCredential("unknown version"));
        }
        let (&id_len, rest) = rest
            .split_first()
            .ok_or(IdentityError::MalformedCredential("truncated"))?;
        let id_len = id_len as usize;
        if rest.len() != id_len + PUBLIC_KEY_LEN + SIGNATURE_LEN {
            return Err(IdentityError::MalformedCredential("wrong length"));
        }
        let (id, rest) = rest.split_at(id_len);
        let (pk, sig) = rest.split_at(PUBLIC_KEY_LEN);
        let user_id = std::str::from_utf8(id)
            .map_err(|_| IdentityError::MalformedCredential("user id is not UTF-8"))?;
        check_user_id(user_id)?;
        Ok(Self {
            user_id: user_id.to_owned(),
            identity: IdentityPublicKey::from_bytes(pk)?,
            certificate: IdentitySignature::from_bytes(sig)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "00000000-0000-4000-8000-000000000001";
    /// MLS SignatureScheme code point for ML-DSA-44.
    const MLDSA44: u16 = 0x0904;
    const MLDSA65: u16 = 0x0905;

    fn identity() -> IdentitySecret {
        IdentitySecret::from_parts(&[0x01; 32], &[0x02; 32])
    }

    #[test]
    fn issue_verify_and_round_trip() {
        let mls_key = [0xAB; 1312];
        let cred = MemberCredential::issue(&identity(), UUID, MLDSA44, &mls_key).unwrap();
        assert_eq!(cred.verify(MLDSA44, &mls_key), Ok(()));

        let bytes = cred.to_bytes();
        assert_eq!(bytes.len(), 2 + UUID.len() + PUBLIC_KEY_LEN + SIGNATURE_LEN);
        let parsed = MemberCredential::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, cred);
        assert_eq!(parsed.user_id(), UUID);
        assert_eq!(parsed.identity(), &identity().public_key());
        assert_eq!(parsed.verify(MLDSA44, &mls_key), Ok(()));
    }

    #[test]
    fn rejects_another_mls_key_or_scheme() {
        let cred = MemberCredential::issue(&identity(), UUID, MLDSA44, &[0xAB; 1312]).unwrap();
        assert_eq!(
            cred.verify(MLDSA44, &[0xAC; 1312]),
            Err(IdentityError::BadSignature)
        );
        assert_eq!(
            cred.verify(MLDSA65, &[0xAB; 1312]),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn rejects_a_changed_user_id() {
        let mls_key = [0xAB; 1312];
        let mut bytes = MemberCredential::issue(&identity(), UUID, MLDSA44, &mls_key)
            .unwrap()
            .to_bytes();
        // Last character of the UUID: '1' -> '2'.
        bytes[1 + UUID.len()] = b'2';
        let tampered = MemberCredential::from_bytes(&bytes).unwrap();
        assert_eq!(
            tampered.verify(MLDSA44, &mls_key),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn rejects_another_identity_claiming_the_certificate() {
        let mls_key = [0xAB; 1312];
        let cred = MemberCredential::issue(&identity(), UUID, MLDSA44, &mls_key).unwrap();
        let other = IdentitySecret::from_parts(&[0x03; 32], &[0x04; 32]).public_key();
        let mut bytes = cred.to_bytes();
        let start = 2 + UUID.len();
        bytes[start..start + PUBLIC_KEY_LEN].copy_from_slice(other.as_bytes());
        let swapped = MemberCredential::from_bytes(&bytes).unwrap();
        assert_eq!(
            swapped.verify(MLDSA44, &mls_key),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn rejects_malformed_encodings() {
        let good = MemberCredential::issue(&identity(), UUID, MLDSA44, &[0xAB; 1312])
            .unwrap()
            .to_bytes();

        assert!(MemberCredential::from_bytes(&[]).is_err());

        let mut wrong_version = good.clone();
        wrong_version[0] = 2;
        assert!(MemberCredential::from_bytes(&wrong_version).is_err());

        assert!(MemberCredential::from_bytes(&good[..good.len() - 1]).is_err());

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(MemberCredential::from_bytes(&trailing).is_err());

        let mut bad_utf8 = good.clone();
        bad_utf8[2] = 0xFF;
        assert!(MemberCredential::from_bytes(&bad_utf8).is_err());

        let mut empty_id = vec![1u8, 0];
        empty_id.extend_from_slice(&good[2 + UUID.len()..]);
        assert!(MemberCredential::from_bytes(&empty_id).is_err());
    }

    #[test]
    fn rejects_unusable_user_ids_at_issue() {
        let mls_key = [0xAB; 32];
        assert!(MemberCredential::issue(&identity(), "", MLDSA44, &mls_key).is_err());
        let long = "x".repeat(256);
        assert!(MemberCredential::issue(&identity(), &long, MLDSA44, &mls_key).is_err());
    }
}
