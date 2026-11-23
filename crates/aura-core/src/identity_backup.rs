//! Passphrase-encrypted identity backups.
//!
//! An Aura identity is a 64-byte secret, and the account a server pins to it
//! is first come, first served: lose the secret and the name on every server
//! stays taken by a key nobody holds. A backup has to exist, it has to be
//! safe to leave in a cloud drive, and a backup made on macOS has to restore
//! on Windows. So the format lives here, once, rather than in each client —
//! the same reason the profile signing payload does (see `profile.rs`).
//!
//! # Format (version 1)
//!
//! ```text
//! offset  len  field
//!      0    7  magic "AURAIDB"
//!      7    1  version = 1
//!      8    1  scrypt log2(N)
//!      9    4  scrypt r, big-endian
//!     13    4  scrypt p, big-endian
//!     17   16  salt (random)
//!     33   24  XChaCha20-Poly1305 nonce (random)
//!     57    …  ciphertext of (secret[64] || display name, UTF-8), plus 16-byte tag
//! ```
//!
//! The key is `scrypt(passphrase, salt, N, r, p)`, 32 bytes. Everything before
//! the ciphertext is the AEAD's associated data, so the KDF parameters cannot
//! be swapped (to weaken them) without the tag failing.
//!
//! A wrong passphrase and a damaged file are deliberately the same error: the
//! tag cannot tell them apart, and guessing would only mislead.

use aura_identity::{IdentitySecret, SECRET_LEN};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use scrypt::{scrypt, Params};
use zeroize::Zeroizing;

const MAGIC: &[u8; 7] = b"AURAIDB";
const VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const HEADER_LEN: usize = 7 + 1 + 1 + 4 + 4 + SALT_LEN + NONCE_LEN;
const TAG_LEN: usize = 16;
const KEY_LEN: usize = 32;

/// Cost for new backups: N = 2^17, r = 8, p = 1 — about 128 MiB and a few
/// hundred milliseconds in a release build. A backup is made and opened
/// rarely, so it can afford to be slow for whoever is guessing passphrases.
const LOG_N: u8 = 17;
const R: u32 = 8;
const P: u32 = 1;

/// Bounds accepted when opening a backup. The parameters come from the file,
/// so without a ceiling a crafted file could demand gigabytes of memory.
const MIN_LOG_N: u8 = 10;
const MAX_LOG_N: u8 = 20;
const MAX_R: u32 = 16;
const MAX_P: u32 = 4;

/// Longest display name a backup carries, in bytes.
const MAX_NAME_LEN: usize = 256;

#[derive(Debug, thiserror::Error, uniffi::Error, PartialEq, Eq)]
pub enum IdentityBackupError {
    #[error("This file is not an Aura identity backup")]
    NotABackup,
    #[error("This backup was made by a newer version of Aura (format {version})")]
    UnsupportedVersion { version: u8 },
    #[error("Wrong passphrase, or the backup file is damaged")]
    WrongPassphraseOrDamaged,
    #[error("The backup asks for key-derivation settings Aura refuses to run")]
    UnreasonableParameters,
    #[error("A passphrase is required")]
    EmptyPassphrase,
    #[error("The identity in this backup is not usable: {message}")]
    InvalidContents { message: String },
}

/// What a backup holds.
#[derive(Debug, Clone, uniffi::Record)]
pub struct IdentityBackupContents {
    /// The 64-byte identity secret. Store it like a private key.
    pub identity_secret: Vec<u8>,
    /// The display name the identity was backed up under.
    pub display_name: String,
}

/// Encrypt `identity_secret` and `display_name` under `passphrase`.
///
/// Expensive (scrypt at N = 2^17): call it off the UI thread.
#[uniffi::export]
pub fn encrypt_identity_backup(
    identity_secret: Vec<u8>,
    display_name: String,
    passphrase: String,
) -> Result<Vec<u8>, IdentityBackupError> {
    encrypt_with_params(&identity_secret, &display_name, &passphrase, LOG_N, R, P)
}

