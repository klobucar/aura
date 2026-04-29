//! Voice Activity Detection (VAD)
//!
//! Simple energy-based voice activity detection to avoid transmitting silence.
//! Uses RMS energy with a configurable threshold and hangover timer.

/// Voice Activity Detector using RMS energy
pub struct VoiceActivityDetector {
    /// Threshold in linear scale (not dB)
    threshold: f32,
    /// Hangover frames (to avoid clipping end of speech)
    hangover_frames: u32,
    /// Current hangover countdown
    current_hangover: u32,
    /// Whether voice was detected in the last frame
    is_active: bool,
}

impl VoiceActivityDetector {
    /// Create a new VAD with the given threshold and hangover
    ///
    /// # Arguments
    /// * `threshold_db` - Detection threshold in dB (e.g., -40.0)
    /// * `hangover_ms` - Hangover duration in milliseconds (e.g., 200)
    /// * `frame_duration_ms` - Frame duration in milliseconds (typically 20)
    pub fn new(threshold_db: f32, hangover_ms: u32, frame_duration_ms: u32) -> Self {
        // Convert dB to linear: 10^(dB/20)
        let threshold = 10f32.powf(threshold_db / 20.0);
        let hangover_frames = hangover_ms / frame_duration_ms;

        Self {
            threshold,
            hangover_frames,
            current_hangover: 0,
            is_active: false,
        }
    }

    /// Create with default settings
    ///
    /// Threshold: -40 dB, Hangover: 200ms, Frame: 20ms
    pub fn default_20ms() -> Self {
        Self::new(-40.0, 200, 20)
    }

    /// Process a frame of PCM samples
    ///
    /// Returns true if voice activity is detected (including hangover)
    pub fn process(&mut self, pcm: &[i16]) -> bool {
        let rms = compute_rms_i16(pcm);

        if rms > self.threshold {
            // Voice detected - reset hangover
            self.current_hangover = self.hangover_frames;
            self.is_active = true;
        } else if self.current_hangover > 0 {
            // In hangover period
            self.current_hangover -= 1;
            self.is_active = true;
        } else {
            // Silence
            self.is_active = false;
        }

        self.is_active
    }

    /// Process f32 PCM samples (range -1.0 to 1.0)
    pub fn process_float(&mut self, pcm: &[f32]) -> bool {
        let rms = compute_rms_f32(pcm);

        if rms > self.threshold {
            self.current_hangover = self.hangover_frames;
            self.is_active = true;
        } else if self.current_hangover > 0 {
            self.current_hangover -= 1;
            self.is_active = true;
        } else {
            self.is_active = false;
        }

        self.is_active
    }

    /// Check if currently active (voice or hangover)
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// Reset the detector state
    pub fn reset(&mut self) {
        self.current_hangover = 0;
        self.is_active = false;
    }

    /// Set a new threshold
    pub fn set_threshold_db(&mut self, threshold_db: f32) {
        self.threshold = 10f32.powf(threshold_db / 20.0);
    }
}

impl Default for VoiceActivityDetector {
    fn default() -> Self {
        Self::default_20ms()
    }
}

/// Compute RMS (Root Mean Square) of i16 samples
///
/// Returns value in range 0.0 to 1.0
fn compute_rms_i16(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f64 = samples
        .iter()
        .map(|&s| (s as f64 / i16::MAX as f64).powi(2))
        .sum();

    (sum_squares / samples.len() as f64).sqrt() as f32
}

/// Compute RMS of f32 samples
fn compute_rms_f32(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f32 = samples.iter().map(|&s| s.powi(2)).sum();

    (sum_squares / samples.len() as f32).sqrt()
}

/// Convert linear amplitude to dB
#[allow(dead_code)]
fn linear_to_db(linear: f32) -> f32 {
    20.0 * linear.log10()
}

/// Convert dB to linear amplitude
#[allow(dead_code)]
fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_silence_detection() {
        let mut vad = VoiceActivityDetector::default();

        // Complete silence
        let silence = vec![0i16; 960];
        assert!(!vad.process(&silence));
        assert!(!vad.is_active());
    }

    #[test]
    fn test_voice_detection() {
        let mut vad = VoiceActivityDetector::new(-30.0, 100, 20);

        // Loud signal (about -6 dB)
        let loud: Vec<i16> = (0..960).map(|_| 16000i16).collect();
        assert!(vad.process(&loud));
        assert!(vad.is_active());
    }

    #[test]
    fn test_hangover() {
        let mut vad = VoiceActivityDetector::new(-30.0, 60, 20); // 3 frames hangover

        // Loud signal
        let loud: Vec<i16> = (0..960).map(|_| 16000i16).collect();
        let silence = vec![0i16; 960];

        // Voice triggers
        assert!(vad.process(&loud));

        // Silence - but still in hangover (3 frames)
        assert!(vad.process(&silence)); // hangover 2
        assert!(vad.process(&silence)); // hangover 1
        assert!(vad.process(&silence)); // hangover 0, still true
        assert!(!vad.process(&silence)); // now false
    }

    #[test]
    fn test_rms_computation() {
        // Full-scale sine wave has RMS of 1/sqrt(2) ≈ 0.707
        let full_scale: Vec<i16> = (0..960).map(|_| i16::MAX).collect();
        let rms = compute_rms_i16(&full_scale);
        assert!(
            (rms - 1.0).abs() < 0.01,
            "RMS should be ~1.0 for max signal"
        );

        // Silence
        let silence = vec![0i16; 960];
        let rms_silence = compute_rms_i16(&silence);
        assert_eq!(rms_silence, 0.0);
    }

    #[test]
    fn test_threshold_setting() {
        let mut vad = VoiceActivityDetector::new(-40.0, 100, 20);

        // -20 dB signal
        let mid_signal: Vec<i16> = (0..960).map(|_| 3277i16).collect(); // ~10% = -20dB

        // Should detect with -40dB threshold
        assert!(vad.process(&mid_signal));

        // Raise threshold to -10dB
        vad.set_threshold_db(-10.0);
        vad.reset();

        // Now should not detect
        assert!(!vad.process(&mid_signal));
    }

    #[test]
    fn test_float_processing() {
        let mut vad = VoiceActivityDetector::new(-30.0, 100, 20);

        // Loud float signal
        let loud: Vec<f32> = (0..960).map(|_| 0.5f32).collect();
        assert!(vad.process_float(&loud));

        // Quiet float signal
        let quiet: Vec<f32> = (0..960).map(|_| 0.001f32).collect();
        vad.reset();
        assert!(!vad.process_float(&quiet));
    }
}
