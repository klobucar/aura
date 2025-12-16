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

impl Position {
    pub fn distance(&self, other: &Position) -> f32 {
        ((self.x - other.x).powi(2) + 
         (self.y - other.y).powi(2) + 
         (self.z - other.z).powi(2)).sqrt()
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
            group_id: 202,
            group_type: MlsGroupType::Voice as i32,
            epoch: 5,
            content: Some(mls_envelope::Content::Commit(vec![10, 20, 30])),
        };

        // Serialize
        let bytes = original.encode_to_vec();

        // Deserialize
        let decoded = MlsEnvelope::decode(bytes.as_slice()).expect("Failed to decode");

        assert_eq!(decoded.sender_id, original.sender_id);
        assert_eq!(decoded.group_id, original.group_id);
        assert_eq!(decoded.epoch, original.epoch);
        
        match decoded.content {
            Some(mls_envelope::Content::Commit(data)) => {
                assert_eq!(data, vec![10, 20, 30]);
            },
            _ => panic!("Wrong content type"),
        }
    }

    #[test]
    fn test_text_message_roundtrip() {
        let original = TextMessage {
            sender_uuid: "user-uuid-123".into(),
            timestamp: 123456789,
            content: "Hello World".into(),
            reply_to_id: "none".into(),
            message_id: "msg-id-555".into(),
        };

        // Serialize
        let bytes = original.encode_to_vec();

        // Deserialize
        let decoded = TextMessage::decode(bytes.as_slice()).expect("Failed to decode");

        assert_eq!(decoded.content, "Hello World");
        assert_eq!(decoded.timestamp, 123456789);
    }
}
