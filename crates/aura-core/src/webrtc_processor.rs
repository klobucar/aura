/// WebRTC Audio Processing wrapper
/// 
/// Provides AEC3 (Echo Cancellation), NS (Noise Suppression), and AGC (Auto Gain Control)
/// from Google's WebRTC library.

use webrtc_audio_processing::{
    Processor, InitializationConfig, Config,
    EchoCancellation, NoiseSuppression, GainControl,
    NoiseSuppressionLevel, GainControlMode,
};

pub struct WebRtcProcessor {
    processor: Processor,
}

impl WebRtcProcessor {
    /// Create a new WebRTC processor
    pub fn new(enable_aec: bool, enable_ns: bool, enable_agc: bool) -> Result<Self, String> {
        // Initialization config (cannot be changed after creation)
        let init_config = InitializationConfig {
            num_capture_channels: 1,
            num_render_channels: 1,
            enable_experimental_agc: false,
            enable_intelligibility_enhancer: false,
        };
        
        let mut processor = Processor::new(&init_config)
            .map_err(|e| format!("WebRTC init failed: {:?}", e))?;
        
        // Start with default config
        let mut config = Config::default();
        
        // Configure features
        config.echo_cancellation = if enable_aec {
            Some(EchoCancellation {
                enable_delay_agnostic: false,
                enable_extended_filter: true,
                stream_delay_ms: Some(0),
                suppression_level: webrtc_audio_processing::EchoCancellationSuppressionLevel::Moderate,
            })
        } else {
            None
        };
        
        config.noise_suppression = if enable_ns {
            Some(NoiseSuppression {
                suppression_level: NoiseSuppressionLevel::High,
            })
        } else {
            None
        };
        
        config.gain_control = if enable_agc {
            Some(GainControl {
                mode: GainControlMode::AdaptiveDigital,
                target_level_dbfs: 3,
                compression_gain_db: 9,
                enable_limiter: true,
            })
        } else {
            None
        };
        
        processor.set_config(config);
        
        Ok(Self { processor })
    }
    
    /// Process audio with WebRTC features
    /// 
    /// - `input`: Microphone input (960 samples, 20ms at 48kHz)
    /// - `reference`: Speaker output for AEC (optional, required if AEC is enabled)
    pub fn process(&mut self, input: &[f32], reference: Option<&[f32]>) -> Vec<f32> {
        // Feed reference audio for echo cancellation
        if let Some(ref_audio) = reference {
            let mut render_frame = ref_audio.to_vec();
            let _ = self.processor.process_render_frame(&mut render_frame);
        }
        
        // Process capture frame (applies AEC, NS, and AGC)
        let mut capture_frame = input.to_vec();
        let _ = self.processor.process_capture_frame(&mut capture_frame);
        capture_frame
    }
    
