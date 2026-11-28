import Combine
import SwiftUI

struct AudioSettingsView: View {
    var isInVoiceCall: Bool = false
    @AppStorage("noiseSuppressionEnabled") private var noiseSuppressionEnabled = true
    @AppStorage("aecEnabled") private var aecEnabled = false
    @AppStorage("webrtcNsEnabled") private var webrtcNsEnabled = false
    @AppStorage("webrtcAgcEnabled") private var webrtcAgcEnabled = true
    @AppStorage("masterVolume") private var masterVolume = 1.0
    @AppStorage("jitterBufferMs") private var jitterBufferMs = 20
    @StateObject private var loopback = MicLoopbackController()
    @ObservedObject private var deviceManager = AudioDeviceManager.shared

    var body: some View {
        Form {
            Section {
                HStack(spacing: 10) {
                    Image(systemName: "speaker.fill")
                        .foregroundStyle(.secondary)
                    Slider(value: $masterVolume, in: 0...1)
                        .onChange(of: masterVolume) { _, newValue in
                            NotificationCenter.default.post(
                                name: .audioSettingsChanged,
                                object: ["masterVolume": Float(newValue)]
                            )
                        }
                    Image(systemName: "speaker.wave.3.fill")
                        .foregroundStyle(.secondary)
                }

                Text("\(Int((masterVolume * 100).rounded()))% output volume")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Output")
            }

            Section {
                Toggle("Test Mic (Loopback)", isOn: Binding(
                    get: { loopback.isTesting },
                    set: { enabled in
                        loopback.setTesting(
                            enabled,
                            outputDeviceID: deviceManager.selectedOutputDeviceID,
                            volume: Float(masterVolume)
                        )
                    }
                ))
                .disabled(isInVoiceCall)
                if loopback.isTesting {
                    MicLoopbackMeter(db: loopback.inputLevelDb)
                }
                Text(isInVoiceCall ? "Unavailable while connected to a voice channel" : "Hear your microphone locally before joining a channel")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Mic Test")
            }

            Section("Audio Quality") {
                Toggle("Noise Suppression (RNNoise)", isOn: $noiseSuppressionEnabled)
                    .onChange(of: noiseSuppressionEnabled) { _, newValue in
                        NotificationCenter.default.post(
                            name: .audioSettingsChanged,
                            object: ["noiseSuppression": newValue]
                        )
                    }

                Text("Neural network-based background noise removal")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section {
                Toggle("Echo Cancellation (AEC)", isOn: $aecEnabled)
                    .onChange(of: aecEnabled) { _, newValue in
                        NotificationCenter.default.post(
                            name: .audioSettingsChanged,
                            object: ["aecEnabled": newValue]
                        )
                    }

                Toggle("Noise Suppression (WebRTC)", isOn: $webrtcNsEnabled)
                    .onChange(of: webrtcNsEnabled) { _, newValue in
                        NotificationCenter.default.post(
                            name: .audioSettingsChanged,
                            object: ["webrtcNsEnabled": newValue]
                        )
                    }

                Toggle("Automatic Gain Control (AGC)", isOn: $webrtcAgcEnabled)
                    .onChange(of: webrtcAgcEnabled) { _, newValue in
                        NotificationCenter.default.post(
                            name: .audioSettingsChanged,
                            object: ["webrtcAgcEnabled": newValue]
                        )
                    }

                Text("WebRTC preprocessing. AEC removes speaker echo on open-mic setups; AGC normalizes input level.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Preprocessing")
            }

            Section {
                Picker("Jitter Buffer", selection: $jitterBufferMs) {
                    Text("0ms (Instant)").tag(0)
                    Text("10ms (Minimal)").tag(10)
                    Text("20ms (Ultra Low)").tag(20)
                    Text("40ms (Low)").tag(40)
                    Text("60ms (Balanced)").tag(60)
                    Text("80ms (Stable)").tag(80)
                    Text("100ms (Maximum)").tag(100)
                }
                .onChange(of: jitterBufferMs) { _, newValue in
                    NotificationCenter.default.post(
                        name: .audioSettingsChanged,
                        object: ["jitterBuffer": newValue]
                    )
                }
                
                if jitterBufferMs == 0 {
                    Label {
                        Text("0ms is only for LAN/localhost. Internet connections will sound choppy due to packet reordering.")
                    } icon: {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                    }
                    .font(.caption)
                } else {
                    Text("Lower = less delay, higher = more stable on poor connections")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } header: {
                Text("Latency")
            }
        }
        .navigationTitle("Audio Settings")
        .onChange(of: isInVoiceCall) { _, inCall in
            if inCall { loopback.stop() }
        }
        .onDisappear { loopback.stop() }
    }
}

private actor MicLoopbackOutput {
    private let playback = AudioPlayback(postsRestartNotification: false)
    private var isRunning = false

    func start(deviceID: UInt32?, volume: Float) {
        guard !isRunning else { return }
        playback.setOutputDevice(deviceID)
        playback.setVolume(volume)
        playback.start()
        isRunning = true
    }

    func stop() {
        guard isRunning else { return }
        playback.stop()
        isRunning = false
    }

    func enqueue(_ samples: [Float]) {
        guard isRunning, !samples.isEmpty else { return }
        let pcm = samples.map { sample -> Int16 in
            let clamped = max(-1.0, min(1.0, sample))
            return Int16(clamped * Float(Int16.max))
        }
        playback.enqueue(pcm: pcm)
    }
}

@MainActor
final class MicLoopbackController: ObservableObject {
    @Published var isTesting = false
    @Published var inputLevelDb: Float = -60

    private let capture = AudioCapture()
    private let output = MicLoopbackOutput()

    func setTesting(_ enabled: Bool, outputDeviceID: UInt32?, volume: Float) {
        if enabled {
            start(outputDeviceID: outputDeviceID, volume: volume)
        } else {
            stop()
        }
    }

    func start(outputDeviceID: UInt32?, volume: Float) {
        guard !isTesting else { return }
        isTesting = true
        let out = output
        Task {
            await out.start(deviceID: outputDeviceID, volume: volume)
        }
        capture.start { [weak self] samples in
            var sum: Float = 0
            for s in samples { sum += s * s }
            let rms = sqrt(sum / Float(max(1, samples.count)))
            let db = rms > 0 ? max(-60, 20 * log10(rms)) : -60
            Task { @MainActor [weak self] in
                if self?.isTesting == true {
                    self?.inputLevelDb = db
                }
            }
            Task {
                await out.enqueue(samples)
            }
        }
    }

    func stop() {
        guard isTesting else { return }
        isTesting = false
        inputLevelDb = -60
        capture.stop()
        let out = output
        Task {
            await out.stop()
        }
    }
}

struct MicLoopbackMeter: View {
    let db: Float

    private var level: Double {
        let clamped = max(-60, min(0, Double(db)))
        return (clamped + 60) / 60
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule()
                    .fill(Color.white.opacity(0.10))
                Capsule()
                    .fill(AuraTheme.Gradients.primary)
                    .frame(width: max(4, geo.size.width * level))
            }
        }
        .frame(height: 6)
        .animation(.easeOut(duration: 0.08), value: level)
    }
}

#Preview {
    NavigationStack {
        AudioSettingsView()
    }
}
