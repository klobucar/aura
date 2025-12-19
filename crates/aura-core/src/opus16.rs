//! Safe Rust wrapper for libopus 1.6
//!
//! Provides RAII wrappers for Opus encoder/decoder with:
//! - DRED (Deep Redundancy) support
//! - 24-bit encode/decode API
//! - 96kHz sample rate support (Opus HD)

use opus16_sys as ffi;
use std::ptr::NonNull;
use std::os::raw::c_int;
use std::marker::PhantomData;

// ============================================================================
// Error Types
// ============================================================================

/// Opus operation errors
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Opus16Error {
    #[error("invalid argument")]
    BadArg,
    #[error("buffer too small")]
    BufferTooSmall,
    #[error("internal error")]
    Internal,
    #[error("invalid packet")]
    InvalidPacket,
    #[error("unimplemented feature")]
    Unimplemented,
    #[error("invalid state")]
    InvalidState,
    #[error("allocation failed")]
    AllocFail,
    #[error("invalid sample rate: {0}")]
    InvalidSampleRate(u32),
    #[error("invalid channel count: {0}")]
    InvalidChannels(u8),
    #[error("unknown error code: {0}")]
    Unknown(c_int),
}

impl From<c_int> for Opus16Error {
    fn from(code: c_int) -> Self {
        match code {
            ffi::OPUS_BAD_ARG => Self::BadArg,
            ffi::OPUS_BUFFER_TOO_SMALL => Self::BufferTooSmall,
            ffi::OPUS_INTERNAL_ERROR => Self::Internal,
            ffi::OPUS_INVALID_PACKET => Self::InvalidPacket,
            ffi::OPUS_UNIMPLEMENTED => Self::Unimplemented,
            ffi::OPUS_INVALID_STATE => Self::InvalidState,
            ffi::OPUS_ALLOC_FAIL => Self::AllocFail,
            other => Self::Unknown(other),
        }
    }
}

fn check_error(code: c_int) -> Result<(), Opus16Error> {
    if code < 0 {
        Err(Opus16Error::from(code))
    } else {
        Ok(())
    }
}

// ============================================================================
// Application Mode
// ============================================================================

/// Opus application mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Application {
    /// Best for VoIP/videoconference (default)
    Voip,
    /// Best for broadcast/high-fidelity audio
    Audio,
    /// Lowest achievable latency
    RestrictedLowDelay,
}

impl Application {
    fn to_ffi(self) -> c_int {
        match self {
            Self::Voip => ffi::OPUS_APPLICATION_VOIP,
            Self::Audio => ffi::OPUS_APPLICATION_AUDIO,
            Self::RestrictedLowDelay => ffi::OPUS_APPLICATION_RESTRICTED_LOWDELAY,
        }
    }
}

// ============================================================================
// Sample Rate Validation
// ============================================================================

fn validate_sample_rate(rate: u32) -> Result<ffi::opus_int32, Opus16Error> {
    match rate {
        8000 | 12000 | 16000 | 24000 | 48000 | 96000 => Ok(rate as ffi::opus_int32),
        _ => Err(Opus16Error::InvalidSampleRate(rate)),
    }
}

fn validate_channels(channels: u8) -> Result<c_int, Opus16Error> {
    match channels {
        1 | 2 => Ok(channels as c_int),
        _ => Err(Opus16Error::InvalidChannels(channels)),
    }
}

// ============================================================================
// Encoder
// ============================================================================

/// Safe wrapper around OpusEncoder with RAII lifecycle
///
/// **NOT thread-safe**: Do not share across threads without external synchronization
pub struct Opus16Encoder {
    ptr: NonNull<ffi::OpusEncoder>,
    sample_rate: u32,
    channels: u8,
    _marker: PhantomData<*mut ()>,
}

// Safety: OpusEncoder is a standalone C struct. It is safe to move to another thread.
// It is NOT safe to access concurrently without a lock (which we provide in the high-level API).
unsafe impl Send for Opus16Encoder {}
unsafe impl Sync for Opus16Encoder {}

