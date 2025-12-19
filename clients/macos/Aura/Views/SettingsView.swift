import SwiftUI
import AVFoundation
import CoreAudio

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var settings: AudioSettings
    @ObservedObject var ttsManager: TtsManager
    @StateObject private var hotkeyManager = HotkeyManager.shared
    @StateObject private var deviceManager = AudioDeviceManager()
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Settings")
                    .font(.title2)
                    .fontWeight(.bold)
                Spacer()
                Button(action: { dismiss() }) {
                    Image(systemName: "xmark.circle.fill")
                        .font(.title2)
                        .foregroundColor(.secondary)
                }
                .buttonStyle(.plain)
            }
            .padding()
            
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    // Audio Devices Section
                    settingsSection("Audio Devices") {
                        VStack(alignment: .leading, spacing: 16) {
                            // Input Device
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Input Device (Microphone)")
                                    .font(.subheadline)
                                    .foregroundColor(.secondary)
                                
                                Picker("", selection: Binding(
                                    get: { deviceManager.selectedInputDeviceID },
                                    set: { if let deviceID = $0 { deviceManager.setInputDevice(deviceID) } }
                                )) {
                                    Text("System Default").tag(nil as AudioDeviceID?)
                                    ForEach(deviceManager.availableInputDevices) { device in
                                        Text(device.name).tag(device.id as AudioDeviceID?)
                                    }
                                }
                                .labelsHidden()
                            }
                            
                            // Output Device
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Output Device (Speakers)")
                                    .font(.subheadline)
                                    .foregroundColor(.secondary)
                                
                                Picker("", selection: Binding(
                                    get: { deviceManager.selectedOutputDeviceID },
                                    set: { if let deviceID = $0 { deviceManager.setOutputDevice(deviceID) } }
                                )) {
                                    Text("System Default").tag(nil as AudioDeviceID?)
                                    ForEach(deviceManager.availableOutputDevices) { device in
                                        Text(device.name).tag(device.id as AudioDeviceID?)
                                    }
                                }
                                .labelsHidden()
                            }
                        }
                    }
                    
                    Divider()
                    
                    // Transmission Mode Section
                    settingsSection("Transmission") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(AudioSettings.TransmissionMode.allCases, id: \.self) { mode in
                                HStack {
                                    Image(systemName: settings.transmissionMode == mode ? "largecircle.fill.circle" : "circle")
                                        .foregroundColor(settings.transmissionMode == mode ? .blue : .secondary)
                                    
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(mode.displayName)
                                            .font(.body)
                                        
                                        Text(transmissionModeDescription(mode))
                                            .font(.caption)
                                            .foregroundColor(.secondary)
                                    }
                                    
                                    Spacer()
                                }
                                .contentShape(Rectangle())
                                .onTapGesture {
                                    settings.transmissionMode = mode
                                }
                            }
                        }
                    }
                    
                    // Conditional settings based on mode
                    if settings.transmissionMode == .pushToTalk {
                        Divider()
                        pttSettings
                    } else if settings.transmissionMode == .voiceActivation {
                        Divider()
                        vadSettings
                    }
                    
                    Divider()
                    
                    // TTS Settings Section
                    settingsSection("Text-to-Speech") {
                        VStack(alignment: .leading, spacing: 12) {
                            Toggle("Enable Text-to-Speech", isOn: $ttsManager.settings.enabled)
                                .toggleStyle(.checkbox)
                            
                            if ttsManager.settings.enabled {
                                Toggle("Speak Chat Messages", isOn: $ttsManager.settings.speakChat)
                                    .toggleStyle(.checkbox)
                                    .padding(.leading, 20)
                                
                                Toggle("Speak Join/Leave Events", isOn: $ttsManager.settings.speakJoinLeave)
                                    .toggleStyle(.checkbox)
                                    .padding(.leading, 20)
                                
                                VStack(alignment: .leading, spacing: 4) {
                                    HStack {
                                        Text("Speech Rate")
                                            .font(.subheadline)
                                            .foregroundColor(.secondary)
                                        Spacer()
                                        Text(speedLabel)
                                            .font(.caption)
                                            .foregroundColor(.blue)
                                    }
                                    
                                    HStack {
                                        Image(systemName: "tortoise")
                                            .foregroundColor(.secondary)
                                        Slider(value: $ttsManager.settings.rate, in: 0.0...1.0)
                                            .accentColor(.blue)
                                        Image(systemName: "hare")
                                            .foregroundColor(.secondary)
                                    }
                                }
                                .padding(.leading, 20)
                                
                                VStack(alignment: .leading, spacing: 4) {
                                    HStack {
                                        Text("Volume")
                                            .font(.subheadline)
                                            .foregroundColor(.secondary)
                                        Spacer()
                                        Text("\(Int(ttsManager.settings.volume * 100))%")
                                            .font(.caption)
                                            .foregroundColor(.blue)
                                    }
                                    
                                    HStack {
                                        Image(systemName: "speaker")
                                            .foregroundColor(.secondary)
                                        Slider(value: $ttsManager.settings.volume, in: 0.0...1.0)
                                            .accentColor(.blue)
                                        Image(systemName: "speaker.wave.3")
                                            .foregroundColor(.secondary)
                                    }
                                }
                                .padding(.leading, 20)
                            }
                        }
                    }
                }
                .padding()
            }
            
            // Footer
            HStack {
                Spacer()
                Button("Done") {
                    settings.saveSettings()
                    dismiss()
                }
                .buttonStyle(.borderedProminent)
            }
            .padding()
        }
        .frame(width: 500, height: 650)
        .background(VisualEffectBlur(material: .hudWindow, blendingMode: .behindWindow))
    }
    
    // MARK: - PTT Settings
    
    private var pttSettings: some View {
        settingsSection("Push-to-Talk") {
            VStack(alignment: .leading, spacing: 12) {
                Text("Hotkey")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
                
                HotkeyRecorderButton(hotkey: $settings.pttHotkey)
                
                if !hotkeyManager.hasAccessibilityPermission {
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundColor(.orange)
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Accessibility Permission Required")
                                .font(.caption)
                                .fontWeight(.semibold)
                            Text("Aura needs accessibility permission to capture global hotkeys.")
                                .font(.caption2)
                                .foregroundColor(.secondary)
                        }
                    }
                    .padding(8)
                    .background(Color.orange.opacity(0.1))
                    .cornerRadius(6)
                    
                    Button("Open System Settings") {
                        hotkeyManager.requestAccessibilityPermission()
                    }
                    .buttonStyle(.bordered)
                }
                
                Text("Hold the hotkey to transmit audio")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
        }
    }
    
    // MARK: - VAD Settings
    
    private var vadSettings: some View {
        settingsSection("Voice Activation") {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("Sensitivity")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                    Spacer()
                    Text(sensitivityLabel)
                        .font(.caption)
                        .foregroundColor(.blue)
                }
                
                Slider(value: $settings.vadSensitivity, in: 0.0...1.0) {
                    Text("Sensitivity")
                }
                .accentColor(.blue)
                
                HStack {
                    Text("Very Sensitive")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                    Spacer()
                    Text("Loud Speech Only")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
                
                Text("Automatically transmit when speech is detected above the threshold")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
        }
    }
    
    // MARK: - Helper Views
    
    private func settingsSection<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title)
                .font(.headline)
            content()
        }
    }
    
    // MARK: - Helpers
    
    private func transmissionModeDescription(_ mode: AudioSettings.TransmissionMode) -> String {
        switch mode {
        case .pushToTalk: return "Hold a hotkey to transmit"
        case .alwaysOn: return "Continuous transmission"
        case .voiceActivation: return "Automatic speech detection"
        }
    }
    
    private var sensitivityLabel: String {
        let percent = Int(settings.vadSensitivity * 100)
        if settings.vadSensitivity < 0.3 {
            return "Very Sensitive (\(percent)%)"
        } else if settings.vadSensitivity < 0.7 {
            return "Moderate (\(percent)%)"
        } else {
            return "Less Sensitive (\(percent)%)"
        }
    }
    
    private var speedLabel: String {
        let percent = Int(ttsManager.settings.rate * 100)
        if ttsManager.settings.rate < 0.3 {
            return "Slow (\(percent)%)"
        } else if ttsManager.settings.rate < 0.7 {
            return "Normal (\(percent)%)"
        } else {
            return "Fast (\(percent)%)"
        }
    }
}
