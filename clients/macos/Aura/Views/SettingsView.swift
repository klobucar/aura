import SwiftUI
import AVFoundation
import CoreAudio

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var settings: AudioSettings
    @ObservedObject var ttsManager: TtsManager
    @StateObject private var appSettings = AppSettings.shared
    @StateObject private var hotkeyManager = HotkeyManager.shared
    @StateObject private var deviceManager = AudioDeviceManager()
    
    // Audio Quality Settings
    @AppStorage("noiseSuppressionEnabled") private var noiseSuppressionEnabled = true
    @AppStorage("aecEnabled") private var aecEnabled = true
    @AppStorage("webrtcNsEnabled") private var webrtcNsEnabled = false
    @AppStorage("webrtcAgcEnabled") private var webrtcAgcEnabled = true
    @AppStorage("jitterBufferMs") private var jitterBufferMs = 20
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Settings")
                        .font(.system(size: 24, weight: .bold))
                    Text("Customize your Aura experience")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                }
                Spacer()
                Button(action: { dismiss() }) {
                    Image(systemName: "xmark")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundColor(.secondary)
                        .padding(8)
                        .background(Circle().fill(Color.white.opacity(0.1)))
                }
                .buttonStyle(.plain)
                .auraFluidHover()
            }
            .padding(24)
            
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    // Appearance Section
                    settingsSection("Appearance", icon: "paintbrush.fill") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(AuraThemeType.allCases, id: \.self) { theme in
                                themeRow(theme: theme)
                            }
                        }
                    }

                    // Audio Devices Section
                    settingsSection("Audio Devices", icon: "hifispeaker.2.fill") {
                        VStack(alignment: .leading, spacing: 16) {
                            devicePicker(
                                title: "Input Device",
                                subtitle: "Select your microphone",
                                selection: Binding(
                                    get: { deviceManager.selectedInputDeviceID },
                                    set: { if let deviceID = $0 { deviceManager.setInputDevice(deviceID) } }
                                ),
                                devices: deviceManager.availableInputDevices
                            )
                            
                            devicePicker(
                                title: "Output Device",
                                subtitle: "Select your speakers/headphones",
                                selection: Binding(
                                    get: { deviceManager.selectedOutputDeviceID },
                                    set: { if let deviceID = $0 { deviceManager.setOutputDevice(deviceID) } }
                                ),
                                devices: deviceManager.availableOutputDevices
                            )
                        }
                    }
                    
                    // Transmission Mode Section
                    settingsSection("Transmission", icon: "wave.3.right") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(AudioSettings.TransmissionMode.allCases, id: \.self) { mode in
                                transmissionModeRow(mode: mode)
                            }
                            
                            if settings.transmissionMode == .pushToTalk {
                                pttSettings.padding(.top, 8)
                            } else if settings.transmissionMode == .voiceActivation {
                                vadSettings.padding(.top, 8)
                            }
                        }
                    }
                    
                    // Audio Quality Section
                    settingsSection("Audio Quality", icon: "waveform") {
                        VStack(alignment: .leading, spacing: 16) {
                            Toggle(isOn: $noiseSuppressionEnabled) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("Noise Suppression (RNNoise)")
                                        .font(.system(size: 14, weight: .medium))
                                    Text("Neural network-based background noise removal")
                                        .font(.system(size: 12))
                                        .foregroundColor(.secondary)
                                }
                            }
                            .toggleStyle(.switch)
                            .onChange(of: noiseSuppressionEnabled) { _, newValue in
                                if newValue && webrtcNsEnabled {
                                    webrtcNsEnabled = false
                                }
                                NotificationCenter.default.post(
                                    name: .audioSettingsChanged,
                                    object: ["noiseSuppression": newValue, "webrtcNsEnabled": webrtcNsEnabled]
                                )
                            }
                            
                            Toggle(isOn: $webrtcNsEnabled) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("WebRTC Noise Suppression")
                                        .font(.system(size: 14, weight: .medium))
                                    Text("Standard WebRTC NS (Lighter than RNNoise)")
                                        .font(.system(size: 12))
                                        .foregroundColor(.secondary)
                                }
                            }
                            .toggleStyle(.switch)
                            .onChange(of: webrtcNsEnabled) { _, newValue in
                                if newValue && noiseSuppressionEnabled {
                                    noiseSuppressionEnabled = false
                                }
                                NotificationCenter.default.post(
                                    name: .audioSettingsChanged,
                                    object: ["webrtcNsEnabled": newValue, "noiseSuppression": noiseSuppressionEnabled]
                                )
                            }
                            
                            Divider().opacity(0.2)
                            
                            Toggle(isOn: $aecEnabled) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("Echo Cancellation (AEC)")
                                        .font(.system(size: 14, weight: .medium))
                                    Text("Removes echo from speakers/feedback")
                                        .font(.system(size: 12))
                                        .foregroundColor(.secondary)
                                }
                            }
                            .toggleStyle(.switch)
                            .onChange(of: aecEnabled) { _, newValue in
                                NotificationCenter.default.post(
                                    name: .audioSettingsChanged,
                                    object: ["aecEnabled": newValue]
                                )
                            }
                            
                            Toggle(isOn: $webrtcAgcEnabled) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("Auto Gain Control (AGC)")
                                        .font(.system(size: 14, weight: .medium))
                                    Text("Normalize microphone volume automatically")
                                        .font(.system(size: 12))
                                        .foregroundColor(.secondary)
                                }
                            }
                            .toggleStyle(.switch)
                            .onChange(of: webrtcAgcEnabled) { _, newValue in
                                NotificationCenter.default.post(
                                    name: .audioSettingsChanged,
                                    object: ["webrtcAgcEnabled": newValue]
                                )
                            }
                            
                            Divider().opacity(0.2)
                            
                            VStack(alignment: .leading, spacing: 8) {
                                HStack {
                                    Text("JITTER BUFFER")
                                        .font(.system(size: 10, weight: .bold))
                                        .foregroundColor(.secondary)
                                    Spacer()
                                    Text("\(jitterBufferMs)ms")
                                        .font(.system(size: 11, weight: .bold))
                                        .foregroundColor(AuraTheme.Colors.primary)
                                }
                                
                                Picker("", selection: $jitterBufferMs) {
                                    Text("0ms (Instant)").tag(0)
                                    Text("10ms (Minimal)").tag(10)
                                    Text("20ms (Ultra Low)").tag(20)
                                    Text("40ms (Low)").tag(40)
                                    Text("60ms (Balanced)").tag(60)
                                    Text("80ms (Stable)").tag(80)
                                    Text("100ms (Maximum)").tag(100)
                                }
                                .labelsHidden()
                                .onChange(of: jitterBufferMs) { _, newValue in
                                    NotificationCenter.default.post(
                                        name: .audioSettingsChanged,
                                        object: ["jitterBuffer": newValue]
                                    )
                                }
                                
                                if jitterBufferMs == 0 {
                                    HStack(spacing: 6) {
                                        Image(systemName: "exclamationmark.triangle.fill")
                                            .foregroundColor(.orange)
                                            .font(.system(size: 10))
                                        Text("0ms is only for LAN/localhost")
                                            .font(.system(size: 11))
                                            .foregroundColor(.secondary)
                                    }
                                    .padding(.top, 4)
                                } else {
                                    Text("Lower = less delay, higher = more stable")
                                        .font(.system(size: 11))
                                        .foregroundColor(.secondary)
                                        .padding(.top, 4)
                                }
                            }
                        }
                    }
                    
                    // TTS Settings Section
                    settingsSection("Text-to-Speech", icon: "bubble.left.and.exclamationmark.bubble.right.fill") {
                        ttsSettings
                    }
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }
            
            HStack {
                Spacer()
                Button("Done") {
                    settings.saveSettings()
                    dismiss()
                }
                .buttonStyle(.plain)
                .padding(.vertical, 10)
                .padding(.horizontal, 28)
                .background(AuraTheme.Gradients.lushIndigo)
                .cornerRadius(12)
                .foregroundColor(.white)
                .font(.system(size: 14, weight: .bold))
                .modifier(AuraTheme.Shadows.soft())
                .auraFluidHover()
            }
            .padding(24)
        }
        .frame(width: 550, height: 750)
        .auraGlass(material: .hudWindow)
    }
    
    // MARK: - Components
    
    private func settingsSection<Content: View>(_ title: String, icon: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 8) {
                Image(systemName: icon)
                    .foregroundColor(AuraTheme.Colors.primary)
                    .font(.system(size: 12, weight: .bold))
                Text(title.uppercased())
                    .font(.system(size: 11, weight: .bold))
                    .foregroundColor(.secondary)
                    .kerning(1)
            }
            
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(20)
        .background(Color.white.opacity(0.03))
        .cornerRadius(AuraTheme.Layout.glassCornerRadius)
        .overlay(
            RoundedRectangle(cornerRadius: AuraTheme.Layout.glassCornerRadius)
                .strokeBorder(Color.white.opacity(0.08), lineWidth: 0.5)
        )
    }
    
    private func devicePicker(title: String, subtitle: String, selection: Binding<AudioDeviceID?>, devices: [AudioDeviceManager.AudioDevice]) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.system(size: 14, weight: .medium))
            Text(subtitle)
                .font(.system(size: 12))
                .foregroundColor(.secondary)
            
            Picker("", selection: selection) {
                Text("System Default").tag(nil as AudioDeviceID?)
                ForEach(devices) { device in
                    Text(device.name).tag(device.id as AudioDeviceID?)
                }
            }
            .labelsHidden()
            .controlSize(.large)
        }
    }
    
    private func themeRow(theme: AuraThemeType) -> some View {
        let isSelected = appSettings.theme == theme
        return HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(theme.displayName)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundColor(isSelected ? .primary : .secondary)
            }
            Spacer()
            if isSelected {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(AuraTheme.Colors.primary)
                    .font(.title3)
            } else {
                Circle()
                    .stroke(Color.secondary.opacity(0.3), lineWidth: 2)
                    .frame(width: 18, height: 18)
            }
        }
        .padding(10)
        .background(isSelected ? AuraTheme.Colors.primary.opacity(0.1) : Color.clear)
        .cornerRadius(8)
        .contentShape(Rectangle())
        .onTapGesture {
            withAnimation(.spring()) {
                appSettings.theme = theme
                appSettings.saveSettings()
            }
        }
        .auraFluidHover()
    }
    
    private func transmissionModeRow(mode: AudioSettings.TransmissionMode) -> some View {
        let isSelected = settings.transmissionMode == mode
        return HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(mode.displayName)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundColor(isSelected ? .primary : .secondary)
                Text(transmissionModeDescription(mode))
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
            }
            Spacer()
            if isSelected {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(AuraTheme.Colors.primary)
                    .font(.title2)
            } else {
                Circle()
                    .stroke(Color.secondary.opacity(0.3), lineWidth: 2)
                    .frame(width: 20, height: 20)
            }
        }
        .padding(10)
        .background(isSelected ? AuraTheme.Colors.primary.opacity(0.1) : Color.clear)
        .cornerRadius(8)
        .contentShape(Rectangle())
        .onTapGesture {
            withAnimation(.spring()) {
                settings.transmissionMode = mode
            }
        }
        .auraFluidHover()
    }
    
    private var pttSettings: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("PTT HOTKEY")
                .font(.system(size: 10, weight: .bold))
                .foregroundColor(.secondary)
            
            HotkeyRecorderButton(hotkey: $settings.pttHotkey)
            
            if !hotkeyManager.hasAccessibilityPermission {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundColor(.orange)
                    Text("Accessibility Permission Required")
                        .font(.system(size: 11, weight: .semibold))
                    Spacer()
                    Button("Grant") {
                        hotkeyManager.requestAccessibilityPermission()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                .padding(8)
                .background(Color.orange.opacity(0.1))
                .cornerRadius(6)
            }
        }
        .padding(12)
        .background(Color.black.opacity(0.1))
        .cornerRadius(10)
    }
    
    private var vadSettings: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("VAD SENSITIVITY")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundColor(.secondary)
                Spacer()
                Text("\(Int(settings.vadSensitivity * 100))%")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundColor(AuraTheme.Colors.primary)
            }
            
            Slider(value: $settings.vadSensitivity, in: 0.0...1.0)
                .accentColor(AuraTheme.Colors.primary)
        }
        .padding(12)
        .background(Color.black.opacity(0.1))
        .cornerRadius(10)
    }
    
    private var ttsSettings: some View {
        VStack(alignment: .leading, spacing: 16) {
            Toggle(isOn: $ttsManager.settings.enabled) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Master Switch")
                        .font(.system(size: 14, weight: .medium))
                    Text("Hear messages spoken aloud")
                        .font(.system(size: 12))
                        .foregroundColor(.secondary)
                }
            }
            .toggleStyle(.switch)
            
            if ttsManager.settings.enabled {
                VStack(spacing: 12) {
                    Toggle("Speak Chat Messages", isOn: $ttsManager.settings.speakChat)
                    Toggle("Speak Join/Leave Events", isOn: $ttsManager.settings.speakJoinLeave)
                }
                .font(.system(size: 13))
                .padding(.leading, 4)
                
                Divider().opacity(0.2)
                
                VStack(spacing: 16) {
                    sliderRow(title: "Speech Rate", icon: "hare", value: $ttsManager.settings.rate)
                    sliderRow(title: "Voice Volume", icon: "speaker.wave.2", value: $ttsManager.settings.volume)
                }
            }
        }
    }
    
    private func sliderRow(title: String, icon: String, value: Binding<Float>) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Image(systemName: icon)
                    .font(.system(size: 10))
                Text(title.uppercased())
                    .font(.system(size: 10, weight: .bold))
                Spacer()
                Text("\(Int(value.wrappedValue * 100))%")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundColor(AuraTheme.Colors.primary)
            }
            .foregroundColor(.secondary)
            
            Slider(value: value, in: 0.0...1.0)
                .accentColor(AuraTheme.Colors.primary)
                .controlSize(.small)
        }
    }
    
    // MARK: - Helpers
    
    private func transmissionModeDescription(_ mode: AudioSettings.TransmissionMode) -> String {
        switch mode {
        case .pushToTalk: return "Hold a hotkey to transmit audio"
        case .alwaysOn: return "Continuous audio transmission"
        case .voiceActivation: return "Transmit when speech is detected"
        }
    }
}