impl Opus16Encoder {
    /// Create a new Opus encoder
    ///
    /// # Arguments
    /// * `sample_rate` - 8000, 12000, 16000, 24000, 48000, or 96000 Hz
    /// * `channels` - 1 (mono) or 2 (stereo)
    /// * `application` - Coding mode
    pub fn new(sample_rate: u32, channels: u8, application: Application) -> Result<Self, Opus16Error> {
        let fs = validate_sample_rate(sample_rate)?;
        let ch = validate_channels(channels)?;
        
        let mut error: c_int = 0;
        let ptr = unsafe {
            ffi::opus_encoder_create(fs, ch, application.to_ffi(), &mut error)
        };
        
        check_error(error)?;
        
        let ptr = NonNull::new(ptr).ok_or(Opus16Error::AllocFail)?;
        
        Ok(Self {
            ptr,
            sample_rate,
            channels,
            _marker: PhantomData,
        })
    }
    
    /// Get the sample rate
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    
    /// Get the number of channels
    pub fn channels(&self) -> u8 {
        self.channels
    }
    
    /// Encode 16-bit PCM to Opus
    ///
    /// # Arguments
    /// * `pcm` - Input samples (frame_size * channels)
    /// * `output` - Output buffer (4000 bytes recommended)
    ///
    /// # Returns
    /// Number of bytes written to output
    pub fn encode(&mut self, pcm: &[i16], output: &mut [u8]) -> Result<usize, Opus16Error> {
        let frame_size = (pcm.len() / self.channels as usize) as c_int;
        
        let len = unsafe {
            ffi::opus_encode(
                self.ptr.as_ptr(),
                pcm.as_ptr(),
                frame_size,
                output.as_mut_ptr(),
                output.len() as ffi::opus_int32,
            )
        };
        
        if len < 0 {
            Err(Opus16Error::from(len as c_int))
        } else {
            Ok(len as usize)
        }
    }
    
    /// Encode 24-bit PCM to Opus (NEW in v1.6)
    ///
    /// PCM samples are in the lower 24 bits of i32, range [-2^23, 2^23-1]
    pub fn encode24(&mut self, pcm: &[i32], output: &mut [u8]) -> Result<usize, Opus16Error> {
        let frame_size = (pcm.len() / self.channels as usize) as c_int;
        
        let len = unsafe {
            ffi::opus_encode24(
                self.ptr.as_ptr(),
                pcm.as_ptr(),
                frame_size,
                output.as_mut_ptr(),
                output.len() as ffi::opus_int32,
            )
        };
        
        if len < 0 {
            Err(Opus16Error::from(len as c_int))
        } else {
            Ok(len as usize)
        }
    }
    
    /// Encode float PCM to Opus
    pub fn encode_float(&mut self, pcm: &[f32], output: &mut [u8]) -> Result<usize, Opus16Error> {
        let frame_size = (pcm.len() / self.channels as usize) as c_int;
        
        let len = unsafe {
            ffi::opus_encode_float(
                self.ptr.as_ptr(),
                pcm.as_ptr(),
                frame_size,
                output.as_mut_ptr(),
                output.len() as ffi::opus_int32,
            )
        };
        
        if len < 0 {
            Err(Opus16Error::from(len as c_int))
        } else {
            Ok(len as usize)
        }
    }
    
    /// Set the bitrate in bits/second
    pub fn set_bitrate(&mut self, bps: i32) -> Result<(), Opus16Error> {
        let result = unsafe {
            ffi::opus_encoder_ctl(
                self.ptr.as_ptr(),
                ffi::OPUS_SET_BITRATE_REQUEST,
                bps as c_int,
            )
        };
        check_error(result)
    }
    
    /// Enable/disable in-band FEC
    pub fn set_inband_fec(&mut self, enabled: bool) -> Result<(), Opus16Error> {
        let result = unsafe {
            ffi::opus_encoder_ctl(
                self.ptr.as_ptr(),
                ffi::OPUS_SET_INBAND_FEC_REQUEST,
                enabled as c_int,
            )
        };
        check_error(result)
    }
    
    /// Set expected packet loss percentage (0-100)
    pub fn set_packet_loss_perc(&mut self, percent: u8) -> Result<(), Opus16Error> {
        let result = unsafe {
            ffi::opus_encoder_ctl(
                self.ptr.as_ptr(),
                ffi::OPUS_SET_PACKET_LOSS_PERC_REQUEST,
                percent as c_int,
            )
        };
        check_error(result)
    }
    
    /// Set DRED duration in 10ms frames (0 = disabled, max ~100 = 1 second)
    pub fn set_dred_duration(&mut self, frames: i32) -> Result<(), Opus16Error> {
        let result = unsafe {
            ffi::opus_encoder_ctl(
                self.ptr.as_ptr(),
                ffi::OPUS_SET_DRED_DURATION_REQUEST,
                frames as c_int,
            )
        };
        check_error(result)
    }
    
