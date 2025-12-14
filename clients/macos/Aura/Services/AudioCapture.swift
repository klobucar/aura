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
    private var onAudioData: ((Data) -> Void)?
    
    public init() {}
    
    // MARK: - Public API
    
    /// Start capturing audio from the default microphone.
    /// - Parameter handler: Callback with PCM audio data.
    public func start(handler: @escaping (Data) -> Void) {
        guard !isRunning else { return }
        
        onAudioData = handler
        
        do {
            audioEngine = AVAudioEngine()
            guard let engine = audioEngine else { return }
            
            inputNode = engine.inputNode
            guard let input = inputNode else { return }
            
            // Use the input node's native format
            let inputFormat = input.inputFormat(forBus: 0)
            print("[AudioCapture] Input format: \(inputFormat)")
            
            // We'll need to convert to our target format (48kHz mono)
            let targetFormat = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: Self.sampleRate,
                channels: Self.channelCount,
                interleaved: true
            )!
            
            let bufferSize = AVAudioFrameCount(Self.samplesPerFrame)
            
            // Install tap with the input's native format
            input.installTap(onBus: 0, bufferSize: bufferSize, format: inputFormat) { [weak self] buffer, time in
                guard let self = self else { return }
                
                // Convert to target format if needed
                let convertedBuffer: AVAudioPCMBuffer
                if inputFormat.sampleRate != Self.sampleRate || inputFormat.channelCount != Self.channelCount {
                    // Need to convert
                    guard let converter = AVAudioConverter(from: inputFormat, to: targetFormat) else {
                        print("[AudioCapture] Failed to create converter")
                        return
                    }
                    
                    let capacity = AVAudioFrameCount(Double(buffer.frameLength) * Self.sampleRate / inputFormat.sampleRate)
                    guard let outputBuffer = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: capacity) else {
                        return
                    }
                    
                    var error: NSError?
                    converter.convert(to: outputBuffer, error: &error) { _, outStatus in
                        outStatus.pointee = .haveData
                        return buffer
                    }
                    
                    if let error = error {
                        print("[AudioCapture] Conversion error: \(error)")
                        return
                    }
                    
                    convertedBuffer = outputBuffer
                } else {
                    convertedBuffer = buffer
                }
                
                // Convert Float32 to Int16 PCM
                let pcmData = self.convertToInt16PCM(buffer: convertedBuffer)
                
                // Chunk into 20ms frames (960 samples = 1920 bytes) for datagram MTU
                let bytesPerFrame = 1920 // 960 samples * 2 bytes
                let chunks = stride(from: 0, to: pcmData.count, by: bytesPerFrame).map { startIndex in
                    let endIndex = min(startIndex + bytesPerFrame, pcmData.count)
                    return pcmData.subdata(in: startIndex..<endIndex)
                }
                
                Task { @MainActor in
                    for chunk in chunks {
                        if chunk.count > 0 {
                            self.packetsSent += 1
                            self.onAudioData?(chunk)
                        }
                    }
                }
            }
            
            try engine.start()
            isRunning = true
            errorMessage = nil
            print("[AudioCapture] Started - 48kHz, 16-bit mono, \(Self.bufferMilliseconds)ms frames")
            
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
    
    // MARK: - Audio Conversion
    
    private func convertToInt16PCM(buffer: AVAudioPCMBuffer) -> Data {
        guard let floatData = buffer.floatChannelData else {
            return Data()
        }
        
        let frameCount = Int(buffer.frameLength)
        var result = Data(count: frameCount * 2) // 2 bytes per Int16 sample
        
        result.withUnsafeMutableBytes { rawBuffer in
            let int16Buffer = rawBuffer.bindMemory(to: Int16.self)
            
            for i in 0..<frameCount {
                // Convert float (-1.0 to 1.0) to Int16
                let floatSample = floatData[0][i]
                let clampedSample = max(-1.0, min(1.0, floatSample))
                int16Buffer[i] = Int16(clampedSample * Float(Int16.max))
            }
        }
        
        return result
    }
}
