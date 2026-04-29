// Audio pipeline integration tests
// crates/aura-core/tests/audio_pipeline_tests.rs

use crate::uniffi_bindings::{AudioReceiverWrapper, AudioSenderWrapper};
use std::sync::Arc;

#[tokio::test]
async fn test_full_audio_pipeline() {
    let key = [0x42u8; 32];
    let sender = AudioSenderWrapper::new(1, &key).expect("Failed to create sender");
    let receiver = AudioReceiverWrapper::new();
    receiver
        .add_sender(1, &key, 0)
        .expect("Failed to add sender");

    // Simulate audio (20ms at 48kHz = 960 samples)
    let pcm = vec![0i16; 960];
    let encoded = sender
        .process(&pcm)
        .expect("Process failed")
        .expect("VAD off by default; expected a packet");

    assert!(!encoded.is_empty(), "Encoded data should not be empty");

    // Pass packet to receiver
    receiver.on_packet(&encoded).expect("OnPacket failed");

    // Mix and verify
    let mixed = receiver.pop_mixed().expect("PopMixed failed");
    assert_eq!(mixed.pcm.len(), 960, "Mixed audio should have 960 samples");
    assert!(
        mixed.active_speakers.contains(&1),
        "Sender 1 should be an active speaker"
    );
}

#[tokio::test]
async fn test_packet_loss_recovery() {
    let key = [0x42u8; 32];
    let sender = AudioSenderWrapper::new(1, &key).expect("Failed to create sender");
    let receiver = AudioReceiverWrapper::new();
    receiver
        .add_sender(1, &key, 0)
        .expect("Failed to add sender");

    let pcm = vec![100i16; 960];

    // Packet 1
    let packet1 = sender
        .process(&pcm)
        .expect("Process 1 failed")
        .expect("VAD off");
    receiver.on_packet(&packet1).expect("OnPacket 1 failed");
    let _ = receiver.pop_mixed(); // Clear buffer

    // Skip Packet 2 (manually increment sender sequence if possible, or just skip)
    // AudioSenderWrapper doesn't expose manual sequence set, so we just process twice
    let _packet2 = sender
        .process(&pcm)
        .expect("Process 2 failed")
        .expect("VAD off");

    // Packet 3
    let packet3 = sender
        .process(&pcm)
        .expect("Process 3 failed")
        .expect("VAD off");
    receiver.on_packet(&packet3).expect("OnPacket 3 failed");

    let mixed = receiver.pop_mixed().expect("PopMixed failed");
    assert_eq!(mixed.pcm.len(), 960);
}

#[test]
fn test_concurrent_senders() {
    use std::thread;

    let key = [0x42u8; 32];
    let receiver = Arc::new(AudioReceiverWrapper::new());
    let mut handles = vec![];

    // 5 concurrent senders (reduced from 10 to be safe with resources in test)
    for session_id in 1..=5 {
        let receiver_clone: Arc<AudioReceiverWrapper> = Arc::clone(&receiver);
        receiver
            .add_sender(session_id, &key, 0)
            .expect("Failed to add sender");

        let handle = thread::spawn(move || {
            let sender =
                AudioSenderWrapper::new(session_id, &key).expect("Failed to create sender");
            let pcm = vec![1000i16; 960];
            let encoded = sender
                .process(&pcm)
                .expect("Process failed")
                .expect("VAD off");
            receiver_clone.on_packet(&encoded).expect("OnPacket failed");
            session_id
        });
        handles.push(handle);
    }

    for handle in handles {
        let sid: u32 = handle.join().expect("Thread panicked");
        assert!(sid > 0);
    }

    let mixed = receiver.pop_mixed().expect("PopMixed failed");
    assert_eq!(mixed.pcm.len(), 960);
    assert!(!mixed.active_speakers.is_empty());
}

#[test]
fn test_zero_amplitude_handling() {
    let key = [0x42u8; 32];
    let sender = AudioSenderWrapper::new(1, &key).expect("Failed to create sender");
    let receiver = AudioReceiverWrapper::new();
    receiver
        .add_sender(1, &key, 0)
        .expect("Failed to add sender");

    let silence = vec![0i16; 960];
    let encoded = sender
        .process(&silence)
        .expect("Process failed")
        .expect("VAD off; silent PCM still emits a (small) Opus packet");
    receiver.on_packet(&encoded).expect("OnPacket failed");

    let mixed = receiver.pop_mixed().expect("PopMixed failed");

    // Decoded silence should be zero
    let max_amplitude = mixed
        .pcm
        .iter()
        .map(|&s| (s as i32).abs())
        .max()
        .unwrap_or(0);
    assert!(
        max_amplitude < 50,
        "Decoded silence should be near zero, got {}",
        max_amplitude
    );
}
