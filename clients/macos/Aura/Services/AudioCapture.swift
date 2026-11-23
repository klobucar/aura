import Foundation
import Combine
import AVFoundation
import os

/// Audio capture service using AVAudioEngine.
/// Captures 48kHz, 16-bit, mono PCM audio.
@MainActor
public class AudioCapture: ObservableObject {
    
    // MARK: - Constants
    
    nonisolated public static let sampleRate: Double = 48000
    nonisolated public static let channelCount: AVAudioChannelCount = 1
    nonisolated public static let bufferMilliseconds: UInt32 = 20
    nonisolated public static let samplesPerFrame = UInt32(sampleRate * Double(bufferMilliseconds) / 1000) // 960
    
    // MARK: - Published State
    
    @Published public var isRunning = false
    @Published public var errorMessage: String?

    /// 20 ms frames handed to the handler. Counted on the tap's thread.
    public nonisolated var packetsSent: UInt64 { sentFrames.withLock { $0 } }
    
    // MARK: - Private Properties
    
    private var audioEngine: AVAudioEngine?
    private var inputNode: AVAudioInputNode?

    /// The running capture's handler, read by the tap for every buffer.
    /// `stop()` clears it first, so frames the tap produces after a stop are
    /// dropped rather than sent.
    private nonisolated let frameHandler = OSAllocatedUnfairLock<(@Sendable ([Float]) -> Void)?>(initialState: nil)
    private nonisolated let sentFrames = OSAllocatedUnfairLock(initialState: UInt64(0))
    
    public init() {}

    deinit {
        frameHandler.withLock { $0 = nil }
    }
    
    // MARK: - Public API
    
    /// Start capturing audio from the default microphone.
    /// - Parameter handler: Called with each 20 ms Float PCM frame, on the
    ///   capture tap's thread — never the main actor.
    public func start(handler: @escaping @Sendable ([Float]) -> Void) {
        guard !isRunning else { return }
        
        frameHandler.withLock { $0 = handler }
        
        do {
            audioEngine = AVAudioEngine()
            guard let engine = audioEngine else { return }
            
            inputNode = engine.inputNode
            guard let input = inputNode else { return }
            
            // Get the input node's native format
            let inputFormat = input.inputFormat(forBus: 0)
            print("[AudioCapture] Input format: \(inputFormat)")
            
            // Target format: 48kHz Float32 mono
            let targetFormat = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: Self.sampleRate,
                channels: Self.channelCount,
                interleaved: true
            )!
            
            let bufferSize = AVAudioFrameCount(Self.samplesPerFrame)
            let frameHandler = self.frameHandler
            let sentFrames = self.sentFrames
            
            // Install tap with input format, we'll convert if needed. The tap
            // runs on an audio thread: @Sendable keeps it off the main actor
            // (AVAudioNodeTapBlock isn't Sendable, so it would inherit it).
            input.installTap(onBus: 0, bufferSize: bufferSize, format: inputFormat) { @Sendable buffer, time in
                guard frameHandler.withLock({ $0 }) != nil else { return }

                let convertedBuffer: AVAudioPCMBuffer
                if inputFormat.sampleRate != Self.sampleRate || inputFormat.channelCount != Self.channelCount {
                    guard let converter = AVAudioConverter(from: inputFormat, to: targetFormat) else { return }
                    let capacity = AVAudioFrameCount(Double(buffer.frameLength) * Self.sampleRate / inputFormat.sampleRate)
                    guard let outputBuffer = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: capacity) else { return }
                    
                    var error: NSError?
                    converter.convert(to: outputBuffer, error: &error) { _, outStatus in
                        outStatus.pointee = .haveData
                        return buffer
                    }
                    convertedBuffer = outputBuffer
                } else {
                    convertedBuffer = buffer
                }
                
                guard let floatData = convertedBuffer.floatChannelData else { return }
                let frameCount = Int(convertedBuffer.frameLength)
                let samples = Array(UnsafeBufferPointer(start: floatData[0], count: frameCount))
                
                // Chunk into 20ms frames (960 samples)
                let samplesPerFrame = Int(Self.samplesPerFrame)
                let chunks = stride(from: 0, to: samples.count, by: samplesPerFrame).map { startIndex -> [Float] in
                    let endIndex = min(startIndex + samplesPerFrame, samples.count)
                    return Array(samples[startIndex..<endIndex])
                }
                
                // Straight to the handler, on this thread: a frame must not
                // wait for the main actor. Re-read per frame so none goes out
                // once `stop()` has run.
                for chunk in chunks where chunk.count == samplesPerFrame {
                    guard let handler = frameHandler.withLock({ $0 }) else { return }
                    sentFrames.withLock { $0 += 1 }
                    handler(chunk)
                }
            }
            
            try engine.start()
            
            isRunning = true
            errorMessage = nil
            print("[AudioCapture] Started - 48kHz Float32 mono (Voice Processing disabled - using Opus NS)")
            
        } catch {
            errorMessage = "Failed to start audio capture: \(error.localizedDescription)"
            print("[AudioCapture] Error: \(error)")
        }
    }
    
    /// Stop capturing audio.
    public func stop() {
        guard isRunning else { return }
        
        frameHandler.withLock { $0 = nil }
        inputNode?.removeTap(onBus: 0)
        audioEngine?.stop()
        audioEngine = nil
        inputNode = nil
        isRunning = false
        print("[AudioCapture] Stopped - \(packetsSent) packets sent")
    }
}