    /// Get DRED duration in 10ms frames
    pub fn get_dred_duration(&self) -> Result<i32, Opus16Error> {
        let mut duration: c_int = 0;
        let result = unsafe {
            ffi::opus_encoder_ctl(
                self.ptr.as_ptr(),
                ffi::OPUS_GET_DRED_DURATION_REQUEST,
                &mut duration as *mut c_int,
            )
        };
        check_error(result)?;
        Ok(duration)
    }
}

impl Drop for Opus16Encoder {
    fn drop(&mut self) {
        unsafe {
            ffi::opus_encoder_destroy(self.ptr.as_ptr());
        }
    }
}

// ============================================================================
// Decoder
// ============================================================================

/// Safe wrapper around OpusDecoder with RAII lifecycle
///
/// **NOT thread-safe**: Do not share across threads without external synchronization
pub struct Opus16Decoder {
    ptr: NonNull<ffi::OpusDecoder>,
    sample_rate: u32,
    channels: u8,
    _marker: PhantomData<*mut ()>,
}

unsafe impl Send for Opus16Decoder {}
unsafe impl Sync for Opus16Decoder {}

impl Opus16Decoder {
    /// Create a new Opus decoder
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self, Opus16Error> {
        let fs = validate_sample_rate(sample_rate)?;
        let ch = validate_channels(channels)?;
        
        let mut error: c_int = 0;
        let ptr = unsafe {
            ffi::opus_decoder_create(fs, ch, &mut error)
        };
        
        check_error(error)?;
        
        let ptr = NonNull::new(ptr).ok_or(Opus16Error::AllocFail)?;
        
        Ok(Self {
            ptr,
            sample_rate,
            channels,
            _marker: PhantomData,
        })
    }
    
    /// Get the sample rate
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    
    /// Get the number of channels
    pub fn channels(&self) -> u8 {
        self.channels
    }
    
    /// Decode Opus to 16-bit PCM
    ///
    /// # Arguments
    /// * `data` - Opus packet
    /// * `output` - Output buffer (frame_size * channels samples)
    /// * `fec` - Request FEC decoding for previous packet
    ///
    /// # Returns
    /// Number of samples decoded per channel
    pub fn decode(&mut self, data: &[u8], output: &mut [i16], fec: bool) -> Result<usize, Opus16Error> {
        let frame_size = (output.len() / self.channels as usize) as c_int;
        
        let samples = unsafe {
            ffi::opus_decode(
                self.ptr.as_ptr(),
                data.as_ptr(),
                data.len() as ffi::opus_int32,
                output.as_mut_ptr(),
                frame_size,
                fec as c_int,
            )
        };
        
        if samples < 0 {
            Err(Opus16Error::from(samples))
        } else {
            Ok(samples as usize)
        }
    }
    
    /// Decode Opus to 24-bit PCM (NEW in v1.6)
    pub fn decode24(&mut self, data: &[u8], output: &mut [i32], fec: bool) -> Result<usize, Opus16Error> {
        let frame_size = (output.len() / self.channels as usize) as c_int;
        
        let samples = unsafe {
            ffi::opus_decode24(
                self.ptr.as_ptr(),
                data.as_ptr(),
                data.len() as ffi::opus_int32,
                output.as_mut_ptr(),
                frame_size,
                fec as c_int,
            )
        };
        
        if samples < 0 {
            Err(Opus16Error::from(samples))
        } else {
            Ok(samples as usize)
        }
    }
    
    /// Decode Opus to float PCM
    pub fn decode_float(&mut self, data: &[u8], output: &mut [f32], fec: bool) -> Result<usize, Opus16Error> {
        let frame_size = (output.len() / self.channels as usize) as c_int;
        
        let samples = unsafe {
            ffi::opus_decode_float(
                self.ptr.as_ptr(),
                data.as_ptr(),
                data.len() as ffi::opus_int32,
                output.as_mut_ptr(),
                frame_size,
                fec as c_int,
            )
        };
        
        if samples < 0 {
            Err(Opus16Error::from(samples))
        } else {
            Ok(samples as usize)
        }
    }
    
    /// Packet Loss Concealment - generate audio for missing packet
    pub fn decode_plc(&mut self, output: &mut [i16]) -> Result<usize, Opus16Error> {
        let frame_size = (output.len() / self.channels as usize) as c_int;
        
        let samples = unsafe {
            ffi::opus_decode(
                self.ptr.as_ptr(),
                std::ptr::null(),
                0,
                output.as_mut_ptr(),
                frame_size,
                0,
            )
        };
        
        if samples < 0 {
            Err(Opus16Error::from(samples))
        } else {
            Ok(samples as usize)
        }
    }
}

