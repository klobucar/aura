import Foundation
import Combine
import AVFoundation

/// Audio capture service using AVAudioEngine.
/// Captures 48kHz, 16-bit, mono PCM audio.
@MainActor
public class AudioCapture: ObservableObject {
    
    // MARK: - Constants
    
    public static let sampleRate: Double = 48000
    public static let channelCount: AVAudioChannelCount = 1
    public static let bufferMilliseconds: UInt32 = 20
    public static let samplesPerFrame = UInt32(sampleRate * Double(bufferMilliseconds) / 1000) // 960
    
    // MARK: - Published State
    
    @Published public var isRunning = false
    @Published public var packetsSent: UInt64 = 0
    @Published public var errorMessage: String?
    
    // MARK: - Private Properties
    
    private var audioEngine: AVAudioEngine?
    private var inputNode: AVAudioInputNode?
    private var onAudioData: (([Float]) -> Void)?
    
    public init() {}
    
    // MARK: - Public API
    
    /// Start capturing audio from the default microphone.
    /// - Parameter handler: Callback with Float PCM audio data.
    public func start(handler: @escaping ([Float]) -> Void) {
        guard !isRunning else { return }
        
        onAudioData = handler
        
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
            
            // Install tap with input format, we'll convert if needed
            input.installTap(onBus: 0, bufferSize: bufferSize, format: inputFormat) { [weak self] buffer, time in
                guard let self = self else { return }
                
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
                
                Task { @MainActor in
                    for chunk in chunks {
                        if chunk.count == samplesPerFrame {
                            self.packetsSent += 1
                            self.onAudioData?(chunk)
                        }
                    }
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
        
        inputNode?.removeTap(onBus: 0)
        audioEngine?.stop()
        audioEngine = nil
        inputNode = nil
        isRunning = false
        onAudioData = nil
        print("[AudioCapture] Stopped - \(packetsSent) packets sent")
    }
}