/// Open a backup made by [`encrypt_identity_backup`] on any platform.
///
/// Expensive (the file's scrypt cost): call it off the UI thread.
#[uniffi::export]
pub fn decrypt_identity_backup(
    backup: Vec<u8>,
    passphrase: String,
) -> Result<IdentityBackupContents, IdentityBackupError> {
    if passphrase.is_empty() {
        return Err(IdentityBackupError::EmptyPassphrase);
    }
    if backup.len() < HEADER_LEN + TAG_LEN + SECRET_LEN || &backup[..7] != MAGIC {
        return Err(IdentityBackupError::NotABackup);
    }
    let version = backup[7];
    if version != VERSION {
        return Err(IdentityBackupError::UnsupportedVersion { version });
    }

    let log_n = backup[8];
    let r = u32::from_be_bytes(backup[9..13].try_into().expect("4 bytes"));
    let p = u32::from_be_bytes(backup[13..17].try_into().expect("4 bytes"));
    if !(MIN_LOG_N..=MAX_LOG_N).contains(&log_n) || r == 0 || r > MAX_R || p == 0 || p > MAX_P {
        return Err(IdentityBackupError::UnreasonableParameters);
    }
    let salt = &backup[17..17 + SALT_LEN];
    let nonce =
        XNonce::from(<[u8; NONCE_LEN]>::try_from(&backup[33..33 + NONCE_LEN]).expect("24 bytes"));

    let key = derive_key(&passphrase, salt, log_n, r, p)?;
    let plaintext = Zeroizing::new(
        XChaCha20Poly1305::new((&*key).into())
            .decrypt(
                &nonce,
                Payload {
                    msg: &backup[HEADER_LEN..],
                    aad: &backup[..HEADER_LEN],
                },
            )
            .map_err(|_| IdentityBackupError::WrongPassphraseOrDamaged)?,
    );

    if plaintext.len() < SECRET_LEN || plaintext.len() - SECRET_LEN > MAX_NAME_LEN {
        return Err(IdentityBackupError::InvalidContents {
            message: "unexpected length".into(),
        });
    }
    let (secret, name) = plaintext.split_at(SECRET_LEN);
    IdentitySecret::from_bytes(secret).map_err(|e| IdentityBackupError::InvalidContents {
        message: e.to_string(),
    })?;
    let display_name =
        String::from_utf8(name.to_vec()).map_err(|_| IdentityBackupError::InvalidContents {
            message: "display name is not UTF-8".into(),
        })?;

    Ok(IdentityBackupContents {
        identity_secret: secret.to_vec(),
        display_name,
    })
}

