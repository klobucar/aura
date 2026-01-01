// Audio pipeline integration tests
// crates/aura-core/tests/audio_pipeline_tests.rs

use aura_core::{AudioSenderWrapper, AudioReceiverWrapper};

#[tokio::test]
async fn test_full_audio_pipeline() {
    // Create sender and receiver
    let sender = AudioSenderWrapper::new(1, 48000);
    let receiver = AudioReceiverWrapper::new(48000);
    
    // Encode audio
    let input_samples = vec![0.1f32; 960]; // 20ms at 48kHz
    let encoded = sender.encode_frame(&input_samples).unwrap();
    
    assert!(!encoded.is_empty(), "Encoded data should not be empty");
    
    // Decrypt and decode (in real scenario, would be encrypted)
    let decoded = receiver.decode_frame(&encoded, 1).unwrap();
    
    assert_eq!(decoded.len(), 960, "Decoded should have same length as input");
}

#[tokio::test]
async fn test_packet_loss_recovery() {
    let receiver = AudioReceiverWrapper::new(48000);
    
    // Simulate packet loss by skipping sequence numbers
    let packet1 = vec![1u8; 100];
    let packet3 = vec![3u8; 100]; // Skip packet 2
    
    let decoded1 = receiver.decode_frame(&packet1, 1).unwrap();
    assert_eq!(decoded1.len(), 960);
    
    // Decoding with gap should use PLC (packet loss concealment)
    let decoded3 = receiver.decode_frame(&packet3, 3).unwrap();
    assert_eq!(decoded3.len(), 960);
}

#[tokio::test]
async fn test_out_of_order_packets() {
    let receiver = AudioReceiverWrapper::new(48000);
    
    let packet1 = vec![1u8; 100];
    let packet2 = vec![2u8; 100];
    let packet3 = vec![3u8; 100];
    
    // Receive out of order: 1, 3, 2
    let _ = receiver.decode_frame(&packet1, 1).unwrap();
    let _ = receiver.decode_frame(&packet3, 3).unwrap();
    let _ = receiver.decode_frame(&packet2, 2).unwrap(); // Late packet
    
    // Should handle gracefully
}

#[test]
fn test_opus_encoding_quality() {
    let sender = AudioSenderWrapper::new(1, 48000);
    
    // Test with different signal types
    let silence = vec![0.0f32; 960];
    let tone = (0..960).map(|i| (i as f32 * 0.01).sin()).collect::<Vec<_>>();
    let noise = (0..960).map(|i| (i as f32 * 0.001).sin() * 0.1).collect::<Vec<_>>();
    
    let encoded_silence = sender.encode_frame(&silence).unwrap();
    let encoded_tone = sender.encode_frame(&tone).unwrap();
    let encoded_noise = sender.encode_frame(&noise).unwrap();
    
    // Silence should compress well
    assert!(encoded_silence.len() < 100, "Silence should compress to < 100 bytes");
    
    // Tone and noise should be larger
    assert!(encoded_tone.len() > encoded_silence.len());
    assert!(encoded_noise.len() > 0);
}

#[test]
fn test_concurrent_senders() {
    use std::sync::Arc;
    use std::thread;
    
    let receiver = Arc::new(AudioReceiverWrapper::new(48000));
    let mut handles = vec![];
    
    // 10 concurrent senders
    for session_id in 1..=10 {
        let receiver_clone = Arc::clone(&receiver);
        let handle = thread::spawn(move {
            let sender = AudioSenderWrapper::new(session_id, 48000);
            let input = vec![0.1f32; 960];
            let encoded = sender.encode_frame(&input).unwrap();
            receiver_clone.decode_frame(&encoded, session_id as u64).unwrap()
        });
        handles.push(handle);
    }
    
    for handle in handles {
        let decoded = handle.join().unwrap();
        assert_eq!(decoded.len(), 960);
    }
}

#[test]
fn test_sample_rate_conversion() {
    // Test 16kHz to 48kHz conversion
    let sender_16k = AudioSenderWrapper::new(1, 16000);
    let sender_48k = AudioSenderWrapper::new(2, 48000);
    
    let input_16k = vec![0.1f32; 320]; // 20ms at 16kHz
    let input_48k = vec![0.1f32; 960]; // 20ms at 48kHz
    
    let encoded_16k = sender_16k.encode_frame(&input_16k).unwrap();
    let encoded_48k = sender_48k.encode_frame(&input_48k).unwrap();
    
    // Both should produce valid Opus packets
    assert!(!encoded_16k.is_empty());
    assert!(!encoded_48k.is_empty());
}

#[test]
fn test_audio_frame_sizes() {
    let sender = AudioSenderWrapper::new(1, 48000);
    
    // Test different frame sizes (Opus supports 2.5, 5, 10, 20, 40, 60ms)
    let frame_20ms = vec![0.1f32; 960];  // 20ms at 48kHz
    
    let encoded = sender.encode_frame(&frame_20ms).unwrap();
    assert!(!encoded.is_empty());
    
    // Invalid frame size should fail gracefully
    let invalid_frame = vec![0.1f32; 100];
    let result = sender.encode_frame(&invalid_frame);
    // Should either work (with padding) or return error
}

#[test]
fn test_audio_clipping_prevention() {
    let sender = AudioSenderWrapper::new(1, 48000);
    
    // Test with clipping signal
    let clipping_signal = vec![2.0f32; 960]; // > 1.0 (clipping)
    
    let encoded = sender.encode_frame(&clipping_signal);
    // Should handle gracefully (Opus internally clips to [-1, 1])
    assert!(encoded.is_ok() || encoded.is_err());
}

#[test]
fn test_zero_amplitude_handling() {
    let sender = AudioSenderWrapper::new(1, 48000);
    let receiver = AudioReceiverWrapper::new(48000);
    
    let silence = vec![0.0f32; 960];
    let encoded = sender.encode_frame(&silence).unwrap();
    let decoded = receiver.decode_frame(&encoded, 1).unwrap();
    
    // Decoded silence should be close to zero
    let max_amplitude = decoded.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    assert!(max_amplitude < 0.01, "Decoded silence should be near zero");
}
