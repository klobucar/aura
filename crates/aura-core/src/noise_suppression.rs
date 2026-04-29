//! Noise Suppression using RNNoise
//!
//! Provides real-time noise reduction for audio input using a recurrent neural network.
//! Based on Xiph's RNNoise, implemented in pure Rust via nnnoiseless.

use nnnoiseless::DenoiseState;

/// Noise suppressor for audio input
///
/// RNNoise processes audio in 10ms frames (480 samples at 48kHz).
/// For our 20ms Opus frames, we process in two 10ms chunks.
pub struct NoiseSuppressor {
    state: Box<DenoiseState<'static>>,
    first_frame: bool,
}

impl NoiseSuppressor {
    /// Create a new noise suppressor
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            first_frame: true,
        }
    }

    /// Process a 20ms frame (960 samples at 48kHz)
    ///
    /// Splits into two 10ms chunks for RNNoise processing.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        assert_eq!(
            input.len(),
            960,
            "Input must be 960 samples (20ms at 48kHz)"
        );

        let mut output = Vec::with_capacity(960);

        // Process first 10ms (480 samples)
        let mut first_out = [0.0f32; DenoiseState::FRAME_SIZE];
        self.state.process_frame(&mut first_out, &input[0..480]);

        // Skip the very first frame output (RNNoise warmup)
        if !self.first_frame {
            output.extend_from_slice(&first_out);
        } else {
            // For the first frame, use the input directly
            output.extend_from_slice(&input[0..480]);
            self.first_frame = false;
        }

        // Process second 10ms (480 samples)
        let mut second_out = [0.0f32; DenoiseState::FRAME_SIZE];
        self.state.process_frame(&mut second_out, &input[480..960]);
        output.extend_from_slice(&second_out);

        output
    }

    /// Reset the internal state (e.g., when switching audio devices)
    pub fn reset(&mut self) {
        self.state = DenoiseState::new();
        self.first_frame = true;
    }
}

impl Default for NoiseSuppressor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_frame() {
        let mut suppressor = NoiseSuppressor::new();
        let input = vec![0.01f32; 960]; // Quiet noise
        let output = suppressor.process(&input);
        assert_eq!(output.len(), 960);
    }

    #[test]
    #[should_panic(expected = "Input must be 960 samples")]
    fn test_wrong_frame_size() {
        let mut suppressor = NoiseSuppressor::new();
        let input = vec![0.0f32; 480]; // Wrong size
        suppressor.process(&input);
    }
}
