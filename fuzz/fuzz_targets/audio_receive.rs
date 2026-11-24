//! The client's audio receive path end to end: datagram parse, per-sender key
//! lookup across epochs, DAVE decrypt, jitter buffer, Opus decode and mixing.
//!
//! A forged datagram never survives the AEAD tag, so most inputs would stop at
//! decryption. `Valid` ops therefore encrypt an arbitrary payload under the
//! sender's real key: that is what a malicious group member can send, and it
//! puts arbitrary bytes in front of the Opus decoder.
#![no_main]

use arbitrary::Arbitrary;
use aura_core::audio_pipeline::AudioReceiver;
use aura_core::crypto::DaveCrypto;
use aura_protocol::FastAudioPacket;
use bytes::Bytes;
use libfuzzer_sys::fuzz_target;

const KEY_A: [u8; 32] = [1; 32];
const KEY_B: [u8; 32] = [2; 32];

#[derive(Arbitrary, Debug)]
enum Op {
    /// Whatever arrives on the datagram socket.
    Raw(Vec<u8>),
    /// A well-formed, correctly encrypted packet carrying arbitrary "Opus".
    Valid {
        session: u8,
        epoch_hint: u16,
        sequence: u16,
        nonce: [u8; 24],
        use_key_b: bool,
        payload: Vec<u8>,
    },
    AddSender {
        session: u8,
        epoch: u16,
    },
    UpdateKey {
        session: u8,
        epoch: u16,
    },
    RemoveSender {
        session: u8,
    },
    Gain {
        session: u8,
        gain: f32,
    },
    Mute {
        session: u8,
        muted: bool,
    },
    SetJitterMs(u32),
    PopDecoded,
    PopMixed,
}

fuzz_target!(|ops: Vec<Op>| {
    let rx = AudioReceiver::new();
    rx.add_sender(1, &KEY_A, 0).unwrap();
    for op in ops.into_iter().take(64) {
        match op {
            Op::Raw(bytes) => {
                let _ = rx.on_packet(Bytes::from(bytes));
            }
            Op::Valid {
                session,
                epoch_hint,
                sequence,
                nonce,
                use_key_b,
                payload,
            } => {
                let key = if use_key_b { &KEY_B } else { &KEY_A };
                let ciphertext = DaveCrypto::new(key)
                    .encrypt(&payload, &nonce)
                    .expect("encrypt");
                let packet = FastAudioPacket {
                    session_id: (session % 4) as u32,
                    epoch_hint,
                    sequence,
                    nonce,
                    payload: Bytes::from(ciphertext),
                };
                let _ = rx.on_packet(packet.to_bytes());
            }
            Op::AddSender { session, epoch } => {
                let _ = rx.add_sender((session % 4) as u32, &KEY_B, epoch);
            }
            Op::UpdateKey { session, epoch } => {
                rx.update_sender_key((session % 4) as u32, &KEY_B, epoch);
            }
            Op::RemoveSender { session } => rx.remove_sender((session % 4) as u32),
            Op::Gain { session, gain } => {
                rx.set_sender_gain((session % 4) as u32, gain);
            }
            Op::Mute { session, muted } => {
                rx.set_sender_muted((session % 4) as u32, muted);
            }
            Op::SetJitterMs(ms) => rx.set_jitter_buffer_ms(ms),
            Op::PopDecoded => {
                let _ = rx.pop_decoded();
            }
            Op::PopMixed => {
                let _ = rx.pop_mixed();
            }
        }
    }
});
