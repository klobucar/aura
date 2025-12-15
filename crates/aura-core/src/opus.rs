//! Opus Audio Codec for Aura
//!
//! Provides encoding and decoding of audio using the Opus codec.
//! Configured for VoIP: 48kHz, mono, 20ms frames (960 samples).

use audiopus::{
    coder::{Encoder, Decoder},
    Application, Channels, SampleRate, Bitrate,
};
use std::sync::Mutex;

/// Opus codec configuration
pub struct OpusConfig {
    /// Sample rate (default: 48000 Hz)
    pub sample_rate: u32,
    /// Channels (default: Mono)
    pub channels: u8,
    /// Frame duration in milliseconds (default: 20ms)
    pub frame_duration_ms: u32,
    /// Target bitrate in bits/second (default: 64000)
    pub bitrate: i32,
}

impl Default for OpusConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            frame_duration_ms: 20,
            bitrate: 64000,
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
/// Thread-safe via internal mutexes.
pub struct OpusCodec {
    encoder: Mutex<Encoder>,
    decoder: Mutex<Decoder>,
    config: OpusConfig,
}

impl OpusCodec {
    /// Create a new Opus codec with default VoIP settings
    /// (48kHz, mono, 20ms frames, ~64kbps)
    pub fn new() -> Result<Self, OpusError> {
        Self::with_config(OpusConfig::default())
    }
    
    /// Create a new Opus codec with custom configuration
    pub fn with_config(config: OpusConfig) -> Result<Self, OpusError> {
        let sample_rate = match config.sample_rate {
            8000 => SampleRate::Hz8000,
            12000 => SampleRate::Hz12000,
            16000 => SampleRate::Hz16000,
            24000 => SampleRate::Hz24000,
            48000 => SampleRate::Hz48000,
            _ => return Err(OpusError::InvalidSampleRate(config.sample_rate)),
        };
        
        let channels = match config.channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => return Err(OpusError::InvalidChannels(config.channels)),
        };
        
        let mut encoder = Encoder::new(sample_rate, channels, Application::Voip)
            .map_err(OpusError::EncoderInit)?;
        
        // Set bitrate
        encoder.set_bitrate(Bitrate::BitsPerSecond(config.bitrate))
            .map_err(OpusError::EncoderConfig)?;
        
        let decoder = Decoder::new(sample_rate, channels)
            .map_err(OpusError::DecoderInit)?;
        
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
    
    /// Encode PCM samples to Opus
    /// 
    /// Input: `frame_size()` samples of i16 PCM
    /// Output: Compressed Opus frame (typically 40-160 bytes for voice)
    pub fn encode(&self, pcm: &[i16]) -> Result<Vec<u8>, OpusError> {
        if pcm.len() != self.config.frame_size() {
            return Err(OpusError::InvalidFrameSize {
                expected: self.config.frame_size(),
                got: pcm.len(),
            });
        }
        
        let mut output = vec![0u8; 256]; // Max Opus frame size
        
        let len = self.encoder
            .lock()
            .map_err(|_| OpusError::LockFailed)?
            .encode(pcm, &mut output)
            .map_err(OpusError::Encode)?;
        
        output.truncate(len);
        Ok(output)
    }
    
    /// Encode f32 PCM samples to Opus
    /// 
    /// Converts f32 [-1.0, 1.0] to i16 and encodes
    pub fn encode_float(&self, pcm: &[f32]) -> Result<Vec<u8>, OpusError> {
        // Convert f32 to i16
        let pcm_i16: Vec<i16> = pcm.iter()
            .map(|&s| {
                let clamped = s.clamp(-1.0, 1.0);
                (clamped * i16::MAX as f32) as i16
            })
            .collect();
        
        self.encode(&pcm_i16)
    }
    
    /// Decode Opus frame to PCM
    /// 
    /// Input: Opus compressed frame
    /// Output: `frame_size()` samples of i16 PCM
    pub fn decode(&self, opus: &[u8]) -> Result<Vec<i16>, OpusError> {
        let mut output = vec![0i16; self.config.frame_size()];
        
        let samples = self.decoder
            .lock()
            .map_err(|_| OpusError::LockFailed)?
            .decode(Some(opus), &mut output, false)
            .map_err(OpusError::Decode)?;
        
        output.truncate(samples);
        Ok(output)
    }
    
    /// Packet Loss Concealment (PLC)
    /// 
    /// Generate audio to fill gap when packet is lost.
    /// Call this instead of decode() when a packet is missing.
    pub fn decode_plc(&self) -> Result<Vec<i16>, OpusError> {
        let mut output = vec![0i16; self.config.frame_size()];
        
        let samples = self.decoder
            .lock()
            .map_err(|_| OpusError::LockFailed)?
            .decode(None::<&[u8]>, &mut output, false)
            .map_err(OpusError::Decode)?;
        
        output.truncate(samples);
        Ok(output)
    }
    
