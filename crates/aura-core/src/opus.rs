//! Opus Audio Codec for Aura (High-Level API)
//!
//! Provides encoding and decoding of audio using the Opus 1.6 backend.
//! Supports DRED, 24-bit audio, and up to 96kHz (Opus HD).

use crate::opus16::{Opus16Encoder, Opus16Decoder, Application, Opus16Error};
use std::sync::Mutex;

/// Opus codec configuration
#[derive(Debug, Clone)]
pub struct OpusConfig {
    /// Sample rate (default: 48000 Hz)
    /// Supported: 8000, 12000, 16000, 24000, 48000, 96000
    pub sample_rate: u32,
    /// Channels (default: Mono)
    pub channels: u8,
    /// Frame duration in milliseconds (default: 20ms)
    pub frame_duration_ms: u32,
    /// Target bitrate in bits/second (default: 64000)
    pub bitrate: i32,
    /// Enable In-Band Forward Error Correction (default: true)
    pub inband_fec: bool,
    /// Expected packet loss percentage (0-100, default: 10)
    pub packet_loss_perc: u8,
    /// DRED (Deep Redundancy) duration in 10ms frames (0-100, default: 0)
    /// Requires libopus with ML features enabled.
    pub dred_duration_frames: i32,
}

impl Default for OpusConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            frame_duration_ms: 20,
            bitrate: 64000,
            inband_fec: true,
            packet_loss_perc: 10,
            dred_duration_frames: 0,
        }
    }
}

impl OpusConfig {
    /// Calculate samples per frame
    pub fn frame_size(&self) -> usize {
        (self.sample_rate * self.frame_duration_ms / 1000) as usize
    }
}

/// Opus encoder/decoder pair
/// 
/// High-level, thread-safe wrapper around the Opus 1.6 engine.
pub struct OpusCodec {
    encoder: Mutex<Opus16Encoder>,
    decoder: Mutex<Opus16Decoder>,
    config: OpusConfig,
}

impl OpusCodec {
    /// Create a new Opus codec with default VoIP settings
    pub fn new() -> Result<Self, OpusError> {
        Self::with_config(OpusConfig::default())
    }
    
    /// Create a new Opus codec with custom configuration
    pub fn with_config(config: OpusConfig) -> Result<Self, OpusError> {
        let mut encoder = Opus16Encoder::new(
            config.sample_rate,
            config.channels,
            Application::Voip,
        ).map_err(OpusError::from_opus16)?;
        
        encoder.set_bitrate(config.bitrate)
            .map_err(OpusError::from_opus16)?;
        
        encoder.set_inband_fec(config.inband_fec)
            .map_err(OpusError::from_opus16)?;
        
        encoder.set_packet_loss_perc(config.packet_loss_perc)
            .map_err(OpusError::from_opus16)?;
        
        if config.dred_duration_frames > 0 {
            // Logically ignore errors if DRED is not compiled in
            let _ = encoder.set_dred_duration(config.dred_duration_frames);
        }
        
        let decoder = Opus16Decoder::new(
            config.sample_rate,
            config.channels,
        ).map_err(OpusError::from_opus16)?;
        
        Ok(Self {
            encoder: Mutex::new(encoder),
            decoder: Mutex::new(decoder),
            config,
        })
    }
    
    /// Get the frame size in samples
    pub fn frame_size(&self) -> usize {
        self.config.frame_size()
    }
    
    /// Get the frame duration in milliseconds
    pub fn frame_duration_ms(&self) -> u32 {
        self.config.frame_duration_ms
    }
    
    /// Encode 16-bit PCM samples to Opus
    pub fn encode(&self, pcm: &[i16]) -> Result<Vec<u8>, OpusError> {
        let mut encoder = self.encoder.lock().map_err(|_| OpusError::LockFailed)?;
        
        let mut output = vec![0u8; 2048]; // Sufficient for most frames
        let len = encoder.encode(pcm, &mut output)
            .map_err(OpusError::from_opus16)?;
        
        output.truncate(len);
        Ok(output)
    }
    
    /// Encode 24-bit PCM samples to Opus
    /// 
    /// Samples should be in lower 24 bits of i32.
    pub fn encode24(&self, pcm: &[i32]) -> Result<Vec<u8>, OpusError> {
        let mut encoder = self.encoder.lock().map_err(|_| OpusError::LockFailed)?;
        
        let mut output = vec![0u8; 2048];
        let len = encoder.encode24(pcm, &mut output)
            .map_err(OpusError::from_opus16)?;
        
        output.truncate(len);
        Ok(output)
    }
    
