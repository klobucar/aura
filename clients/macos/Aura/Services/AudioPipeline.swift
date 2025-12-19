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
    
    private var sender: AudioSenderWrapper?
    private var receiver: AudioReceiverWrapper?
    
    private var sessionId: UInt32 = 0
    private var encryptionKey: Data?
    private var epochHint: UInt16 = 0
    
    // MARK: - Constants
    
    /// Frame size: 20ms at 48kHz mono = 960 samples
    public static let frameSize = 960
    
    /// Sample rate in Hz
    public static let sampleRate = 48000
    
    // MARK: - Initialize
    
    /// Initialize the audio pipeline with session ID and encryption key
    public func initialize(sessionId: UInt32, key: Data) throws {
        self.sessionId = sessionId
        self.encryptionKey = key
        
        // Create Rust wrappers
        self.sender = try AudioSenderWrapper(sessionId: sessionId, key: key)
        self.receiver = AudioReceiverWrapper()
        
        self.isInitialized = true
        print("[AudioPipeline] Initialized for session \(sessionId)")
    }
    
    /// Add a remote sender's key for decryption
    public func addSender(sessionId: UInt32, key: Data) throws {
        guard let receiver = receiver else { throw AudioPipelineError.notInitialized }
        
        try receiver.addSender(sessionId: sessionId, key: key, epochHint: epochHint)
        activeTransmitters.insert(sessionId)
        print("[AudioPipeline] Added sender \(sessionId)")
    }
    
    /// Remove a sender (when they leave)
    public func removeSender(sessionId: UInt32) {
        receiver?.removeSender(sessionId: sessionId)
        activeTransmitters.remove(sessionId)
        print("[AudioPipeline] Removed sender \(sessionId)")
    }
    
    /// Set current MLS epoch
    public func setEpoch(_ epoch: UInt64) {
        epochHint = UInt16(truncatingIfNeeded: epoch)
        sender?.setEpoch(epoch: epoch)
    }
    
    /// Set DRED duration (0-100 frames, each 10ms)
    public func setDredDuration(_ duration: Int32) {
        sender?.setDredDuration(duration: duration)
    }
    
    // MARK: - Transmit Pipeline
    
    /// Process PCM audio for transmission
    public func process(pcm: [Int16]) throws -> Data {
        guard let sender = sender else { throw AudioPipelineError.notInitialized }
        let packet = try sender.process(pcm: pcm)
        return Data(packet)
    }
    
    /// Process float PCM for transmission (preferred for libopus 1.6)
    public func process(floatPcm: [Float]) throws -> Data {
        guard let sender = sender else { throw AudioPipelineError.notInitialized }
        let packet = try sender.processFloat(pcm: floatPcm)
        return Data(packet)
    }
    
    public func getSequence() -> UInt16 {
        return sender?.sequence() ?? 0
    }
    
    // MARK: - Receive Pipeline
    
    /// Process received QUIC datagram
    public func onPacketReceived(_ data: Data) throws {
        guard let receiver = receiver else { throw AudioPipelineError.notInitialized }
        try receiver.onPacket(data: data)
    }
    
    /// Pop mixed audio from all senders with speaker metadata
    /// Call this every 20ms to get playback audio
    /// Returns mixed PCM and list of active speaker session IDs
    public func popMixed() -> MixedAudioResult? {
        return receiver?.popMixed()
    }
    
    // MARK: - Cleanup
    
    public func reset() {
        sender = nil
        receiver = nil
        sessionId = 0
        encryptionKey = nil
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
