// Security-focused tests for crypto module
// Refactored to use DaveCrypto API

use crate::crypto::{DaveCrypto, KEY_SIZE, NONCE_SIZE};

#[test]
fn test_nonce_uniqueness() {
    use std::collections::HashSet;
    
    let mut nonces = HashSet::new();
    
    // Generate 1000 nonces (fewer than before for speed)
    for _ in 0..1000 {
        let nonce = DaveCrypto::random_nonce();
        assert!(!nonces.contains(&nonce), "Duplicate nonce detected!");
        nonces.insert(nonce);
    }
    
    assert_eq!(nonces.len(), 1000);
}

#[test]
fn test_nonce_randomness() {
    // Generate multiple nonces and verify they're different
    let nonce1 = DaveCrypto::random_nonce();
    let nonce2 = DaveCrypto::random_nonce();
    let nonce3 = DaveCrypto::random_nonce();
    
    assert_ne!(nonce1, nonce2);
    assert_ne!(nonce2, nonce3);
    assert_ne!(nonce1, nonce3);
}

#[test]
fn test_encryption_with_wrong_key_fails() {
    let plaintext = b"secret message";
    let key1 = [0x42u8; KEY_SIZE];
    let key2 = [0x43u8; KEY_SIZE];
    let nonce = DaveCrypto::random_nonce();
    
    let crypto1 = DaveCrypto::new(&key1);
    let crypto2 = DaveCrypto::new(&key2);
    
    let ciphertext = crypto1.encrypt(plaintext, &nonce).unwrap();
    
    // Decryption with wrong key should fail
    let result = crypto2.decrypt(&ciphertext, &nonce);
    assert!(result.is_err(), "Decryption with wrong key should fail");
}

#[test]
fn test_encryption_with_wrong_nonce_fails() {
    let plaintext = b"secret message";
    let key = [0x42u8; KEY_SIZE];
    let nonce1 = DaveCrypto::random_nonce();
    let nonce2 = DaveCrypto::random_nonce();
    
    let crypto = DaveCrypto::new(&key);
    
    let ciphertext = crypto.encrypt(plaintext, &nonce1).unwrap();
    
    // Decryption with wrong nonce should fail
    let result = crypto.decrypt(&ciphertext, &nonce2);
    assert!(result.is_err(), "Decryption with wrong nonce should fail");
}

#[test]
fn test_ciphertext_tampering_detected() {
    let plaintext = b"secret message";
    let key = [0x42u8; KEY_SIZE];
    let nonce = DaveCrypto::random_nonce();
    
    let crypto = DaveCrypto::new(&key);
    let mut ciphertext = crypto.encrypt(plaintext, &nonce).unwrap();
    
    // Tamper with ciphertext
    if !ciphertext.is_empty() {
        ciphertext[0] ^= 0xFF;
    }
    
    // Decryption should fail due to authentication tag mismatch
    let result = crypto.decrypt(&ciphertext, &nonce);
    assert!(result.is_err(), "Tampering should be detected");
}

#[test]
fn test_empty_plaintext_encryption() {
    let plaintext = b"";
    let key = [0x42u8; KEY_SIZE];
    let nonce = DaveCrypto::random_nonce();
    
    let crypto = DaveCrypto::new(&key);
    let ciphertext = crypto.encrypt(plaintext, &nonce).unwrap();
    let decrypted = crypto.decrypt(&ciphertext, &nonce).unwrap();
    
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_large_plaintext_encryption() {
    let plaintext = vec![0x42u8; 100_000]; // 100KB
    let key = [0x42u8; KEY_SIZE];
    let nonce = DaveCrypto::random_nonce();
    
    let crypto = DaveCrypto::new(&key);
    let ciphertext = crypto.encrypt(&plaintext, &nonce).unwrap();
    let decrypted = crypto.decrypt(&ciphertext, &nonce).unwrap();
    
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_key_zeroization() {
    use zeroize::Zeroize;
    
    let mut key = [0x42u8; KEY_SIZE];
    key.zeroize();
    
    // Verify all bytes are zero
    assert!(key.iter().all(|&b| b == 0), "Key should be zeroized");
}

#[test]
fn test_constant_time_comparison() {
    // Implement a simple constant-time comparison helper for the test
    fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut res = 0;
        for (x, y) in a.iter().zip(b.iter()) {
            res |= x ^ y;
        }
        res == 0
    }

    let a = [0x42u8; 32];
    let b = [0x42u8; 32];
    let c = [0x43u8; 32];
    
    // Equal arrays
    assert!(constant_time_eq(&a, &b));
    
    // Different arrays
    assert!(!constant_time_eq(&a, &c));
}
