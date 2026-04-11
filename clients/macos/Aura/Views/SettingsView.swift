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
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button(action: { dismiss() }) {
                    Image(systemName: "xmark")
                        .accessibilityLabel("Close")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundStyle(.secondary)
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
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(AuraThemeType.allCases, id: \.self) { theme in
                            themeRow(theme: theme)
                        }
                    }
                    .auraGlassSection(title: "Appearance", icon: "paintbrush.fill")

                    // Audio Devices Section
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
                    .auraGlassSection(title: "Audio Devices", icon: "hifispeaker.2.fill")
                    
                    // Transmission Mode Section
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
                    .auraGlassSection(title: "Transmission", icon: "wave.3.right")
                    
                    // Audio Quality Section
                    VStack(alignment: .leading, spacing: 16) {
                        Toggle(isOn: $noiseSuppressionEnabled) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Noise Suppression (RNNoise)")
                                    .font(.system(size: 14, weight: .medium))
                                Text("Neural network-based background noise removal")
                                    .font(.system(size: 12))
                                    .foregroundStyle(.secondary)
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
                                    .foregroundStyle(.secondary)
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
                        
                        Divider().opacity(0.1)
                        
                        Toggle(isOn: $aecEnabled) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Echo Cancellation (AEC)")
                                    .font(.system(size: 14, weight: .medium))
                                Text("Removes echo from speakers/feedback")
                                    .font(.system(size: 12))
                                    .foregroundStyle(.secondary)
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
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .toggleStyle(.switch)
                        .onChange(of: webrtcAgcEnabled) { _, newValue in
                            NotificationCenter.default.post(
                                name: .audioSettingsChanged,
                                object: ["webrtcAgcEnabled": newValue]
                            )
                        }
                        
                        Divider().opacity(0.1)
                        
                        VStack(alignment: .leading, spacing: 8) {
                            HStack {
                                Text("JITTER BUFFER")
                                    .font(.system(size: 10, weight: .bold))
                                    .foregroundStyle(.secondary)
                                Spacer()
                                Text("\(jitterBufferMs)ms")
                                    .font(.system(size: 11, weight: .bold))
                                    .foregroundStyle(AuraTheme.Colors.primary)
                            }
                            
                            Picker("", selection: $jitterBufferMs) {
                                Text("0ms").tag(0)
                                Text("10ms").tag(10)
                                Text("20ms").tag(20)
                                Text("40ms").tag(40)
                                Text("60ms").tag(60)
                                Text("80ms").tag(80)
                                Text("100ms").tag(100)
                            }
                            .labelsHidden()
                            .onChange(of: jitterBufferMs) { _, newValue in
                                NotificationCenter.default.post(
                                    name: .audioSettingsChanged,
                                    object: ["jitterBuffer": newValue]
                                )
                            }
                            
                            Text(jitterBufferMs == 0 ? "LAN only" : "Lower delay vs more stability")
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                                .padding(.top, 4)
                        }
                    }
                    .auraGlassSection(title: "Audio Quality", icon: "waveform")
                    
                    // TTS Settings Section
                    ttsSettings
                        .auraGlassSection(title: "Text-to-Speech", icon: "bubble.left.and.exclamationmark.bubble.right.fill")
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
                .clipShape(.rect(cornerRadius: 12))
                .foregroundStyle(.white)
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
        content()
            .auraGlassSection(title: title, icon: icon)
    }
    
    private func devicePicker(title: String, subtitle: String, selection: Binding<AudioDeviceID?>, devices: [AudioDeviceManager.AudioDevice]) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.system(size: 14, weight: .medium))
            Text(subtitle)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            
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
                    .foregroundStyle(isSelected ? .primary : .secondary)
            }
            Spacer()
            if isSelected {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(AuraTheme.Colors.primary)
                    .font(.title3)
            } else {
                Circle()
                    .stroke(Color.secondary.opacity(0.3), lineWidth: 2)
                    .frame(width: 18, height: 18)
            }
        }
        .padding(10)
        .background(isSelected ? AuraTheme.Colors.primary.opacity(0.1) : Color.clear)
        .clipShape(.rect(cornerRadius: 8))
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
                    .foregroundStyle(isSelected ? .primary : .secondary)
                Text(transmissionModeDescription(mode))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if isSelected {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(AuraTheme.Colors.primary)
                    .font(.title2)
            } else {
                Circle()
                    .stroke(Color.secondary.opacity(0.3), lineWidth: 2)
                    .frame(width: 20, height: 20)
            }
        }
        .padding(10)
        .background(isSelected ? AuraTheme.Colors.primary.opacity(0.1) : Color.clear)
        .clipShape(.rect(cornerRadius: 8))
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
                .foregroundStyle(.secondary)
            
            HotkeyRecorderButton(hotkey: $settings.pttHotkey)
            
            if !hotkeyManager.hasAccessibilityPermission {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
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
                .clipShape(.rect(cornerRadius: 6))
            }
        }
        .padding(12)
        .background(Color.black.opacity(0.1))
        .clipShape(.rect(cornerRadius: 10))
    }
    
    private var vadSettings: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("VAD SENSITIVITY")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(.secondary)
                Spacer()
                Text("\(Int(settings.vadSensitivity * 100))%")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundStyle(AuraTheme.Colors.primary)
            }
            
            Slider(value: $settings.vadSensitivity, in: 0.0...1.0)
                .accentColor(AuraTheme.Colors.primary)
        }
        .padding(12)
        .background(Color.black.opacity(0.1))
        .clipShape(.rect(cornerRadius: 10))
    }
    
    private var ttsSettings: some View {
        VStack(alignment: .leading, spacing: 16) {
            Toggle(isOn: $ttsManager.settings.enabled) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Master Switch")
                        .font(.system(size: 14, weight: .medium))
                    Text("Hear messages spoken aloud")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
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
                    .foregroundStyle(AuraTheme.Colors.primary)
            }
            .foregroundStyle(.secondary)
            
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
