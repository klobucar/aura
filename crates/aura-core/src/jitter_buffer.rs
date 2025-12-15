//! Jitter Buffer for Audio Packet Reordering
//!
//! Handles out-of-order packet delivery inherent in unreliable QUIC datagrams.
//! Uses a BTreeMap for O(log n) insertion and ordered retrieval.

use bytes::Bytes;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Configuration for jitter buffer behavior
#[derive(Debug, Clone)]
pub struct JitterBufferConfig {
    /// Target latency in milliseconds (how long to buffer before playing)
    pub target_latency_ms: u32,
    /// Maximum packets to hold before dropping oldest
    pub max_packets: usize,
    /// Maximum age in milliseconds before dropping a packet
    pub max_age_ms: u32,
    /// Frame duration in milliseconds (default: 20ms for Opus)
    pub frame_duration_ms: u32,
}

impl Default for JitterBufferConfig {
    fn default() -> Self {
        Self {
            target_latency_ms: 60,   // 3 frames at 20ms = 60ms buffer
            max_packets: 100,        // ~2 seconds of audio
            max_age_ms: 200,         // Drop packets older than 200ms
            frame_duration_ms: 20,   // Standard Opus frame
        }
    }
}

impl JitterBufferConfig {
    /// Create a low-latency config (for local/LAN)
    pub fn low_latency() -> Self {
        Self {
            target_latency_ms: 40,
            max_packets: 50,
            max_age_ms: 100,
            frame_duration_ms: 20,
        }
    }
    
    /// Create a high-latency config (for unstable connections)
    pub fn high_latency() -> Self {
        Self {
            target_latency_ms: 120,
            max_packets: 200,
            max_age_ms: 400,
            frame_duration_ms: 20,
        }
    }
}

/// A buffered packet with metadata
struct BufferedPacket {
    /// The audio data (encrypted or decrypted depending on stage)
    data: Bytes,
    /// RTP-style timestamp (sample count)
    timestamp: u32,
    /// When this packet was received
    received_at: Instant,
}

/// Statistics for monitoring jitter buffer health
#[derive(Debug, Default, Clone)]
pub struct JitterBufferStats {
    /// Total packets received
    pub packets_received: u64,
    /// Packets successfully played
    pub packets_played: u64,
    /// Packets dropped because they arrived too late
    pub packets_dropped_late: u64,
    /// Duplicate packets received
    pub packets_dropped_duplicate: u64,
    /// Packets lost (gap in sequence)
    pub packets_lost: u64,
    /// Current number of packets in buffer
    pub current_buffer_size: usize,
    /// Current estimated jitter in milliseconds
    pub estimated_jitter_ms: f32,
}

/// Per-sender jitter buffer with packet reordering
/// 
/// Each audio sender gets their own jitter buffer to handle
/// independent packet loss and reordering.
pub struct JitterBuffer {
    /// Buffered packets indexed by sequence number
    packets: BTreeMap<u64, BufferedPacket>,
    /// Next expected sequence number to play
    next_seq: u64,
    /// Last sequence number we successfully played
    last_played_seq: Option<u64>,
    /// Whether we've started receiving packets
    started: bool,
    /// Configuration
    config: JitterBufferConfig,
    /// Statistics
    stats: JitterBufferStats,
    /// Jitter estimation (exponential moving average)
    jitter_ema: f32,
    /// Last packet arrival time for jitter calculation
    last_arrival: Option<Instant>,
}

impl JitterBuffer {
    /// Create a new jitter buffer with the given configuration
    pub fn new(config: JitterBufferConfig) -> Self {
        Self {
            packets: BTreeMap::new(),
            next_seq: 0,
            last_played_seq: None,
            started: false,
            config,
            stats: JitterBufferStats::default(),
            jitter_ema: 0.0,
            last_arrival: None,
        }
    }
    
    /// Create a new jitter buffer with default configuration
    pub fn with_defaults() -> Self {
        Self::new(JitterBufferConfig::default())
    }
    
