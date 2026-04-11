//! Audio I/O using cpal for cross-platform microphone and speaker access
//!
//! Provides AudioCapture (microphone) and AudioPlayback (speaker) abstractions
//! with ring buffers for thread-safe audio processing.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Audio sample rate (48kHz for Opus)
pub const SAMPLE_RATE: u32 = 48000;

/// Frame size in samples (20ms at 48kHz)
pub const FRAME_SIZE: usize = 960;

/// Audio I/O errors
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("No audio device found")]
    NoDevice,
    
    #[error("Device error: {0}")]
    Device(String),
    
    #[error("Stream error: {0}")]
    Stream(String),
    
    #[error("Unsupported format")]
    UnsupportedFormat,
}

/// A wrapper for cpal::Stream to make it Send + Sync.
/// 
/// On some platforms (like macOS), cpal's Stream is not Send + Sync
/// because it contains CoreAudio callbacks that might not be thread-safe
/// to move. However, for use in UniFFI objects protected by a Mutex,
/// we need this marker.
struct SendableStream(Stream);
unsafe impl Send for SendableStream {}
unsafe impl Sync for SendableStream {}

impl std::ops::Deref for SendableStream {
    type Target = Stream;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Microphone capture
/// 
/// Captures audio from the default input device and sends 20ms frames
/// through a channel for processing.
pub struct AudioCapture {
    stream: SendableStream,
    running: Arc<AtomicBool>,
}

impl AudioCapture {
    /// Create a new audio capture from the default input device
    /// 
    /// Returns the capture handle and a receiver for 20ms PCM frames (960 samples)
    pub fn new() -> Result<(Self, Receiver<Vec<i16>>), AudioError> {
        let host = cpal::default_host();
        let device = host.default_input_device()
            .ok_or(AudioError::NoDevice)?;
        
        let config = StreamConfig {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            buffer_size: cpal::BufferSize::Fixed(FRAME_SIZE as u32),
        };
        
        let (tx, rx) = channel();
        let running = Arc::new(AtomicBool::new(false));
        let running_clone = running.clone();
        
        // Accumulator for building complete frames
        let mut buffer = Vec::with_capacity(FRAME_SIZE);
        
        let stream = device.build_input_stream(
            &config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                if !running_clone.load(Ordering::Relaxed) {
                    return;
                }
                
                // Accumulate samples
                buffer.extend_from_slice(data);
                
                // Send complete frames
                while buffer.len() >= FRAME_SIZE {
                    let frame: Vec<i16> = buffer.drain(..FRAME_SIZE).collect();
                    let _ = tx.send(frame);
                }
            },
            move |err| {
                eprintln!("Audio capture error: {}", err);
            },
            None, // No timeout
        ).map_err(|e| AudioError::Device(e.to_string()))?;
        
        Ok((Self { stream: SendableStream(stream), running }, rx))
    }
    
    /// Start capturing audio
    pub fn start(&self) -> Result<(), AudioError> {
        self.running.store(true, Ordering::Relaxed);
        self.stream.play().map_err(|e| AudioError::Stream(e.to_string()))
    }
    
    /// Stop capturing audio
    pub fn stop(&self) -> Result<(), AudioError> {
        self.running.store(false, Ordering::Relaxed);
        self.stream.pause().map_err(|e| AudioError::Stream(e.to_string()))
    }
    
    /// Check if currently capturing
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

/// Speaker playback
/// 
/// Plays audio through the default output device. Receives 20ms PCM frames
/// through a channel.
pub struct AudioPlayback {
    stream: SendableStream,
    running: Arc<AtomicBool>,
}

impl AudioPlayback {
    /// Create a new audio playback to the default output device
    /// 
    /// Returns the playback handle and a sender for 20ms PCM frames (960 samples)
    pub fn new() -> Result<(Self, Sender<Vec<i16>>), AudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device()
            .ok_or(AudioError::NoDevice)?;
        
        let config = StreamConfig {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            buffer_size: cpal::BufferSize::Fixed(FRAME_SIZE as u32),
        };
        
        let (tx, rx): (Sender<Vec<i16>>, Receiver<Vec<i16>>) = channel();
        let running = Arc::new(AtomicBool::new(false));
        let running_clone = running.clone();
        
        // Buffer for samples waiting to be played
        let mut pending: Vec<i16> = Vec::with_capacity(FRAME_SIZE * 4);
        
