import Foundation
import Combine

/// Swift wrapper for the Rust audio pipeline
/// 
/// Uses UniFFI-generated bindings when available, falls back to
/// native Swift implementation for development/testing.
///
/// Architecture:
/// ```
/// PCM Audio → AudioPipeline.process() → Encrypted Packet → QUIC Datagram
///                                                              ↓
/// Speaker ← AudioPipeline.decode() ← Decrypted PCM ← QUIC Datagram
/// ```
@MainActor
public class AudioPipeline: ObservableObject {
    
    // MARK: - State
    
    @Published public var isInitialized = false
    @Published public var activeTransmitters: Set<UInt32> = []
    
    private var sessionId: UInt32 = 0
    private var encryptionKey: Data?
    public private(set) var sequence: UInt16 = 0
    private var epochHint: UInt16 = 0
    
    // Other sender keys for decryption
    private var senderKeys: [UInt32: Data] = [:]
    
    // MARK: - Constants
    
    /// Frame size: 20ms at 48kHz mono = 960 samples
    public static let frameSize = 960
    
    /// Sample rate in Hz
    public static let sampleRate = 48000
    
    // MARK: - Initialize
    
    /// Initialize the audio pipeline with session ID and encryption key
    public func initialize(sessionId: UInt32, key: Data) throws {
        guard key.count == 32 else {
            throw AudioPipelineError.invalidKeySize
        }
        
        self.sessionId = sessionId
        self.encryptionKey = key
        self.sequence = 0
        self.isInitialized = true
        
        print("[AudioPipeline] Initialized for session \(sessionId)")
    }
    
    /// Add a remote sender's key for decryption
    public func addSender(sessionId: UInt32, key: Data) throws {
        guard key.count == 32 else {
            throw AudioPipelineError.invalidKeySize
        }
        
        senderKeys[sessionId] = key
        activeTransmitters.insert(sessionId)
        print("[AudioPipeline] Added sender \(sessionId)")
    }
    
    /// Remove a sender (when they leave)
    public func removeSender(sessionId: UInt32) {
        senderKeys.removeValue(forKey: sessionId)
        activeTransmitters.remove(sessionId)
        print("[AudioPipeline] Removed sender \(sessionId)")
    }
    
    /// Set current MLS epoch
    public func setEpoch(_ epoch: UInt64) {
        epochHint = UInt16(truncatingIfNeeded: epoch)
    }
    
    // MARK: - Transmit Pipeline
    
    /// Process PCM audio for transmission
    ///
    /// Pipeline: PCM → Opus → Zero-pad → Encrypt → Header → Packet
    ///
    /// - Parameter pcm: 960 samples of Int16 PCM (20ms at 48kHz)
    /// - Returns: Serialized packet ready for QUIC datagram
    public func process(pcm: [Int16]) throws -> Data {
        guard isInitialized, let _ = encryptionKey else {
            throw AudioPipelineError.notInitialized
        }
        
        guard pcm.count == Self.frameSize else {
            throw AudioPipelineError.invalidFrameSize
        }
        
        // TODO: Call Rust core via UniFFI
        // For now, create a packet with the raw PCM (temporary)
        
        // Build packet header (32 bytes)
        var packet = Data(capacity: 32 + pcm.count * 2)
        
        // SessionID: u32 (4 bytes)
        withUnsafeBytes(of: sessionId.littleEndian) { packet.append(contentsOf: $0) }
        
        // EpochHint: u16 (2 bytes)
        withUnsafeBytes(of: epochHint.littleEndian) { packet.append(contentsOf: $0) }
        
        // Sequence: u16 (2 bytes)
        withUnsafeBytes(of: sequence.littleEndian) { packet.append(contentsOf: $0) }
        sequence &+= 1
        
        // Nonce: 24 bytes (using sequence-based for now)
        var nonce = Data(repeating: 0, count: 24)
        withUnsafeBytes(of: sessionId.littleEndian) { nonce.replaceSubrange(0..<4, with: $0) }
        withUnsafeBytes(of: sequence.littleEndian) { nonce.replaceSubrange(4..<6, with: $0) }
        packet.append(nonce)
        
        // Payload: PCM data (temporary - should be Opus + encrypted)
        for sample in pcm {
            withUnsafeBytes(of: sample.littleEndian) { packet.append(contentsOf: $0) }
        }
        
        return packet
    }
    
    /// Process float PCM for transmission
    public func process(floatPcm: [Float]) throws -> Data {
        let pcm = floatPcm.map { sample -> Int16 in
            let clamped = max(-1.0, min(1.0, sample))
            return Int16(clamped * Float(Int16.max))
        }
        return try process(pcm: pcm)
    }
    
    // MARK: - Receive Pipeline
    
    /// Process received QUIC datagram
    ///
    /// Pipeline: Packet → Parse Header → Decrypt → Opus Decode → PCM
    ///
    /// - Parameter data: Raw QUIC datagram
    /// - Returns: Decoded PCM samples, or nil if not ready
    public func onPacketReceived(_ data: Data) throws {
        guard data.count >= 32 else {
            throw AudioPipelineError.packetTooShort
        }
        
        // TODO: Call Rust core via UniFFI
        // Parse header
        let senderSessionId = data.withUnsafeBytes { $0.load(fromByteOffset: 0, as: UInt32.self).littleEndian }
        
        // Check if we have this sender's key
        guard senderKeys[senderSessionId] != nil else {
            print("[AudioPipeline] Unknown sender: \(senderSessionId)")
            throw AudioPipelineError.unknownSender
        }
        
        // Would insert into jitter buffer and decrypt here
        print("[AudioPipeline] Received packet from sender \(senderSessionId)")
    }
    
    /// Pop mixed audio from all senders
    /// Call this every 20ms to get playback audio
    public func popMixed() -> [Int16]? {
        // TODO: Call Rust core via UniFFI
        // For now return nil (no audio)
        return nil
    }
    
    // MARK: - Cleanup
    
    public func reset() {
        sessionId = 0
        encryptionKey = nil
        sequence = 0
        senderKeys.removeAll()
        activeTransmitters.removeAll()
        isInitialized = false
    }
}

// MARK: - Errors

public enum AudioPipelineError: Error, LocalizedError {
    case notInitialized
    case invalidKeySize
    case invalidFrameSize
    case packetTooShort
    case unknownSender
    case encryptionFailed
    case decryptionFailed
    case opusEncodeFailed
    case opusDecodeFailed
    
    public var errorDescription: String? {
        switch self {
        case .notInitialized: return "Audio pipeline not initialized"
        case .invalidKeySize: return "Key must be 32 bytes"
        case .invalidFrameSize: return "Frame must be 960 samples"
        case .packetTooShort: return "Packet too short (< 32 bytes)"
        case .unknownSender: return "Unknown sender"
        case .encryptionFailed: return "Encryption failed"
        case .decryptionFailed: return "Decryption failed"
        case .opusEncodeFailed: return "Opus encoding failed"
        case .opusDecodeFailed: return "Opus decoding failed"
        }
    }
}