    /// Encode f32 PCM samples to Opus
    pub fn encode_float(&self, pcm: &[f32]) -> Result<Vec<u8>, OpusError> {
        let mut encoder = self.encoder.lock().map_err(|_| OpusError::LockFailed)?;
        
        let mut output = vec![0u8; 2048];
        let len = encoder.encode_float(pcm, &mut output)
            .map_err(OpusError::from_opus16)?;
        
        output.truncate(len);
        Ok(output)
    }
    
    /// Decode Opus frame to 16-bit PCM
    pub fn decode(&self, opus: &[u8], fec: bool) -> Result<Vec<i16>, OpusError> {
        let mut decoder = self.decoder.lock().map_err(|_| OpusError::LockFailed)?;
        
        let mut output = vec![0i16; self.config.frame_size() * self.config.channels as usize];
        let samples = decoder.decode(opus, &mut output, fec)
            .map_err(OpusError::from_opus16)?;
        
        output.truncate(samples * self.config.channels as usize);
        Ok(output)
    }
    
    /// Decode Opus frame to 24-bit PCM
    pub fn decode24(&self, opus: &[u8], fec: bool) -> Result<Vec<i32>, OpusError> {
        let mut decoder = self.decoder.lock().map_err(|_| OpusError::LockFailed)?;
        
        let mut output = vec![0i32; self.config.frame_size() * self.config.channels as usize];
        let samples = decoder.decode24(opus, &mut output, fec)
            .map_err(OpusError::from_opus16)?;
        
        output.truncate(samples * self.config.channels as usize);
        Ok(output)
    }
    
    /// Decode Opus frame to float PCM
    pub fn decode_float(&self, opus: &[u8], fec: bool) -> Result<Vec<f32>, OpusError> {
        let mut decoder = self.decoder.lock().map_err(|_| OpusError::LockFailed)?;
        
        let mut output = vec![0f32; self.config.frame_size() * self.config.channels as usize];
        let samples = decoder.decode_float(opus, &mut output, fec)
            .map_err(OpusError::from_opus16)?;
        
        output.truncate(samples * self.config.channels as usize);
        Ok(output)
    }
    
    /// Packet Loss Concealment (PLC)
    pub fn decode_plc(&self) -> Result<Vec<i16>, OpusError> {
        let mut decoder = self.decoder.lock().map_err(|_| OpusError::LockFailed)?;
        
        let mut output = vec![0i16; self.config.frame_size() * self.config.channels as usize];
        let samples = decoder.decode_plc(&mut output)
            .map_err(OpusError::from_opus16)?;
        
        output.truncate(samples * self.config.channels as usize);
        Ok(output)
    }
    
    /// Set DRED duration (can be updated at runtime)
    pub fn set_dred_duration(&self, frames: i32) -> Result<(), OpusError> {
        let mut encoder = self.encoder.lock().map_err(|_| OpusError::LockFailed)?;
        encoder.set_dred_duration(frames).map_err(OpusError::from_opus16)
    }
}

/// Opus codec errors (mapped from Opus16Error)
#[derive(Debug, thiserror::Error)]
pub enum OpusError {
    #[error("Opus error: {0}")]
    Opus16(#[from] Opus16Error),
    
    #[error("Invalid sample rate: {0}")]
    InvalidSampleRate(u32),
    
    #[error("Invalid channel count: {0}")]
    InvalidChannels(u8),
    
    #[error("Failed to acquire lock")]
    LockFailed,
}

impl OpusError {
    fn from_opus16(err: Opus16Error) -> Self {
        Self::Opus16(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_codec_creation() {
        let codec = OpusCodec::new().expect("Failed to create codec");
        assert_eq!(codec.frame_size(), 960); 
    }
    
    #[test]
    fn test_24bit_roundtrip() {
        let config = OpusConfig {
            sample_rate: 48000,
            channels: 1,
            ..Default::default()
        };
        let codec = OpusCodec::with_config(config).unwrap();
        
        let pcm = vec![0i32; 960];
        let opus = codec.encode24(&pcm).unwrap();
        let decoded = codec.decode24(&opus, false).unwrap();
        assert_eq!(decoded.len(), 960);
    }
}
