import SwiftUI

struct AudioSettingsView: View {
    @AppStorage("noiseSuppressionEnabled") private var noiseSuppressionEnabled = true
    @AppStorage("jitterBufferMs") private var jitterBufferMs = 20
    
    var body: some View {
        Form {
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
    }
}

#Preview {
    NavigationStack {
        AudioSettingsView()
    }
}
