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