    /// Decode Opus frame to f32 PCM
    pub fn decode_float(&self, opus: &[u8]) -> Result<Vec<f32>, OpusError> {
        let pcm_i16 = self.decode(opus)?;
        
        Ok(pcm_i16.iter()
            .map(|&s| s as f32 / i16::MAX as f32)
            .collect())
    }
}

/// Opus codec errors
#[derive(Debug, thiserror::Error)]
pub enum OpusError {
    #[error("Invalid sample rate: {0}")]
    InvalidSampleRate(u32),
    
    #[error("Invalid channel count: {0}")]
    InvalidChannels(u8),
    
    #[error("Encoder initialization failed: {0}")]
    EncoderInit(audiopus::Error),
    
    #[error("Encoder configuration failed: {0}")]
    EncoderConfig(audiopus::Error),
    
    #[error("Decoder initialization failed: {0}")]
    DecoderInit(audiopus::Error),
    
    #[error("Encoding failed: {0}")]
    Encode(audiopus::Error),
    
    #[error("Decoding failed: {0}")]
    Decode(audiopus::Error),
    
    #[error("Invalid frame size: expected {expected}, got {got}")]
    InvalidFrameSize { expected: usize, got: usize },
    
    #[error("Failed to acquire lock")]
    LockFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_codec_creation() {
        let codec = OpusCodec::new().expect("Failed to create codec");
        assert_eq!(codec.frame_size(), 960); // 48000 * 20 / 1000
        assert_eq!(codec.frame_duration_ms(), 20);
    }
    
    #[test]
    fn test_encode_decode_roundtrip() {
        let codec = OpusCodec::new().expect("Failed to create codec");
        
        // Generate test tone (440Hz sine wave)
        let frame_size = codec.frame_size();
        let mut pcm: Vec<i16> = Vec::with_capacity(frame_size);
        for i in 0..frame_size {
            let t = i as f32 / 48000.0;
            let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            pcm.push((sample * 16000.0) as i16);
        }
        
        // Encode
        let opus = codec.encode(&pcm).expect("Encode failed");
        println!("Encoded {} samples to {} bytes", frame_size, opus.len());
        
        // Should be compressed (much smaller than PCM)
        assert!(opus.len() < frame_size * 2);
        assert!(opus.len() > 10); // Sanity check
        
        // Decode
        let decoded = codec.decode(&opus).expect("Decode failed");
        assert_eq!(decoded.len(), frame_size);
        
        // Lossy codec, but should be reasonably similar
        // Check that peak amplitude is preserved within 20%
        let original_peak = pcm.iter().map(|&s| s.abs()).max().unwrap();
        let decoded_peak = decoded.iter().map(|&s| s.abs()).max().unwrap();
        let ratio = decoded_peak as f32 / original_peak as f32;
        assert!(ratio > 0.8 && ratio < 1.2, "Peak ratio: {}", ratio);
    }
    
    #[test]
    fn test_encode_float() {
        let codec = OpusCodec::new().expect("Failed to create codec");
        
        // Generate float PCM
        let frame_size = codec.frame_size();
        let pcm: Vec<f32> = (0..frame_size)
            .map(|i| {
                let t = i as f32 / 48000.0;
                (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5
            })
            .collect();
        
        let opus = codec.encode_float(&pcm).expect("Encode failed");
        assert!(opus.len() > 10);
        // Opus frames are typically under 300 bytes even at high bitrates
        assert!(opus.len() < 500, "Opus frame unexpectedly large: {} bytes", opus.len());
    }
    
    #[test]
    fn test_packet_loss_concealment() {
        let codec = OpusCodec::new().expect("Failed to create codec");
        
        // First, encode and decode a normal frame
        let pcm = vec![0i16; codec.frame_size()];
        let opus = codec.encode(&pcm).expect("Encode failed");
        let _ = codec.decode(&opus).expect("Decode failed");
        
        // Now simulate packet loss
        let plc = codec.decode_plc().expect("PLC failed");
        assert_eq!(plc.len(), codec.frame_size());
    }
    
    #[test]
    fn test_compression_ratio() {
        let codec = OpusCodec::new().expect("Failed to create codec");
        
        // Silence (should compress very well)
        let silence = vec![0i16; codec.frame_size()];
        let opus_silence = codec.encode(&silence).expect("Encode failed");
        
        // Noise (won't compress as well)
        let noise: Vec<i16> = (0..codec.frame_size())
            .map(|i| ((i as i32 * 12345) % 32768) as i16)
            .collect();
        let opus_noise = codec.encode(&noise).expect("Encode failed");
        
        println!("Silence: {} bytes, Noise: {} bytes", opus_silence.len(), opus_noise.len());
        
        // Both should still be much smaller than raw PCM
        let raw_size = codec.frame_size() * 2; // 2 bytes per sample
        assert!(opus_silence.len() < raw_size / 10); // At least 10x compression for silence
        assert!(opus_noise.len() < raw_size / 5);    // At least 5x compression for noise
    }
}
