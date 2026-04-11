//! DAVE Protocol Encryption
//!
//! Implements XChaCha20-Poly1305 AEAD encryption as specified in the DAVE protocol.
//! Includes zero-padding commitment to mitigate partitioning oracle attacks (TOB-DISCE2EC-5).

use chacha20poly1305::{
    aead::{Aead, KeyInit, AeadCore, OsRng},
    XChaCha20Poly1305, XNonce,
};
use bytes::Bytes;

/// Size of XChaCha20-Poly1305 nonce (192 bits = 24 bytes)
pub const NONCE_SIZE: usize = 24;

/// Size of XChaCha20-Poly1305 authentication tag (128 bits = 16 bytes)
pub const TAG_SIZE: usize = 16;

/// Size of zero-padding for commitment (DAVE protocol requirement)
pub const ZERO_PAD_SIZE: usize = 16;

/// Size of encryption key (256 bits = 32 bytes)
pub const KEY_SIZE: usize = 32;

/// DAVE encryption context for a single sender
/// 
/// Each MLS group member has their own encryption context with a unique key.
pub struct DaveCrypto {
    cipher: XChaCha20Poly1305,
}

impl DaveCrypto {
    /// Create a new encryption context with the given key
    pub fn new(key: &[u8; KEY_SIZE]) -> Self {
        let cipher = XChaCha20Poly1305::new(key.into());
        Self { cipher }
    }
    
    /// Generate a random key for testing
    pub fn random_key() -> [u8; KEY_SIZE] {
        use rand::RngExt;
        rand::rng().random()
    }
    
    /// Generate a random nonce
    pub fn random_nonce() -> [u8; NONCE_SIZE] {
        XChaCha20Poly1305::generate_nonce(&mut OsRng).into()
    }
    
    /// Encrypt audio data with DAVE zero-padding commitment
    /// 
    /// Protocol:
    /// 1. Prepend 16 bytes of zeros to plaintext (commitment)
    /// 2. Encrypt with XChaCha20-Poly1305
    /// 
    /// Returns: ciphertext (with 16-byte auth tag appended)
    pub fn encrypt(&self, plaintext: &[u8], nonce: &[u8; NONCE_SIZE]) -> Result<Vec<u8>, CryptoError> {
        // Zero-padding commitment (TOB-DISCE2EC-5 mitigation)
        let mut padded = vec![0u8; ZERO_PAD_SIZE];
        padded.extend_from_slice(plaintext);
        
        let nonce = XNonce::from_slice(nonce);
        self.cipher
            .encrypt(nonce, padded.as_ref())
            .map_err(|_| CryptoError::EncryptionFailed)
    }
    
    /// Decrypt audio data and verify DAVE zero-padding commitment
    /// 
    /// Protocol:
    /// 1. Decrypt with XChaCha20-Poly1305
    /// 2. Verify first 16 bytes are all zeros
    /// 3. Return remaining plaintext
    pub fn decrypt(&self, ciphertext: &[u8], nonce: &[u8; NONCE_SIZE]) -> Result<Vec<u8>, CryptoError> {
        let nonce = XNonce::from_slice(nonce);
        
        let padded = self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| CryptoError::DecryptionFailed)?;
        
        // Verify zero-padding commitment
        if padded.len() < ZERO_PAD_SIZE {
            return Err(CryptoError::InvalidPadding);
        }
        
        for byte in &padded[..ZERO_PAD_SIZE] {
            if *byte != 0 {
                return Err(CryptoError::InvalidPadding);
            }
        }
        
        // Return plaintext without padding
        Ok(padded[ZERO_PAD_SIZE..].to_vec())
    }
    
    /// Encrypt audio data and return as Bytes
    pub fn encrypt_to_bytes(&self, plaintext: &[u8], nonce: &[u8; NONCE_SIZE]) -> Result<Bytes, CryptoError> {
        self.encrypt(plaintext, nonce).map(Bytes::from)
    }
}

