import Foundation
import Combine
import AVFoundation
import CoreAudio
import AudioToolbox
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
    private var activeHandler: (@Sendable ([Float]) -> Void)?
    private var boundDeviceID: AudioDeviceID?
    private var boundSampleRate: Double = 0
    private var boundChannelCount: AVAudioChannelCount = 0
    private var pendingRestartTask: Task<Void, Never>?

    nonisolated private struct ObserverBox: @unchecked Sendable {
        let token: NSObjectProtocol
    }

    nonisolated private struct Observers: Sendable {
        var deviceChange: ObserverBox?
        var configChange: ObserverBox?
    }
    private nonisolated let observers = OSAllocatedUnfairLock(initialState: Observers())

    /// The running capture's handler, read by the tap for every buffer.
    /// `stop()` clears it first, so frames the tap produces after a stop are
    /// dropped rather than sent.
    private nonisolated let frameHandler = OSAllocatedUnfairLock<(@Sendable ([Float]) -> Void)?>(initialState: nil)
    private nonisolated let sentFrames = OSAllocatedUnfairLock(initialState: UInt64(0))
    
    public init() {
        let token = NotificationCenter.default.addObserver(
            forName: .audioInputDeviceChanged,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                self?.scheduleRestartForHardwareChange()
            }
        }
        let box = ObserverBox(token: token)
        observers.withLock { $0.deviceChange = box }
    }

    deinit {
        frameHandler.withLock { $0 = nil }
        let removed = observers.withLock { state -> (ObserverBox?, ObserverBox?) in
            let pair = (state.deviceChange, state.configChange)
            state.deviceChange = nil
            state.configChange = nil
            return pair
        }
        if let box = removed.0 { NotificationCenter.default.removeObserver(box.token) }
        if let box = removed.1 { NotificationCenter.default.removeObserver(box.token) }
    }
    
    // MARK: - Public API
    
    /// Start capturing audio from the selected (or default) microphone.
    /// - Parameter handler: Called with each 20 ms Float PCM frame, on the
    ///   capture tap's thread — never the main actor.
    public func start(handler: @escaping @Sendable ([Float]) -> Void) {
        guard !isRunning else { return }
        
        activeHandler = handler
        frameHandler.withLock { $0 = handler }
        
        do {
            audioEngine = AVAudioEngine()
            guard let engine = audioEngine else { return }
            
            inputNode = engine.inputNode
            guard let input = inputNode else { return }

            // Route per-app input device without mutating system-wide defaults.
            let selectedID = AudioDeviceManager.shared.selectedInputDeviceID
            if let selectedID,
               selectedID != 0,
               let audioUnit = input.audioUnit {
                var deviceID = selectedID
                let status = AudioUnitSetProperty(
                    audioUnit,
                    kAudioOutputUnitProperty_CurrentDevice,
                    kAudioUnitScope_Global,
                    0,
                    &deviceID,
                    UInt32(MemoryLayout<AudioDeviceID>.size)
                )
                if status != noErr {
                    print("[AudioCapture] Warning: failed to bind input device \(selectedID) (status \(status))")
                }
            }
            boundDeviceID = selectedID

            // Get the input node's native format
            let inputFormat = input.inputFormat(forBus: 0)
            boundSampleRate = inputFormat.sampleRate
            boundChannelCount = inputFormat.channelCount
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

            // Install after start() so binding kAudioOutputUnitProperty_CurrentDevice
            // during setup cannot self-trigger a configuration-change restart loop.
            let prevConfigBox = observers.withLock { state -> ObserverBox? in
                let old = state.configChange
                state.configChange = nil
                return old
            }
            if let prevConfigBox {
                NotificationCenter.default.removeObserver(prevConfigBox.token)
            }
            let newConfigToken = NotificationCenter.default.addObserver(
                forName: .AVAudioEngineConfigurationChange,
                object: engine,
                queue: .main
            ) { [weak self] _ in
                Task { @MainActor in
                    self?.scheduleRestartForHardwareChange()
                }
            }
            let newConfigBox = ObserverBox(token: newConfigToken)
            observers.withLock { $0.configChange = newConfigBox }

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
        activeHandler = nil
        pendingRestartTask?.cancel()
        pendingRestartTask = nil
        stopEngine()
    }

    private func stopEngine() {
        guard isRunning else { return }

        frameHandler.withLock { $0 = nil }
        let configBox = observers.withLock { state -> ObserverBox? in
            let old = state.configChange
            state.configChange = nil
            return old
        }
        if let configBox {
            NotificationCenter.default.removeObserver(configBox.token)
        }
        inputNode?.removeTap(onBus: 0)
        audioEngine?.stop()
        audioEngine = nil
        inputNode = nil
        isRunning = false
        print("[AudioCapture] Stopped - \(packetsSent) packets sent")
    }

    private func scheduleRestartForHardwareChange() {
        guard isRunning, activeHandler != nil else { return }
        pendingRestartTask?.cancel()
        pendingRestartTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(150))
            guard !Task.isCancelled, let self else { return }
            self.restartForHardwareChange()
        }
    }

    private func restartForHardwareChange() {
        guard isRunning, let handler = activeHandler else { return }
        // Loop breaker: if the engine is still running with the exact same device
        // and native format, nothing actually needs a restart.
        if let engine = audioEngine,
           engine.isRunning,
           let input = inputNode,
           AudioDeviceManager.shared.selectedInputDeviceID == boundDeviceID {
            let fmt = input.inputFormat(forBus: 0)
            if fmt.sampleRate == boundSampleRate && fmt.channelCount == boundChannelCount {
                return
            }
        }
        print("[AudioCapture] Hardware configuration changed; restarting capture...")
        stopEngine()
        start(handler: handler)
    }
}
