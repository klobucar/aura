import Foundation
import AVFoundation
import Combine

/// Audio playback engine using AVAudioEngine
/// Receives PCM buffers from the network and plays them via the speaker
@MainActor
public class AudioPlayback: ObservableObject {
    
    // MARK: - Constants
    
    public static let sampleRate: Double = 48000
    public static let channelCount: AVAudioChannelCount = 1
    public static let frameSize = 960 // 20ms at 48kHz
    
    // MARK: - Published State
    
    @Published public var isPlaying = false
    @Published public var framesPlayed: UInt64 = 0
    
    // MARK: - Private Properties
    
    private var audioEngine: AVAudioEngine?
    private var playerNode: AVAudioPlayerNode?
    private let format: AVAudioFormat
    
    // Buffer queue for incoming audio
    private var bufferQueue: [AVAudioPCMBuffer] = []
    private let queueLock = NSLock()
    
    public init() {
        // Create audio format (48kHz, mono, float)
        format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: Self.sampleRate,
            channels: Self.channelCount,
            interleaved: true
        )!
    }
    
    // MARK: - Public API
    
    /// Start the audio playback engine
    public func start() {
        guard !isPlaying else { return }
        
        do {
            audioEngine = AVAudioEngine()
            guard let engine = audioEngine else { return }
            
            playerNode = AVAudioPlayerNode()
            guard let player = playerNode else { return }
            
            engine.attach(player)
            engine.connect(player, to: engine.mainMixerNode, format: format)
            
            try engine.start()
            player.play()
            
            isPlaying = true
            print("[AudioPlayback] Started - 48kHz mono")
            
        } catch {
            print("[AudioPlayback] Error starting: \\(error)")
        }
    }
    
    /// Stop the audio playback engine
    public func stop() {
        guard isPlaying else { return }
        
        playerNode?.stop()
        audioEngine?.stop()
        audioEngine = nil
        playerNode = nil
        isPlaying = false
        
        queueLock.lock()
        bufferQueue.removeAll()
        queueLock.unlock()
        
        print("[AudioPlayback] Stopped - \\(framesPlayed) frames played")
    }
    
    /// Enqueue PCM samples for playback
    /// - Parameter pcm: Array of Int16 PCM samples (960 samples = 20ms)
    public func enqueue(pcm: [Int16]) {
        guard pcm.count == Self.frameSize, let player = playerNode else {
            return
        }
        
        // Convert Int16 to Float32
        let floatSamples = pcm.map { sample -> Float in
            Float(sample) / Float(Int16.max)
        }
        
        // Create PCM buffer
        guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: UInt32(Self.frameSize)) else {
            return
        }
        
        buffer.frameLength = UInt32(Self.frameSize)
        
        if let floatData = buffer.floatChannelData {
            for i in 0..<Self.frameSize {
                floatData[0][i] = floatSamples[i]
            }
        }
        
        // Schedule buffer for playback
        player.scheduleBuffer(buffer) { [weak self] in
            Task { @MainActor in
                self?.framesPlayed += 1
            }
        }
    }
}
