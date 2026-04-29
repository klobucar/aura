// Include the generated protobuf code
// Package: aura.v1alpha1 -> aura.v1alpha1.rs
pub mod aura {
    pub mod v1alpha1 {
        include!(concat!(env!("OUT_DIR"), "/aura.v1alpha1.rs"));
    }
}

// Re-export v1alpha1 types to top-level for convenience/compat
pub use aura::v1alpha1::*;

pub mod fast_header;
pub use fast_header::*;

/// Generate a consistent MLS group ID for a channel.
/// Format: {type}-v1-{channel_id}
/// This function is idempotent: if the ID is already formatted, it returns it as-is.
pub fn make_mls_group_id(channel_id: &str, is_voice: bool) -> String {
    let group_type = if is_voice { "voice" } else { "text" };
    let prefix = format!("{}-v1-", group_type);

    if channel_id.starts_with(&prefix) {
        channel_id.to_string()
    } else {
        format!("{}{}", prefix, channel_id)
    }
}

impl Position {
    pub fn distance(&self, other: &Position) -> f32 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2) + (self.z - other.z).powi(2))
            .sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn test_mls_envelope_roundtrip() {
        let original = MlsEnvelope {
            sender_id: 101,
            channel_id: "202".to_string(),
            group_type: MlsGroupType::Voice as i32,
            epoch: 5,
            target_session_id: 0,
            target_uuid: "".to_string(),
            content: Some(mls_envelope::Content::Commit(vec![10, 20, 30])),
        };

        // Serialize
        let bytes = original.encode_to_vec();

        // Deserialize
        let decoded = MlsEnvelope::decode(bytes.as_slice()).expect("Failed to decode");

        assert_eq!(decoded.sender_id, original.sender_id);
        assert_eq!(decoded.channel_id, original.channel_id);
        assert_eq!(decoded.epoch, original.epoch);

        match decoded.content {
            Some(mls_envelope::Content::Commit(data)) => {
                assert_eq!(data, vec![10, 20, 30]);
            }
            _ => panic!("Wrong content type"),
        }
    }

    #[test]
    fn test_text_message_roundtrip() {
        let original = TextMessage {
            sender_uuid: "user-uuid-123".into(),
            timestamp: 123456789,
            r#type: MediaType::Text as i32,
            content: "Hello World".into(),
            reply_to_id: "none".into(),
            message_id: "msg-id-555".into(),
            file_size: 0,
            sha256_hash: "".into(),
        };

        // Serialize
        let bytes = original.encode_to_vec();

        // Deserialize
        let decoded = TextMessage::decode(bytes.as_slice()).expect("Failed to decode");

        assert_eq!(decoded.content, "Hello World");
        assert_eq!(decoded.timestamp, 123456789);
    }
}