impl Drop for Opus16Decoder {
    fn drop(&mut self) {
        unsafe {
            ffi::opus_decoder_destroy(self.ptr.as_ptr());
        }
    }
}

// ============================================================================
// DRED (Deep Redundancy)
// ============================================================================

/// DRED state container
pub struct OpusDred {
    ptr: NonNull<ffi::OpusDRED>,
}

impl OpusDred {
    /// Create a new DRED state
    pub fn new() -> Result<Self, Opus16Error> {
        let mut error: c_int = 0;
        let ptr = unsafe { ffi::opus_dred_create(&mut error) };
        
        check_error(error)?;
        let ptr = NonNull::new(ptr).ok_or(Opus16Error::AllocFail)?;
        
        Ok(Self { ptr })
    }
    
    pub(crate) fn as_ptr(&self) -> *const ffi::OpusDRED {
        self.ptr.as_ptr()
    }
    
    pub(crate) fn as_mut_ptr(&mut self) -> *mut ffi::OpusDRED {
        self.ptr.as_ptr()
    }
}

impl Drop for OpusDred {
    fn drop(&mut self) {
        unsafe { ffi::opus_dred_free(self.ptr.as_ptr()) }
    }
}

/// DRED decoder/parser
pub struct OpusDredDecoder {
    ptr: NonNull<ffi::OpusDREDDecoder>,
}

impl OpusDredDecoder {
    /// Create a new DRED decoder
    pub fn new() -> Result<Self, Opus16Error> {
        let mut error: c_int = 0;
        let ptr = unsafe { ffi::opus_dred_decoder_create(&mut error) };
        
        check_error(error)?;
        let ptr = NonNull::new(ptr).ok_or(Opus16Error::AllocFail)?;
        
        Ok(Self { ptr })
    }
    
    /// Parse DRED data from an Opus packet extension
    ///
    /// # Returns
    /// Number of DRED samples available
    pub fn parse(
        &mut self,
        dred: &mut OpusDred,
        data: &[u8],
        max_samples: i32,
        sample_rate: i32,
    ) -> Result<i32, Opus16Error> {
        let mut dred_end: c_int = 0;
        
        let samples = unsafe {
            ffi::opus_dred_parse(
                self.ptr.as_ptr(),
                dred.as_mut_ptr(),
                data.as_ptr(),
                data.len() as ffi::opus_int32,
                max_samples,
                sample_rate,
                &mut dred_end,
                0, // Don't defer processing
            )
        };
        
        if samples < 0 {
            Err(Opus16Error::from(samples))
        } else {
            Ok(samples)
        }
    }
}