        let stream = device.build_output_stream(
            &config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                if !running_clone.load(Ordering::Relaxed) {
                    // Output silence when not running
                    data.fill(0);
                    return;
                }
                
                // Receive any available frames
                while let Ok(frame) = rx.try_recv() {
                    pending.extend(frame);
                }
                
                // Fill output buffer
                let available = pending.len().min(data.len());
                if available > 0 {
                    data[..available].copy_from_slice(&pending[..available]);
                    pending.drain(..available);
                }
                
                // Zero-fill the rest if not enough data
                if available < data.len() {
                    data[available..].fill(0);
                }
            },
            move |err| {
                eprintln!("Audio playback error: {}", err);
            },
            None, // No timeout
        ).map_err(|e| AudioError::Device(e.to_string()))?;
        
        Ok((Self { stream: SendableStream(stream), running }, tx))
    }
    
    /// Start playing audio
    pub fn start(&self) -> Result<(), AudioError> {
        self.running.store(true, Ordering::Relaxed);
        self.stream.play().map_err(|e| AudioError::Stream(e.to_string()))
    }
    
    /// Stop playing audio
    pub fn stop(&self) -> Result<(), AudioError> {
        self.running.store(false, Ordering::Relaxed);
        self.stream.pause().map_err(|e| AudioError::Stream(e.to_string()))
    }
    
    /// Check if currently playing
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

/// Full-duplex audio I/O
/// 
/// Combines capture and playback for push-to-talk or continuous voice.
pub struct AudioDevice {
    capture: AudioCapture,
    playback: AudioPlayback,
    capture_rx: Receiver<Vec<i16>>,
    playback_tx: Sender<Vec<i16>>,
}

impl AudioDevice {
    /// Create a new full-duplex audio device
    pub fn new() -> Result<Self, AudioError> {
        let (capture, capture_rx) = AudioCapture::new()?;
        let (playback, playback_tx) = AudioPlayback::new()?;
        
        Ok(Self {
            capture,
            playback,
            capture_rx,
            playback_tx,
        })
    }
    
    /// Start both capture and playback
    pub fn start(&self) -> Result<(), AudioError> {
        self.capture.start()?;
        self.playback.start()?;
        Ok(())
    }
    
    /// Stop both capture and playback
    pub fn stop(&self) -> Result<(), AudioError> {
        self.capture.stop()?;
        self.playback.stop()?;
        Ok(())
    }
    
    /// Get captured audio frame (non-blocking)
    /// 
    /// Returns None if no frame is ready
    pub fn try_recv_capture(&self) -> Option<Vec<i16>> {
        self.capture_rx.try_recv().ok()
    }
    
    /// Send audio frame for playback
    pub fn send_playback(&self, frame: Vec<i16>) -> Result<(), AudioError> {
        self.playback_tx.send(frame)
            .map_err(|_| AudioError::Stream("Playback channel closed".into()))
    }
    
    /// Start capture only (for push-to-talk)
    pub fn start_capture(&self) -> Result<(), AudioError> {
        self.capture.start()
    }
    
    /// Stop capture only (for push-to-talk)
    pub fn stop_capture(&self) -> Result<(), AudioError> {
        self.capture.stop()
    }
    
    /// Check if capture is running
    pub fn is_capturing(&self) -> bool {
        self.capture.is_running()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    // Note: These tests require audio hardware and may not work in CI
    
    #[test]
    #[ignore] // Requires audio device
    fn test_capture_creation() {
        let result = AudioCapture::new();
        assert!(result.is_ok(), "Failed to create capture: {:?}", result.err());
    }
    
    #[test]
    #[ignore] // Requires audio device
    fn test_playback_creation() {
        let result = AudioPlayback::new();
        assert!(result.is_ok(), "Failed to create playback: {:?}", result.err());
    }
    
    #[test]
    #[ignore] // Requires audio device
    fn test_audio_device_creation() {
        let result = AudioDevice::new();
        assert!(result.is_ok(), "Failed to create device: {:?}", result.err());
    }
    
    #[test]
    fn test_constants() {
        assert_eq!(SAMPLE_RATE, 48000);
        assert_eq!(FRAME_SIZE, 960);
        // 960 samples at 48kHz = 20ms
        assert_eq!(FRAME_SIZE as f32 / SAMPLE_RATE as f32 * 1000.0, 20.0);
    }
}
