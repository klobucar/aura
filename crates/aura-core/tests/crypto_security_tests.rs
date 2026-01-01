// Security-focused tests for crypto module
// Add to existing tests in crates/aura-core/src/crypto.rs

#[test]
fn test_nonce_uniqueness() {
    use std::collections::HashSet;
    
    let mut nonces = HashSet::new();
    
    // Generate 10000 nonces
    for _ in 0..10000 {
        let nonce = generate_nonce();
        assert!(!nonces.contains(&nonce), "Duplicate nonce detected!");
        nonces.insert(nonce);
    }
    
    assert_eq!(nonces.len(), 10000);
}

#[test]
fn test_nonce_randomness() {
    // Generate multiple nonces and verify they're different
    let nonce1 = generate_nonce();
    let nonce2 = generate_nonce();
    let nonce3 = generate_nonce();
    
    assert_ne!(nonce1, nonce2);
    assert_ne!(nonce2, nonce3);
    assert_ne!(nonce1, nonce3);
}

#[test]
fn test_key_derivation_deterministic() {
    let session_id = 12345u32;
    let seq = 100u64;
    let master_key = [0x42u8; 32];
    
    // Same inputs should produce same output
    let key1 = derive_per_sender_key(session_id, seq, &master_key);
    let key2 = derive_per_sender_key(session_id, seq, &master_key);
    
    assert_eq!(key1, key2);
}

#[test]
fn test_key_derivation_different_sessions() {
    let seq = 100u64;
    let master_key = [0x42u8; 32];
    
    let key1 = derive_per_sender_key(1, seq, &master_key);
    let key2 = derive_per_sender_key(2, seq, &master_key);
    
    assert_ne!(key1, key2, "Different sessions should produce different keys");
}

#[test]
fn test_key_derivation_different_sequences() {
    let session_id = 12345u32;
    let master_key = [0x42u8; 32];
    
    let key1 = derive_per_sender_key(session_id, 100, &master_key);
    let key2 = derive_per_sender_key(session_id, 101, &master_key);
    
    assert_ne!(key1, key2, "Different sequences should produce different keys");
}

#[test]
fn test_encryption_with_wrong_key_fails() {
    let plaintext = b"secret message";
    let key1 = [0x42u8; 32];
    let key2 = [0x43u8; 32];
    let nonce = generate_nonce();
    
    let ciphertext = encrypt_chacha20poly1305(plaintext, &key1, &nonce).unwrap();
    
    // Decryption with wrong key should fail
    let result = decrypt_chacha20poly1305(&ciphertext, &key2, &nonce);
    assert!(result.is_err(), "Decryption with wrong key should fail");
}

#[test]
fn test_encryption_with_wrong_nonce_fails() {
    let plaintext = b"secret message";
    let key = [0x42u8; 32];
    let nonce1 = generate_nonce();
    let nonce2 = generate_nonce();
    
    let ciphertext = encrypt_chacha20poly1305(plaintext, &key, &nonce1).unwrap();
    
    // Decryption with wrong nonce should fail
    let result = decrypt_chacha20poly1305(&ciphertext, &key, &nonce2);
    assert!(result.is_err(), "Decryption with wrong nonce should fail");
}

#[test]
fn test_ciphertext_tampering_detected() {
    let plaintext = b"secret message";
    let key = [0x42u8; 32];
    let nonce = generate_nonce();
    
    let mut ciphertext = encrypt_chacha20poly1305(plaintext, &key, &nonce).unwrap();
    
    // Tamper with ciphertext
    if !ciphertext.is_empty() {
        ciphertext[0] ^= 0xFF;
    }
    
    // Decryption should fail due to authentication tag mismatch
    let result = decrypt_chacha20poly1305(&ciphertext, &key, &nonce);
    assert!(result.is_err(), "Tampering should be detected");
}

#[test]
fn test_empty_plaintext_encryption() {
    let plaintext = b"";
    let key = [0x42u8; 32];
    let nonce = generate_nonce();
    
    let ciphertext = encrypt_chacha20poly1305(plaintext, &key, &nonce).unwrap();
    let decrypted = decrypt_chacha20poly1305(&ciphertext, &key, &nonce).unwrap();
    
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_large_plaintext_encryption() {
    let plaintext = vec![0x42u8; 1_000_000]; // 1MB
    let key = [0x42u8; 32];
    let nonce = generate_nonce();
    
    let ciphertext = encrypt_chacha20poly1305(&plaintext, &key, &nonce).unwrap();
    let decrypted = decrypt_chacha20poly1305(&ciphertext, &key, &nonce).unwrap();
    
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_key_zeroization() {
    use zeroize::Zeroize;
    
    let mut key = [0x42u8; 32];
    key.zeroize();
    
    // Verify all bytes are zero
    assert!(key.iter().all(|&b| b == 0), "Key should be zeroized");
}

#[test]
fn test_constant_time_comparison() {
    // This is a basic test - real constant-time verification requires timing analysis
    let a = [0x42u8; 32];
    let b = [0x42u8; 32];
    let c = [0x43u8; 32];
    
    // Equal arrays
    assert!(constant_time_eq(&a, &b));
    
    // Different arrays
    assert!(!constant_time_eq(&a, &c));
}

#[test]
fn test_session_id_overflow() {
    let max_session = u32::MAX;
    let seq = 100u64;
    let master_key = [0x42u8; 32];
    
    // Should handle max session ID without panic
    let key = derive_per_sender_key(max_session, seq, &master_key);
    assert_eq!(key.len(), 32);
}

#[test]
fn test_sequence_overflow() {
    let session_id = 12345u32;
    let max_seq = u64::MAX;
    let master_key = [0x42u8; 32];
    
    // Should handle max sequence without panic
    let key = derive_per_sender_key(session_id, max_seq, &master_key);
    assert_eq!(key.len(), 32);
}