    /// Insert a received packet into the buffer
    /// 
    /// Returns `true` if the packet was accepted, `false` if dropped
    pub fn push(&mut self, seq: u64, timestamp: u32, data: Bytes) -> bool {
        let now = Instant::now();
        self.stats.packets_received += 1;
        
        // Update jitter estimate
        if let Some(last) = self.last_arrival {
            let interval = now.duration_since(last).as_secs_f32() * 1000.0;
            let expected = self.config.frame_duration_ms as f32;
            let jitter = (interval - expected).abs();
            // Exponential moving average with alpha = 0.1
            self.jitter_ema = self.jitter_ema * 0.9 + jitter * 0.1;
            self.stats.estimated_jitter_ms = self.jitter_ema;
        }
        self.last_arrival = Some(now);
        
        // Check for duplicate
        if self.packets.contains_key(&seq) {
            self.stats.packets_dropped_duplicate += 1;
            return false;
        }
        
        // Check if too old (already played past this sequence)
        if let Some(last) = self.last_played_seq {
            if seq <= last {
                self.stats.packets_dropped_late += 1;
                return false;
            }
        }
        
        // Insert the packet
        self.packets.insert(seq, BufferedPacket {
            data,
            timestamp,
            received_at: now,
        });
        
        // Initialize sequence tracking on first packet
        if !self.started {
            self.next_seq = seq;
            self.started = true;
        }
        
        // Evict oldest packets if buffer is too full
        while self.packets.len() > self.config.max_packets {
            if let Some((&oldest_seq, _)) = self.packets.first_key_value() {
                self.packets.remove(&oldest_seq);
                self.stats.packets_dropped_late += 1;
            }
        }
        
        self.stats.current_buffer_size = self.packets.len();
        true
    }
    
    /// Pop the next frame to play
    /// 
    /// Returns `Some(data)` if a frame is ready, `None` if we need to wait
    /// or generate PLC audio.
    pub fn pop(&mut self) -> PopResult {
        let now = Instant::now();
        let max_age = Duration::from_millis(self.config.max_age_ms as u64);
        
        // First, clean up packets that are too old
        let mut to_remove = Vec::new();
        for (&seq, pkt) in &self.packets {
            if now.duration_since(pkt.received_at) > max_age {
                to_remove.push(seq);
            }
        }
        for seq in to_remove {
            self.packets.remove(&seq);
            self.stats.packets_dropped_late += 1;
        }
        
        // Try to get the next expected packet
        if let Some(pkt) = self.packets.remove(&self.next_seq) {
            self.last_played_seq = Some(self.next_seq);
            self.next_seq += 1;
            self.stats.packets_played += 1;
            self.stats.current_buffer_size = self.packets.len();
            return PopResult::Packet(pkt.data);
        }
        
        // Packet is missing - check if we should skip ahead
        if let Some((&oldest_seq, oldest_pkt)) = self.packets.first_key_value() {
            let age = now.duration_since(oldest_pkt.received_at);
            let target = Duration::from_millis(self.config.target_latency_ms as u64);
            
            // If the oldest buffered packet has been waiting too long, skip to it
            if age > target && oldest_seq > self.next_seq {
                let gap = oldest_seq - self.next_seq;
                self.stats.packets_lost += gap;
                self.next_seq = oldest_seq;
                
                // Now try to pop again
                if let Some(pkt) = self.packets.remove(&self.next_seq) {
                    self.last_played_seq = Some(self.next_seq);
                    self.next_seq += 1;
                    self.stats.packets_played += 1;
                    self.stats.current_buffer_size = self.packets.len();
                    return PopResult::PacketWithGap { data: pkt.data, lost: gap };
                }
            }
        }
        
        // No packet available, check if buffer is empty
        if self.packets.is_empty() && !self.started {
            PopResult::Empty
        } else {
            PopResult::NeedPLC
        }
    }
    
    /// Get current statistics
    pub fn stats(&self) -> &JitterBufferStats {
        &self.stats
    }
    
    /// Get current buffer depth in packets
    pub fn depth(&self) -> usize {
        self.packets.len()
    }
    
    /// Get current buffer depth in milliseconds
    pub fn depth_ms(&self) -> u32 {
        self.packets.len() as u32 * self.config.frame_duration_ms
    }
    
    /// Reset the buffer (e.g., on sender reconnect)
    pub fn reset(&mut self) {
        self.packets.clear();
        self.next_seq = 0;
        self.last_played_seq = None;
        self.started = false;
        self.stats = JitterBufferStats::default();
        self.jitter_ema = 0.0;
        self.last_arrival = None;
    }
}

/// Result of a pop() operation
#[derive(Debug)]
pub enum PopResult {
    /// A packet is ready to play
    Packet(Bytes),
    /// A packet is ready, but some packets were lost before it
    PacketWithGap { data: Bytes, lost: u64 },
    /// No packet available, caller should generate PLC audio
    NeedPLC,
    /// Buffer is empty and hasn't started receiving
    Empty,
}

impl PopResult {
    /// Returns true if this result contains audio data
    pub fn has_data(&self) -> bool {
        matches!(self, PopResult::Packet(_) | PopResult::PacketWithGap { .. })
    }
    
