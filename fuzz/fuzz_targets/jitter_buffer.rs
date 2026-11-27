//! Jitter buffer driven by an arbitrary stream of pushes, pops and resets with
//! u16 sequence numbers (all the receive path ever supplies), arbitrary
//! timestamps and a configuration in the range production uses.
#![no_main]

use arbitrary::Arbitrary;
use aura_core::jitter_buffer::{JitterBuffer, JitterBufferConfig};
use bytes::Bytes;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Config {
    target_latency_ms: u32,
    max_packets: u8,
    max_age_ms: u32,
    frame_duration_ms: u32,
}

#[derive(Arbitrary, Debug)]
enum Op {
    Push {
        seq: u16,
        timestamp: u32,
        len: u8,
    },
    /// Sequence close to the previous one, as a real stream is.
    PushNext {
        delta: i8,
        timestamp: u32,
    },
    Pop,
    Reset,
    SetLatency(u32),
    Inspect,
}

#[derive(Arbitrary, Debug)]
struct Input {
    config: Config,
    ops: Vec<Op>,
}

fuzz_target!(|input: Input| {
    let mut jb = JitterBuffer::new(JitterBufferConfig {
        target_latency_ms: input.config.target_latency_ms,
        max_packets: input.config.max_packets as usize,
        max_age_ms: input.config.max_age_ms,
        // Production uses fixed config (<= 1 s); depth_ms multiplies
        // by the packet count, so arbitrary values only find overflows
        // that cannot happen.
        frame_duration_ms: input.config.frame_duration_ms % 1001,
        ..Default::default()
    });
    let mut last = 0u16;
    for op in input.ops {
        match op {
            Op::Push {
                seq,
                timestamp,
                len,
            } => {
                last = seq;
                jb.push(
                    u64::from(seq),
                    timestamp,
                    Bytes::from(vec![0u8; len as usize]),
                );
            }
            Op::PushNext { delta, timestamp } => {
                last = last.wrapping_add(delta as i16 as u16);
                jb.push(u64::from(last), timestamp, Bytes::from_static(b"x"));
            }
            Op::Pop => {
                let _ = jb.pop().has_data();
            }
            Op::Reset => jb.reset(),
            Op::SetLatency(ms) => jb.set_target_latency(ms),
            Op::Inspect => {
                let _ = (jb.depth(), jb.depth_ms(), jb.stats().clone());
            }
        }
    }
});
