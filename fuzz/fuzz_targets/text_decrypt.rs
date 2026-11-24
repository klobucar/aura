//! `decrypt_text` with a fixed key and an arbitrary packet: must return an
//! error, never panic. Inputs are tried both as raw protobuf bytes and as a
//! structured packet whose field lengths the fuzzer controls.
#![no_main]

use arbitrary::Arbitrary;
use aura_core::text_crypto::{decrypt_text, EncryptedTextPacket};
use libfuzzer_sys::fuzz_target;
use prost::Message;

const KEY: [u8; 32] = [7; 32];

#[derive(Arbitrary, Debug)]
struct Packet {
    sender_session_id: u32,
    channel_id: String,
    epoch: u64,
    message_id: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
    tag: Vec<u8>,
    reply_to_id: String,
}

fuzz_target!(|data: &[u8]| {
    if let Ok(packet) = EncryptedTextPacket::decode(data) {
        let _ = decrypt_text(&KEY, &packet);
    }
    if let Ok(p) = Packet::arbitrary(&mut arbitrary::Unstructured::new(data)) {
        let packet = EncryptedTextPacket {
            sender_session_id: p.sender_session_id,
            channel_id: p.channel_id,
            epoch: p.epoch,
            message_id: p.message_id,
            ciphertext: p.ciphertext,
            nonce: p.nonce,
            tag: p.tag,
            reply_to_id: p.reply_to_id,
        };
        let _ = decrypt_text(&KEY, &packet);
    }
});