    /// Reconfigure features at runtime
    pub fn reconfigure(&mut self, enable_aec: bool, enable_ns: bool, enable_agc: bool) {
        let mut config = Config::default();
        
        config.echo_cancellation = if enable_aec {
            Some(EchoCancellation {
                enable_delay_agnostic: false,
                enable_extended_filter: true,
                stream_delay_ms: Some(0),
                suppression_level: webrtc_audio_processing::EchoCancellationSuppressionLevel::Moderate,
            })
        } else {
            None
        };
        
        config.noise_suppression = if enable_ns {
            Some(NoiseSuppression {
                suppression_level: NoiseSuppressionLevel::High,
            })
        } else {
            None
        };
        
        config.gain_control = if enable_agc {
            Some(GainControl {
                mode: GainControlMode::AdaptiveDigital,
                target_level_dbfs: 3,
                compression_gain_db: 9,
                enable_limiter: true,
            })
        } else {
            None
        };
        
        self.processor.set_config(config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_processor_initialization_all_enabled() {
        let processor = WebRtcProcessor::new(true, true, true);
        assert!(processor.is_ok(), "Should initialize with all features enabled");
    }

    #[test]
    fn test_processor_initialization_all_disabled() {
        let processor = WebRtcProcessor::new(false, false, false);
        assert!(processor.is_ok(), "Should initialize with all features disabled");
    }

    #[test]
    fn test_processor_initialization_aec_only() {
        let processor = WebRtcProcessor::new(true, false, false);
        assert!(processor.is_ok(), "Should initialize with AEC only");
    }

    #[test]
    fn test_processor_initialization_ns_only() {
        let processor = WebRtcProcessor::new(false, true, false);
        assert!(processor.is_ok(), "Should initialize with NS only");
    }

    #[test]
    fn test_processor_initialization_agc_only() {
        let processor = WebRtcProcessor::new(false, false, true);
        assert!(processor.is_ok(), "Should initialize with AGC only");
    }

    #[test]
    fn test_process_audio_without_reference() {
        let mut processor = WebRtcProcessor::new(false, true, true).unwrap();
        
        // 960 samples = 20ms at 48kHz
        let input = vec![0.0f32; 960];
        let output = processor.process(&input, None);
        
        assert_eq!(output.len(), 960, "Output should have same length as input");
    }

    #[test]
    fn test_process_audio_with_reference() {
        let mut processor = WebRtcProcessor::new(true, true, true).unwrap();
        
        let input = vec![0.1f32; 960];
        let reference = vec![0.05f32; 960];
        let output = processor.process(&input, Some(&reference));
        
        assert_eq!(output.len(), 960);
    }

    #[test]
    fn test_process_audio_with_noise() {
        let mut processor = WebRtcProcessor::new(false, true, false).unwrap();
        
        // Simulate noisy input
        let mut input = vec![0.0f32; 960];
        for (i, sample) in input.iter_mut().enumerate() {
            *sample = (i as f32 * 0.001).sin() + 0.01; // Signal + noise
        }
        
        let output = processor.process(&input, None);
        assert_eq!(output.len(), 960);
        
        // Noise suppression should reduce high-frequency noise
        // (exact validation would require spectral analysis)
    }

    #[test]
    fn test_process_audio_with_low_volume() {
        let mut processor = WebRtcProcessor::new(false, false, true).unwrap();
        
        // Very quiet input
        let input = vec![0.001f32; 960];
        let output = processor.process(&input, None);
        
        assert_eq!(output.len(), 960);
        // AGC should boost the signal (output should be louder than input)
    }

    #[test]
    fn test_process_audio_with_high_volume() {
        let mut processor = WebRtcProcessor::new(false, false, true).unwrap();
        
        // Very loud input
        let input = vec![0.9f32; 960];
        let output = processor.process(&input, None);
        
        assert_eq!(output.len(), 960);
        // AGC limiter should prevent clipping
        assert!(output.iter().all(|&s| s.abs() <= 1.0), "Should not clip");
    }

    #[test]
    fn test_reconfigure_enable_all() {
        let mut processor = WebRtcProcessor::new(false, false, false).unwrap();
        
        // Reconfigure to enable all features
        processor.reconfigure(true, true, true);
        
        let input = vec![0.1f32; 960];
        let reference = vec![0.05f32; 960];
        let output = processor.process(&input, Some(&reference));
        
        assert_eq!(output.len(), 960);
    }

    #[test]
    fn test_reconfigure_disable_all() {
        let mut processor = WebRtcProcessor::new(true, true, true).unwrap();
        
        // Reconfigure to disable all features
        processor.reconfigure(false, false, false);
        
        let input = vec![0.1f32; 960];
        let output = processor.process(&input, None);
        
        assert_eq!(output.len(), 960);
    }

    #[test]
    fn test_reconfigure_toggle_features() {
        let mut processor = WebRtcProcessor::new(true, false, true).unwrap();
        
        // Toggle: disable AEC, enable NS, keep AGC
        processor.reconfigure(false, true, true);
        
        let input = vec![0.1f32; 960];
        let output = processor.process(&input, None);
        
        assert_eq!(output.len(), 960);
    }

    #[test]
    fn test_multiple_process_calls() {
        let mut processor = WebRtcProcessor::new(true, true, true).unwrap();
        
        // Process multiple frames
        for _ in 0..10 {
            let input = vec![0.1f32; 960];
            let reference = vec![0.05f32; 960];
            let output = processor.process(&input, Some(&reference));
            assert_eq!(output.len(), 960);
        }
    }

    #[test]
    fn test_process_silence() {
        let mut processor = WebRtcProcessor::new(true, true, true).unwrap();
        
        let input = vec![0.0f32; 960];
        let output = processor.process(&input, None);
        
        assert_eq!(output.len(), 960);
        // Processing silence should not crash
    }

    #[test]
    fn test_process_max_amplitude() {
        let mut processor = WebRtcProcessor::new(true, true, true).unwrap();
        
        let input = vec![1.0f32; 960];
        let output = processor.process(&input, None);
        
        assert_eq!(output.len(), 960);
        // Should handle max amplitude without crashing
    }

    #[test]
    fn test_process_alternating_signal() {
        let mut processor = WebRtcProcessor::new(false, true, true).unwrap();
        
        let mut input = vec![0.0f32; 960];
        for (i, sample) in input.iter_mut().enumerate() {
            *sample = if i % 2 == 0 { 0.5 } else { -0.5 };
        }
        
        let output = processor.process(&input, None);
        assert_eq!(output.len(), 960);
    }
}