impl Drop for OpusDredDecoder {
    fn drop(&mut self) {
        unsafe { ffi::opus_dred_decoder_destroy(self.ptr.as_ptr()) }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_encoder_creation() {
        let encoder = Opus16Encoder::new(48000, 1, Application::Voip);
        assert!(encoder.is_ok());
        
        let encoder = encoder.unwrap();
        assert_eq!(encoder.sample_rate(), 48000);
        assert_eq!(encoder.channels(), 1);
    }
    
    #[test]
    fn test_decoder_creation() {
        let decoder = Opus16Decoder::new(48000, 1);
        assert!(decoder.is_ok());
    }
    
    #[test]
    fn test_invalid_sample_rate() {
        let encoder = Opus16Encoder::new(44100, 1, Application::Voip);
        assert!(matches!(encoder, Err(Opus16Error::InvalidSampleRate(44100))));
    }
    
    #[test]
    fn test_invalid_channels() {
        let encoder = Opus16Encoder::new(48000, 3, Application::Voip);
        assert!(matches!(encoder, Err(Opus16Error::InvalidChannels(3))));
    }
    
    #[test]
    fn test_encode_decode_roundtrip() {
        let mut encoder = Opus16Encoder::new(48000, 1, Application::Voip).unwrap();
        let mut decoder = Opus16Decoder::new(48000, 1).unwrap();
        
        // Generate 20ms frame (960 samples at 48kHz)
        let frame_size = 960;
        let pcm: Vec<i16> = (0..frame_size)
            .map(|i| {
                let t = i as f32 / 48000.0;
                ((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 16000.0) as i16
            })
            .collect();
        
        // Encode
        let mut opus = vec![0u8; 4000];
        let len = encoder.encode(&pcm, &mut opus).unwrap();
        opus.truncate(len);
        
        // Decode
        let mut decoded = vec![0i16; frame_size];
        let samples = decoder.decode(&opus, &mut decoded, false).unwrap();
        
        assert_eq!(samples, frame_size);
        
        // Verify amplitude preserved (lossy, within 30%)
        let orig_peak = pcm.iter().map(|s| s.abs()).max().unwrap();
        let dec_peak = decoded.iter().map(|s| s.abs()).max().unwrap();
        let ratio = dec_peak as f32 / orig_peak as f32;
        assert!(ratio > 0.7 && ratio < 1.3, "Peak ratio: {}", ratio);
    }
    
    #[test]
    fn test_24bit_encode_decode_roundtrip() {
        let mut encoder = Opus16Encoder::new(48000, 1, Application::Voip).unwrap();
        let mut decoder = Opus16Decoder::new(48000, 1).unwrap();
        
        let frame_size = 960;
        
        // Generate 24-bit test tone (440Hz sine wave)
        let pcm: Vec<i32> = (0..frame_size)
            .map(|i| {
                let t = i as f32 / 48000.0;
                let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                (sample * 0x7FFFFF as f32) as i32  // Scale to 24-bit range
            })
            .collect();
        
        // Encode as 24-bit
        let mut opus = vec![0u8; 4000];
        let len = encoder.encode24(&pcm, &mut opus).unwrap();
        opus.truncate(len);
        
        // Decode as 24-bit
        let mut decoded = vec![0i32; frame_size];
        let samples = decoder.decode24(&opus, &mut decoded, false).unwrap();
        
        assert_eq!(samples, frame_size);
        
        // Verify amplitude preserved
        let orig_peak = pcm.iter().map(|s| s.abs()).max().unwrap();
        let dec_peak = decoded.iter().map(|s| s.abs()).max().unwrap();
        let ratio = dec_peak as f32 / orig_peak as f32;
        assert!(ratio > 0.7 && ratio < 1.3, "24-bit peak ratio: {}", ratio);
    }
    
    #[test]
    fn test_dred_duration_control() {
        let mut encoder = Opus16Encoder::new(48000, 1, Application::Voip).unwrap();
        
        // DRED may not be compiled into libopus - skip test if unimplemented
        match encoder.get_dred_duration() {
            Ok(duration) => {
                assert_eq!(duration, 0); // Default should be disabled
                
                // Enable DRED with 50 frames (500ms)
                encoder.set_dred_duration(50).unwrap();
                let duration = encoder.get_dred_duration().unwrap();
                assert_eq!(duration, 50);
                
                // Disable
                encoder.set_dred_duration(0).unwrap();
                let duration = encoder.get_dred_duration().unwrap();
                assert_eq!(duration, 0);
            }
            Err(Opus16Error::Unimplemented) => {
                println!("DRED not compiled into libopus - skipping test");
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }
    
    #[test]
    fn test_96khz_sample_rate() {
        // Test Opus HD (96kHz) support - may not be available in all builds
        match Opus16Encoder::new(96000, 1, Application::Audio) {
            Ok(_encoder) => {
                let decoder = Opus16Decoder::new(96000, 1);
                assert!(decoder.is_ok(), "96kHz decoder should be supported");
            }
            Err(Opus16Error::BadArg) => {
                println!("96kHz not supported by this libopus build - skipping");
            }
            Err(e) => panic!("Unexpected error for 96kHz: {:?}", e),
        }
    }
    
    #[test]
    fn test_packet_loss_concealment() {
        let mut encoder = Opus16Encoder::new(48000, 1, Application::Voip).unwrap();
        let mut decoder = Opus16Decoder::new(48000, 1).unwrap();
        
        let frame_size = 960;
        let pcm = vec![1000i16; frame_size];
        
        // Encode a frame
        let mut opus = vec![0u8; 4000];
        let len = encoder.encode(&pcm, &mut opus).unwrap();
        opus.truncate(len);
        
        // Decode it
        let mut decoded = vec![0i16; frame_size];
        decoder.decode(&opus, &mut decoded, false).unwrap();
        
        // Now simulate packet loss
        let mut plc_output = vec![0i16; frame_size];
        let samples = decoder.decode_plc(&mut plc_output).unwrap();
        
        assert_eq!(samples, frame_size);
    }
}