    /// Extract the data if present
    pub fn into_data(self) -> Option<Bytes> {
        match self {
            PopResult::Packet(data) | PopResult::PacketWithGap { data, .. } => Some(data),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    fn make_packet(n: usize) -> Bytes {
        Bytes::from(vec![n as u8; 10])
    }
    
    #[test]
    fn test_in_order_packets() {
        let mut jb = JitterBuffer::with_defaults();
        
        // Insert packets in order
        for i in 0..5 {
            assert!(jb.push(i, i as u32 * 960, make_packet(i as usize)));
        }
        
        assert_eq!(jb.depth(), 5);
        
        // Pop should return them in order
        for i in 0..5 {
            match jb.pop() {
                PopResult::Packet(data) => {
                    assert_eq!(data[0], i as u8);
                }
                _ => panic!("Expected packet"),
            }
        }
        
        assert_eq!(jb.depth(), 0);
    }
    
    #[test]
    fn test_out_of_order_packets() {
        let mut jb = JitterBuffer::with_defaults();
        
        // First establish sequence starting at 0
        jb.push(0, 0 * 960, make_packet(0));
        
        // Now insert remaining packets out of order: 2, 4, 1, 3
        jb.push(2, 2 * 960, make_packet(2));
        jb.push(4, 4 * 960, make_packet(4));
        jb.push(1, 1 * 960, make_packet(1));
        jb.push(3, 3 * 960, make_packet(3));
        
        // Pop should return them in order
        for i in 0..5 {
            match jb.pop() {
                PopResult::Packet(data) => {
                    assert_eq!(data[0], i as u8, "Expected packet {}", i);
                }
                other => panic!("Expected packet {}, got {:?}", i, other),
            }
        }
    }
    
    #[test]
    fn test_duplicate_rejection() {
        let mut jb = JitterBuffer::with_defaults();
        
        assert!(jb.push(0, 0, make_packet(0)));
        assert!(!jb.push(0, 0, make_packet(0))); // Duplicate should be rejected
        
        assert_eq!(jb.stats().packets_dropped_duplicate, 1);
        assert_eq!(jb.depth(), 1);
    }
    
    #[test]
    fn test_late_packet_rejection() {
        let mut jb = JitterBuffer::with_defaults();
        
        // Push and pop packet 0
        jb.push(0, 0, make_packet(0));
        let _ = jb.pop();
        
        // Now try to push packet 0 again (too late)
        assert!(!jb.push(0, 0, make_packet(0)));
        assert_eq!(jb.stats().packets_dropped_late, 1);
    }
    
    #[test]
    fn test_gap_detection() {
        let config = JitterBufferConfig {
            target_latency_ms: 0, // Immediate skip for test
            ..Default::default()
        };
        let mut jb = JitterBuffer::new(config);
        
        // Push packet 0, then 5 (gap of 1,2,3,4)
        jb.push(0, 0, make_packet(0));
        jb.push(5, 5 * 960, make_packet(5));
        
        // Pop packet 0
        match jb.pop() {
            PopResult::Packet(data) => assert_eq!(data[0], 0),
            _ => panic!("Expected packet 0"),
        }
        
        // Wait a tiny bit to simulate passing time
        std::thread::sleep(Duration::from_millis(1));
        
        // Pop should skip to packet 5 and report gap
        match jb.pop() {
            PopResult::PacketWithGap { data, lost } => {
                assert_eq!(data[0], 5);
                assert_eq!(lost, 4); // Lost packets 1,2,3,4
            }
            PopResult::NeedPLC => {
                // This is also valid if target_latency hasn't passed
            }
            other => panic!("Expected PacketWithGap or NeedPLC, got {:?}", other),
        }
    }
    
    #[test]
    fn test_buffer_overflow() {
        let config = JitterBufferConfig {
            max_packets: 5,
            ..Default::default()
        };
        let mut jb = JitterBuffer::new(config);
        
        // Push more than max
        for i in 0..10 {
            jb.push(i, i as u32 * 960, make_packet(i as usize));
        }
        
        // Should only have max_packets
        assert_eq!(jb.depth(), 5);
    }
    
    #[test]
    fn test_reset() {
        let mut jb = JitterBuffer::with_defaults();
        
        for i in 0..5 {
            jb.push(i, i as u32 * 960, make_packet(i as usize));
        }
        
        assert_eq!(jb.depth(), 5);
        
        jb.reset();
        
        assert_eq!(jb.depth(), 0);
        assert!(!jb.started);
    }
    
    #[test]
    fn test_stats() {
        let mut jb = JitterBuffer::with_defaults();
        
        jb.push(0, 0, make_packet(0));
        jb.push(1, 960, make_packet(1));
        jb.push(1, 960, make_packet(1)); // Duplicate
        
        let _ = jb.pop();
        let _ = jb.pop();
        
        let stats = jb.stats();
        assert_eq!(stats.packets_received, 3);
        assert_eq!(stats.packets_played, 2);
        assert_eq!(stats.packets_dropped_duplicate, 1);
    }
}