/// Crypto errors
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("Encryption failed")]
    EncryptionFailed,
    
    #[error("Decryption failed (invalid auth tag)")]
    DecryptionFailed,
    
    #[error("Invalid zero-padding commitment (potential attack or key mismatch)")]
    InvalidPadding,
    
    #[error("Invalid key size")]
    InvalidKeySize,
    
    #[error("Invalid nonce length: expected 24 bytes, got {0}")]
    InvalidNonceLength(usize),
    
    #[error("Invalid nonce size")]
    InvalidNonceSize,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = DaveCrypto::random_key();
        let crypto = DaveCrypto::new(&key);
        let nonce = DaveCrypto::random_nonce();
        
        let plaintext = b"Hello, Opus audio frame!";
        
        let ciphertext = crypto.encrypt(plaintext, &nonce).expect("Encrypt failed");
        let decrypted = crypto.decrypt(&ciphertext, &nonce).expect("Decrypt failed");
        
        assert_eq!(decrypted, plaintext);
    }
    
    #[test]
    fn test_ciphertext_includes_padding_and_tag() {
        let key = DaveCrypto::random_key();
        let crypto = DaveCrypto::new(&key);
        let nonce = DaveCrypto::random_nonce();
        
        let plaintext = b"test";
        let ciphertext = crypto.encrypt(plaintext, &nonce).expect("Encrypt failed");
        
        // Ciphertext = zero_pad(16) + plaintext(4) + tag(16) = 36 bytes
        assert_eq!(ciphertext.len(), ZERO_PAD_SIZE + plaintext.len() + TAG_SIZE);
    }
    
    #[test]
    fn test_wrong_key_fails() {
        let key1 = DaveCrypto::random_key();
        let key2 = DaveCrypto::random_key();
        let crypto1 = DaveCrypto::new(&key1);
        let crypto2 = DaveCrypto::new(&key2);
        let nonce = DaveCrypto::random_nonce();
        
        let plaintext = b"secret audio";
        let ciphertext = crypto1.encrypt(plaintext, &nonce).expect("Encrypt failed");
        
        // Decrypting with wrong key should fail
        let result = crypto2.decrypt(&ciphertext, &nonce);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_wrong_nonce_fails() {
        let key = DaveCrypto::random_key();
        let crypto = DaveCrypto::new(&key);
        let nonce1 = DaveCrypto::random_nonce();
        let nonce2 = DaveCrypto::random_nonce();
        
        let plaintext = b"secret audio";
        let ciphertext = crypto.encrypt(plaintext, &nonce1).expect("Encrypt failed");
        
        // Decrypting with wrong nonce should fail
        let result = crypto.decrypt(&ciphertext, &nonce2);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_tampered_ciphertext_fails() {
        let key = DaveCrypto::random_key();
        let crypto = DaveCrypto::new(&key);
        let nonce = DaveCrypto::random_nonce();
        
        let plaintext = b"secret audio";
        let mut ciphertext = crypto.encrypt(plaintext, &nonce).expect("Encrypt failed");
        
        // Tamper with ciphertext
        ciphertext[10] ^= 0xFF;
        
        // Decryption should fail
        let result = crypto.decrypt(&ciphertext, &nonce);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_empty_plaintext() {
        let key = DaveCrypto::random_key();
        let crypto = DaveCrypto::new(&key);
        let nonce = DaveCrypto::random_nonce();
        
        let plaintext = b"";
        let ciphertext = crypto.encrypt(plaintext, &nonce).expect("Encrypt failed");
        let decrypted = crypto.decrypt(&ciphertext, &nonce).expect("Decrypt failed");
        
        assert_eq!(decrypted, plaintext);
        // Ciphertext is just padding + tag
        assert_eq!(ciphertext.len(), ZERO_PAD_SIZE + TAG_SIZE);
    }
    
    #[test]
    fn test_large_audio_frame() {
        let key = DaveCrypto::random_key();
        let crypto = DaveCrypto::new(&key);
        let nonce = DaveCrypto::random_nonce();
        
        // Simulate ~200 byte Opus frame
        let plaintext = vec![0xABu8; 200];
        
        let ciphertext = crypto.encrypt(&plaintext, &nonce).expect("Encrypt failed");
        let decrypted = crypto.decrypt(&ciphertext, &nonce).expect("Decrypt failed");
        
        assert_eq!(decrypted, plaintext);
    }
    
    #[test]
    fn test_nonce_uniqueness() {
        // Random nonces should be unique
        let n1 = DaveCrypto::random_nonce();
        let n2 = DaveCrypto::random_nonce();
        assert_ne!(n1, n2);
    }
}
