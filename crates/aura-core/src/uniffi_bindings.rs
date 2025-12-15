//! UniFFI-compatible wrappers for the audio pipeline
//!
//! These wrapper types are used by the UDL-defined interfaces.
//! They provide a simpler API that works with UniFFI's scaffolding.

use std::sync::RwLock;
use bytes::Bytes;

use crate::audio_pipeline::{
    AudioSender as InternalSender,
    AudioReceiver as InternalReceiver,
    AudioPipelineError as InternalError,
};
use crate::crypto::KEY_SIZE;

/// Convert internal error to UniFFI-compatible string description
fn format_error(e: InternalError) -> String {
    match e {
        InternalError::Opus(o) => format!("Opus: {}", o),
        InternalError::Crypto(c) => format!("Crypto: {}", c),
        InternalError::PacketParse(p) => format!("Parse: {}", p),
        InternalError::UnknownSender(id) => format!("Unknown sender: {}", id),
    }
}

/// Audio sender wrapper - manages an InternalSender with thread safety
pub struct AudioSenderWrapper {
    inner: RwLock<InternalSender>,
}

impl AudioSenderWrapper {
    /// Create a new audio sender
    pub fn new(session_id: u32, key: &[u8]) -> Result<Self, String> {
        if key.len() != KEY_SIZE {
            return Err(format!("Invalid key size: expected {}, got {}", KEY_SIZE, key.len()));
        }
        
        let mut key_arr = [0u8; KEY_SIZE];
        key_arr.copy_from_slice(key);
        
        let inner = InternalSender::new(session_id, &key_arr)
            .map_err(format_error)?;
        
        Ok(Self {
            inner: RwLock::new(inner),
        })
    }
    
    /// Set current MLS epoch
    pub fn set_epoch(&self, epoch: u64) {
        if let Ok(inner) = self.inner.read() {
            inner.set_epoch(epoch);
        }
    }
    
    /// Encode and encrypt PCM audio
    pub fn process(&self, pcm: &[i16]) -> Result<Vec<u8>, String> {
        let inner = self.inner.read().map_err(|e| e.to_string())?;
        let bytes = inner.process(pcm).map_err(format_error)?;
        Ok(bytes.to_vec())
    }
    
    /// Get current sequence number
    pub fn sequence(&self) -> u16 {
        self.inner.read().map(|i| i.sequence()).unwrap_or(0)
    }
}

/// Audio receiver wrapper - manages an InternalReceiver with thread safety
pub struct AudioReceiverWrapper {
    inner: RwLock<InternalReceiver>,
}

/// Decoded frame with sender info
pub struct DecodedFrame {
    pub session_id: u32,
    pub pcm: Vec<i16>,
}

impl AudioReceiverWrapper {
    /// Create a new audio receiver
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(InternalReceiver::new()),
        }
    }
    
    /// Add a sender with their key
    pub fn add_sender(&self, session_id: u32, key: &[u8]) -> Result<(), String> {
        if key.len() != KEY_SIZE {
            return Err(format!("Invalid key size: expected {}, got {}", KEY_SIZE, key.len()));
        }
        
        let mut key_arr = [0u8; KEY_SIZE];
        key_arr.copy_from_slice(key);
        
        let inner = self.inner.read().map_err(|e| e.to_string())?;
        inner.add_sender(session_id, &key_arr).map_err(format_error)?;
        Ok(())
    }
    
    /// Remove a sender
    pub fn remove_sender(&self, session_id: u32) {
        if let Ok(inner) = self.inner.read() {
            inner.remove_sender(session_id);
        }
    }
    
    /// Process a received packet
    pub fn on_packet(&self, data: &[u8]) -> Result<(), String> {
        let inner = self.inner.read().map_err(|e| e.to_string())?;
        inner.on_packet(Bytes::copy_from_slice(data)).map_err(format_error)?;
        Ok(())
    }
    
    /// Pop decoded frames
    pub fn pop_decoded(&self) -> Vec<DecodedFrame> {
        self.inner.read()
            .map(|i| {
                i.pop_decoded()
                    .into_iter()
                    .map(|(session_id, pcm)| DecodedFrame { session_id, pcm })
                    .collect()
            })
            .unwrap_or_default()
    }
    
    /// Pop mixed audio
    pub fn pop_mixed(&self) -> Option<Vec<i16>> {
        self.inner.read().ok()?.pop_mixed()
    }
}

impl Default for AudioReceiverWrapper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::DaveCrypto;
    
    #[test]
    fn test_wrapper_roundtrip() {
        let key = DaveCrypto::random_key();
        let session_id = 42;
        
        let sender = AudioSenderWrapper::new(session_id, &key).expect("Create sender");
        let receiver = AudioReceiverWrapper::new();
        receiver.add_sender(session_id, &key).expect("Add sender");
        
        let pcm = vec![1000i16; 960];
        let packet = sender.process(&pcm).expect("Process");
        
        receiver.on_packet(&packet).expect("On packet");
        
        let decoded = receiver.pop_decoded();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].session_id, session_id);
    }
}
