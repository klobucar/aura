//! Text Message Encryption/Decryption using DAVE Protocol
//!
//! Provides encryption and decryption for text messages using the same
//! DAVE (XChaCha20-Poly1305) encryption as audio, derived from MLS group secrets.

use crate::crypto::{DaveCrypto, CryptoError, NONCE_SIZE};
use prost::Message;

/// Re-export text message types from protocol
pub use aura_protocol::{TextMessage, EncryptedTextPacket};

/// Encrypt a TextMessage using the DAVE key from the current MLS epoch
pub fn encrypt_text(
    dave_key: &[u8; 32],
    epoch: u64,
    channel_id: String,
    sender_session_id: u32,
    message: &TextMessage,
) -> Result<EncryptedTextPacket, CryptoError> {
    // Serialize the plaintext message
    let plaintext = message.encode_to_vec();
    
    // Create DAVE crypto context
    let crypto = DaveCrypto::new(dave_key);
    
    // Generate random nonce
    let nonce = DaveCrypto::random_nonce();
    
    // Encrypt using DAVE (includes zero-padding commitment)
    let ciphertext = crypto.encrypt(&plaintext, &nonce)?;
    
    // Extract the auth tag from the ciphertext (last 16 bytes)
    let tag_offset = ciphertext.len() - 16;
    let tag = ciphertext[tag_offset..].to_vec();
    let ciphertext_without_tag = ciphertext[..tag_offset].to_vec();
    
    Ok(EncryptedTextPacket {
        sender_session_id,
        channel_id: channel_id.clone(),
        epoch,
        message_id: message.message_id.clone(),
        ciphertext: ciphertext_without_tag,
        nonce: nonce.to_vec(),
        tag,
        reply_to_id: message.reply_to_id.clone(),
    })
}

/// Decrypt an EncryptedTextPacket using the DAVE key
pub fn decrypt_text(
    dave_key: &[u8; 32],
    packet: &EncryptedTextPacket,
) -> Result<TextMessage, CryptoError> {
    // Create DAVE crypto context
    let crypto = DaveCrypto::new(dave_key);
    
    // Convert nonce to fixed-size array
    let nonce: [u8; NONCE_SIZE] = packet.nonce.as_slice()
        .try_into()
        .map_err(|_| CryptoError::InvalidNonceLength(packet.nonce.len()))?;
    
    // Reconstruct ciphertext with tag appended (as expected by AEAD)
    let mut ciphertext_with_tag = packet.ciphertext.clone();
    ciphertext_with_tag.extend_from_slice(&packet.tag);
    
    // Decrypt using DAVE (verifies zero-padding commitment)
    let plaintext = crypto.decrypt(&ciphertext_with_tag, &nonce)?;
    
    // Deserialize the TextMessage
    TextMessage::decode(plaintext.as_slice())
        .map_err(|_| CryptoError::DecryptionFailed)
}

/// Create a new text message with auto-generated ID and timestamp
pub fn create_text_message(sender_uuid: &str, content: &str, reply_to: Option<&str>) -> TextMessage {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    
    let message_id = uuid::Uuid::new_v4().to_string();
    
    TextMessage {
        sender_uuid: sender_uuid.to_string(),
        timestamp,
        content: content.to_string(),
        reply_to_id: reply_to.unwrap_or("").to_string(),
        message_id,
        r#type: aura_protocol::MediaType::Text as i32,
        file_size: 0,
        sha256_hash: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [0x42u8; 32];
        let message = create_text_message("user-123", "Hello, world!", None);
        
        let packet = encrypt_text(&key, 1, "100".into(), 1, &message)
            .expect("Encryption should succeed");
        
        let decrypted = decrypt_text(&key, &packet)
            .expect("Decryption should succeed");
        
        assert_eq!(decrypted.sender_uuid, message.sender_uuid);
        assert_eq!(decrypted.content, message.content);
        assert_eq!(decrypted.timestamp, message.timestamp);
        assert_eq!(decrypted.message_id, message.message_id);
    }
    
    #[test]
    fn test_wrong_key_fails() {
        let key1 = [0x42u8; 32];
        let key2 = [0x43u8; 32];
        let message = create_text_message("user-123", "Secret message", None);
        
        let packet = encrypt_text(&key1, 1, "100".into(), 1, &message)
            .expect("Encryption should succeed");
        
        let result = decrypt_text(&key2, &packet);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_reply_to_message() {
        let key = [0x42u8; 32];
        let original_id = "msg-456";
        let message = create_text_message("user-123", "This is a reply", Some(original_id));
        
        let packet = encrypt_text(&key, 1, "100".into(), 1, &message)
            .expect("Encryption should succeed");
        
        let decrypted = decrypt_text(&key, &packet)
            .expect("Decryption should succeed");
        
        assert_eq!(decrypted.reply_to_id, original_id);
    }
    
    #[test]
    fn test_packet_metadata() {
        let key = [0x42u8; 32];
        let message = create_text_message("user-123", "Hello!", None);
        
        let packet = encrypt_text(&key, 5, "42".into(), 7, &message)
            .expect("Encryption should succeed");
        
        assert_eq!(packet.epoch, 5);
        assert_eq!(packet.channel_id, "42");
        assert_eq!(packet.sender_session_id, 7);
        assert_eq!(packet.nonce.len(), 24);
        assert_eq!(packet.tag.len(), 16);
    }
}
