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

    /// Notification listeners, kept as tasks so they can be cancelled from
    /// `deinit` without an unchecked wrapper around an observer token.
    nonisolated private struct Observers: Sendable {
        var deviceChange: Task<Void, Never>?
        var configChange: Task<Void, Never>?
    }
    private nonisolated let observers = OSAllocatedUnfairLock(initialState: Observers())

    /// The running capture's handler, read by the tap for every buffer.
    /// `stop()` clears it first, so frames the tap produces after a stop are
    /// dropped rather than sent.
    private nonisolated let frameHandler = OSAllocatedUnfairLock<(@Sendable ([Float]) -> Void)?>(initialState: nil)
    private nonisolated let sentFrames = OSAllocatedUnfairLock(initialState: UInt64(0))
    
    public init() {
        let task = Task { [weak self] in
            let changes = NotificationCenter.default
                .notifications(named: .audioInputDeviceChanged)
                .map { _ in () }
            for await _ in changes {
                self?.scheduleRestartForHardwareChange()
            }
        }
        observers.withLock { $0.deviceChange = task }
    }

    deinit {
        frameHandler.withLock { $0 = nil }
        let tasks = observers.withLock { state -> Observers in
            let current = state
            state = Observers()
            return current
        }
        tasks.deviceChange?.cancel()
        tasks.configChange?.cancel()
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
            let needsConversion = inputFormat.sampleRate != Self.sampleRate || inputFormat.channelCount != Self.channelCount
            let tapConverter: TapConverter?
            if needsConversion {
                let initialCapacity = max(
                    bufferSize * 2,
                    AVAudioFrameCount(ceil(Double(bufferSize) * Self.sampleRate / max(1, inputFormat.sampleRate))) + 64
                )
                tapConverter = TapConverter(from: inputFormat, to: targetFormat, initialCapacity: initialCapacity)
                if tapConverter == nil {
                    print("[AudioCapture] Failed to create AVAudioConverter for \(inputFormat)")
                }
            } else {
                tapConverter = nil
            }
            
            // Install tap with input format, we'll convert if needed. The tap
            // runs on an audio thread: @Sendable keeps it off the main actor
            // (AVAudioNodeTapBlock isn't Sendable, so it would inherit it).
            input.installTap(onBus: 0, bufferSize: bufferSize, format: inputFormat) { @Sendable buffer, time in
                guard frameHandler.withLock({ $0 }) != nil else { return }

                let convertedBuffer: AVAudioPCMBuffer
                if needsConversion {
                    guard let output = tapConverter?.convert(buffer) else { return }
                    convertedBuffer = output
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
            let configTask = Task { [weak self] in
                let changes = NotificationCenter.default
                    .notifications(named: .AVAudioEngineConfigurationChange, object: engine)
                    .map { _ in () }
                for await _ in changes {
                    self?.scheduleRestartForHardwareChange()
                }
            }
            let previous = observers.withLock { state -> Task<Void, Never>? in
                let old = state.configChange
                state.configChange = configTask
                return old
            }
            previous?.cancel()

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
        let configTask = observers.withLock { state -> Task<Void, Never>? in
            let old = state.configChange
            state.configChange = nil
            return old
        }
        configTask?.cancel()
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

/// Caches `AVAudioConverter` and a reusable output `AVAudioPCMBuffer` for the
/// lifetime of a single capture tap so 20ms callbacks do not allocate.
nonisolated private final class TapConverter: @unchecked Sendable {
    private let converter: AVAudioConverter
    private let targetFormat: AVAudioFormat
    private var outputBuffer: AVAudioPCMBuffer

    init?(from inputFormat: AVAudioFormat, to targetFormat: AVAudioFormat, initialCapacity: AVAudioFrameCount) {
        guard let converter = AVAudioConverter(from: inputFormat, to: targetFormat),
              let buffer = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: initialCapacity) else {
            return nil
        }
        self.converter = converter
        self.targetFormat = targetFormat
        self.outputBuffer = buffer
    }

    func convert(_ inputBuffer: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
        let ratio = targetFormat.sampleRate / max(1, inputBuffer.format.sampleRate)
        let neededCapacity = AVAudioFrameCount(ceil(Double(inputBuffer.frameLength) * ratio)) + 16
        if outputBuffer.frameCapacity < neededCapacity {
            guard let grown = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: neededCapacity) else {
                return nil
            }
            outputBuffer = grown
        }
        outputBuffer.frameLength = 0
        var error: NSError?
        var consumed = false
        converter.convert(to: outputBuffer, error: &error) { _, outStatus in
            if consumed {
                outStatus.pointee = .noDataNow
                return nil
            }
            consumed = true
            outStatus.pointee = .haveData
            return inputBuffer
        }
        if error != nil { return nil }
        return outputBuffer
    }
}