fn encrypt_with_params(
    identity_secret: &[u8],
    display_name: &str,
    passphrase: &str,
    log_n: u8,
    r: u32,
    p: u32,
) -> Result<Vec<u8>, IdentityBackupError> {
    if passphrase.is_empty() {
        return Err(IdentityBackupError::EmptyPassphrase);
    }
    IdentitySecret::from_bytes(identity_secret).map_err(|e| {
        IdentityBackupError::InvalidContents {
            message: e.to_string(),
        }
    })?;
    if display_name.len() > MAX_NAME_LEN {
        return Err(IdentityBackupError::InvalidContents {
            message: format!("display name longer than {MAX_NAME_LEN} bytes"),
        });
    }

    let salt: [u8; SALT_LEN] = {
        use rand::RngExt;
        rand::rng().random()
    };
    let nonce_bytes: [u8; NONCE_LEN] = {
        use rand::RngExt;
        rand::rng().random()
    };

    let mut out = Vec::with_capacity(HEADER_LEN + SECRET_LEN + display_name.len() + TAG_LEN);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(log_n);
    out.extend_from_slice(&r.to_be_bytes());
    out.extend_from_slice(&p.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    debug_assert_eq!(out.len(), HEADER_LEN);

    let mut plaintext = Zeroizing::new(Vec::with_capacity(SECRET_LEN + display_name.len()));
    plaintext.extend_from_slice(identity_secret);
    plaintext.extend_from_slice(display_name.as_bytes());

    let key = derive_key(passphrase, &salt, log_n, r, p)?;
    let ciphertext = XChaCha20Poly1305::new((&*key).into())
        .encrypt(
            &XNonce::from(nonce_bytes),
            Payload {
                msg: &plaintext,
                aad: &out,
            },
        )
        .map_err(|_| IdentityBackupError::WrongPassphraseOrDamaged)?;
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn derive_key(
    passphrase: &str,
    salt: &[u8],
    log_n: u8,
    r: u32,
    p: u32,
) -> Result<Zeroizing<[u8; KEY_LEN]>, IdentityBackupError> {
    let params =
        Params::new(log_n, r, p).map_err(|_| IdentityBackupError::UnreasonableParameters)?;
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    scrypt(passphrase.as_bytes(), salt, &params, key.as_mut())
        .map_err(|_| IdentityBackupError::UnreasonableParameters)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap parameters so the tests don't spend seconds in an unoptimised scrypt.
    fn backup(secret: &[u8], name: &str, pass: &str) -> Vec<u8> {
        encrypt_with_params(secret, name, pass, MIN_LOG_N, 8, 1).unwrap()
    }

    fn secret() -> Vec<u8> {
        IdentitySecret::generate().to_bytes().to_vec()
    }

    #[test]
    fn round_trips_secret_and_name() {
        let s = secret();
        let file = backup(&s, "mira ✨", "correct horse battery staple");
        let opened = decrypt_identity_backup(file, "correct horse battery staple".into()).unwrap();
        assert_eq!(opened.identity_secret, s);
        assert_eq!(opened.display_name, "mira ✨");
    }

    #[test]
    fn same_inputs_give_different_files() {
        let s = secret();
        assert_ne!(backup(&s, "a", "pw"), backup(&s, "a", "pw"));
    }

    #[test]
    fn wrong_passphrase_is_rejected() {
        let file = backup(&secret(), "a", "right");
        assert_eq!(
            decrypt_identity_backup(file, "wrong".into()).unwrap_err(),
            IdentityBackupError::WrongPassphraseOrDamaged
        );
    }

    #[test]
    fn every_byte_is_authenticated() {
        let file = backup(&secret(), "a", "pw");
        // Header bytes outside the KDF parameters (magic/version/params have
        // their own checks): salt, nonce, ciphertext and tag.
        for i in 17..file.len() {
            let mut damaged = file.clone();
            damaged[i] ^= 0x01;
            assert!(
                decrypt_identity_backup(damaged, "pw".into()).is_err(),
                "flipping byte {i} went unnoticed"
            );
        }
    }

    #[test]
    fn weakened_parameters_fail_the_tag() {
        let mut file = encrypt_with_params(&secret(), "a", "pw", 12, 8, 1).unwrap();
        file[8] = MIN_LOG_N; // an attacker lowering the cost
        assert_eq!(
            decrypt_identity_backup(file, "pw".into()).unwrap_err(),
            IdentityBackupError::WrongPassphraseOrDamaged
        );
    }

    #[test]
    fn refuses_unreasonable_parameters() {
        let mut file = backup(&secret(), "a", "pw");
        file[8] = MAX_LOG_N + 1;
        assert_eq!(
            decrypt_identity_backup(file.clone(), "pw".into()).unwrap_err(),
            IdentityBackupError::UnreasonableParameters
        );
        file[8] = MIN_LOG_N;
        file[13..17].copy_from_slice(&(MAX_P + 1).to_be_bytes());
        assert_eq!(
            decrypt_identity_backup(file, "pw".into()).unwrap_err(),
            IdentityBackupError::UnreasonableParameters
        );
    }

    #[test]
    fn rejects_other_files_and_versions() {
        assert_eq!(
            decrypt_identity_backup(b"{\"version\":2}".to_vec(), "pw".into()).unwrap_err(),
            IdentityBackupError::NotABackup
        );
        let mut file = backup(&secret(), "a", "pw");
        file[7] = 2;
        assert_eq!(
            decrypt_identity_backup(file, "pw".into()).unwrap_err(),
            IdentityBackupError::UnsupportedVersion { version: 2 }
        );
    }

    #[test]
    fn rejects_empty_passphrase_and_bad_secret() {
        assert_eq!(
            encrypt_identity_backup(secret(), "a".into(), String::new()).unwrap_err(),
            IdentityBackupError::EmptyPassphrase
        );
        assert!(matches!(
            encrypt_with_params(&[1, 2, 3], "a", "pw", MIN_LOG_N, 8, 1).unwrap_err(),
            IdentityBackupError::InvalidContents { .. }
        ));
    }
}
