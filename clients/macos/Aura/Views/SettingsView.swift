import AuraCore
import SwiftUI
import AVFoundation
import CoreAudio

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var settings: AudioSettings
    @ObservedObject var ttsManager: TtsManager
    /// The login identity, for Settings → Identity.
    var identity: UserIdentity? = nil
    /// The server this window is connected to; offers "Leave server…" when set.
    var serverHost: String? = nil
    /// Delete the account on `serverHost` (after the confirmation here).
    var onLeaveServer: (() -> Void)? = nil
    /// Whether the client is currently connected to a voice channel.
    var isInVoiceCall: Bool = false
    @StateObject private var appSettings = AppSettings.shared
    @StateObject private var hotkeyManager = HotkeyManager.shared
    @ObservedObject private var deviceManager = AudioDeviceManager.shared
    @StateObject private var loopback = MicLoopbackController()
    
    // Audio Quality Settings
    @AppStorage("noiseSuppressionEnabled") private var noiseSuppressionEnabled = true
    @AppStorage("aecEnabled") private var aecEnabled = true
    @AppStorage("webrtcNsEnabled") private var webrtcNsEnabled = false
    @AppStorage("webrtcAgcEnabled") private var webrtcAgcEnabled = true
    @AppStorage("jitterBufferMs") private var jitterBufferMs = 20
    /// Same key and notification the network client already reads at connect
    /// (`QuicNetworkClient.applyAudioSettings`).
    @AppStorage("masterVolume") private var masterVolume = 1.0

    // Notifications — keys shared with the desktop client.
    @AppStorage(NotificationPrefs.levelKey) private var notifyLevel = NotificationLevel.mentions
    @AppStorage(NotificationPrefs.keywordsKey) private var notifyKeywords = ""
    @AppStorage(NotificationPrefs.showTextKey) private var notifyShowText = true
    @AppStorage(NotificationPrefs.badgeKey) private var notifyBadge = true
    @AppStorage(NotificationPrefs.joinLeaveKey) private var notifyJoinLeave = false

    // Identity
    /// Seconds since 1970; 0 until the first backup.
    @AppStorage(IdentityBackup.backedUpAtKey) private var backedUpAt: Double = 0
    @StateObject private var profileManager = ProfileManager()
    @State private var pendingRestore: PendingBackup?
    @State private var restoreError: String?
    @State private var confirmingLeave = false

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
                                set: { deviceManager.setInputDevice($0) }
                            ),
                            devices: deviceManager.availableInputDevices
                        )
                        
                        devicePicker(
                            title: "Output Device",
                            subtitle: "Select your speakers/headphones",
                            selection: Binding(
                                get: { deviceManager.selectedOutputDeviceID },
                                set: { deviceManager.setOutputDevice($0) }
                            ),
                            devices: deviceManager.availableOutputDevices
                        )

                        Divider().opacity(0.1)

                        VStack(alignment: .leading, spacing: 8) {
                            Toggle(isOn: Binding(
                                get: { loopback.isTesting },
                                set: { enabled in
                                    loopback.setTesting(
                                        enabled,
                                        outputDeviceID: deviceManager.selectedOutputDeviceID,
                                        volume: Float(masterVolume)
                                    )
                                }
                            )) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("Test Mic (Loopback)")
                                        .font(.system(size: 14, weight: .medium))
                                    Text(isInVoiceCall ? "Unavailable while connected to a voice channel" : "Hear your microphone locally before joining a channel")
                                        .font(.system(size: 12))
                                        .foregroundStyle(.secondary)
                                }
                            }
                            .toggleStyle(.switch)
                            .disabled(isInVoiceCall)
                            .onChange(of: isInVoiceCall) { _, inCall in
                                if inCall { loopback.stop() }
                            }

                            if loopback.isTesting {
                                MicLoopbackMeter(db: loopback.inputLevelDb)
                            }
                        }
                    }
                    .auraGlassSection(title: "Audio Devices", icon: "hifispeaker.2.fill")

                    // Output Volume Section — master playback gain. Per-person
                    // volume lives on each roster member's profile card.
                    outputVolume
                        .auraGlassSection(title: "Output", icon: "speaker.wave.3.fill")

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
                    .onChange(of: settings.transmissionMode) { _, newMode in
                        settings.saveSettings()
                        NotificationCenter.default.post(
                            name: .audioSettingsChanged,
                            object: [
                                "vadEnabled": newMode == .voiceActivation,
                                "vadThresholdDb": settings.vadThresholdDb,
                            ]
                        )
                    }
                    .onChange(of: settings.vadSensitivity) { _, _ in
                        settings.saveSettings()
                        NotificationCenter.default.post(
                            name: .audioSettingsChanged,
                            object: [
                                "vadEnabled": settings.transmissionMode == .voiceActivation,
                                "vadThresholdDb": settings.vadThresholdDb,
                            ]
                        )
                    }
                    
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

                        Divider().opacity(0.1)

                        VStack(alignment: .leading, spacing: 8) {
                            HStack {
                                Text("OPUS BITRATE")
                                    .font(.system(size: 10, weight: .bold))
                                    .foregroundStyle(.secondary)
                                Spacer()
                                Text("\(appSettings.opusBitrateKbps) kbps")
                                    .font(.system(size: 11, weight: .bold))
                                    .foregroundStyle(AuraTheme.Colors.primary)
                            }

                            Picker("", selection: $appSettings.opusBitrateKbps) {
                                ForEach(AppSettings.supportedBitratesKbps, id: \.self) { kbps in
                                    Text("\(kbps) kbps").tag(kbps)
                                }
                            }
                            .labelsHidden()
                            .onChange(of: appSettings.opusBitrateKbps) { _, newValue in
                                appSettings.saveSettings()
                                NotificationCenter.default.post(
                                    name: .audioSettingsChanged,
                                    object: ["opusBitrateKbps": newValue]
                                )
                            }

                            Text("Higher bitrate improves voice fidelity at the cost of bandwidth")
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        }

                        Divider().opacity(0.1)

                        Toggle(isOn: $appSettings.voiceSoundCuesEnabled) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Voice Sound Cues")
                                    .font(.system(size: 14, weight: .medium))
                                Text("Play subtle sounds when muting, unmuting, joining, or leaving")
                                    .font(.system(size: 12))
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .toggleStyle(.switch)
                        .onChange(of: appSettings.voiceSoundCuesEnabled) { _, _ in
                            appSettings.saveSettings()
                        }
                    }
                    .auraGlassSection(title: "Audio Quality", icon: "waveform")
                    
                    // TTS Settings Section
                    ttsSettings
                        .auraGlassSection(title: "Text-to-Speech", icon: "bubble.left.and.exclamationmark.bubble.right.fill")

                    notificationSettings
                        .auraGlassSection(title: "Notifications", icon: "bell.badge.fill")

                    if let identity {
                        identitySettings(identity)
                            .auraGlassSection(title: "Identity", icon: "person.badge.key.fill")
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
        .onDisappear { loopback.stop() }
        .sheet(item: $pendingRestore) { backup in
            RestoreBackupSheet(
                profileManager: profileManager,
                backup: backup,
                loginIdentityKeyHex: identity?.publicKeyHex,
                loginIdentityName: identity?.displayName
            )
        }
        .alert(
            "Couldn't open the backup",
            isPresented: Binding(get: { restoreError != nil }, set: { if !$0 { restoreError = nil } }),
            presenting: restoreError
        ) { _ in
            Button("OK", role: .cancel) {}
        } message: { message in
            Text(message)
        }
        .leaveServerAlert(
            isPresented: $confirmingLeave,
            host: serverHost ?? "",
            name: identity?.displayName ?? ""
        ) {
            dismiss()
            onLeaveServer?()
        }
    }

    // MARK: - Notifications

    private var notificationSettings: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 8) {
                Text("Notify me about")
                    .font(.system(size: 14, weight: .medium))
                Picker("Notify me about", selection: $notifyLevel) {
                    ForEach(NotificationLevel.allCases) { level in
                        Text(level.title).tag(level)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Also notify on these words")
                    .font(.system(size: 14, weight: .medium))
                TextField("deploy, standup", text: $notifyKeywords)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Also notify on these words")
            }

            Divider().opacity(0.1)

            Toggle(isOn: $notifyShowText) {
                Text("Show message text in banners")
                    .font(.system(size: 14, weight: .medium))
            }
            .toggleStyle(.switch)

            Toggle(isOn: $notifyBadge) {
                Text("Count mentions on the app icon")
                    .font(.system(size: 14, weight: .medium))
            }
            .toggleStyle(.switch)
            .onChange(of: notifyBadge) { _, counts in
                if !counts { MessageNotifier.shared.clearBadge() }
            }

            Toggle(isOn: $notifyJoinLeave) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Join and leave banners")
                        .font(.system(size: 14, weight: .medium))
                    Text("Today macOS sends one for every disconnect")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }
            }
            .toggleStyle(.switch)
        }
    }

    // MARK: - Identity

    private func identitySettings(_ identity: UserIdentity) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            IdentitySummaryCard(
                name: identity.displayName,
                identityKey: identity.identityKey,
                lastBackup: backedUpAt > 0 ? Date(timeIntervalSince1970: backedUpAt) : nil
            )

            Divider().opacity(0.1)

            IdentityBackupForm(
                displayName: identity.displayName,
                secret: { identity.secretForBackup() },
                onSaved: { date in backedUpAt = date.timeIntervalSince1970 }
            )

            Button("Restore from backup…") {
                do {
                    pendingRestore = try pickIdentityBackup()
                } catch {
                    restoreError = "The file couldn't be read."
                }
            }
            .buttonStyle(.bordered)

            if serverHost != nil, onLeaveServer != nil {
                Divider().opacity(0.1)

                VStack(alignment: .leading, spacing: 6) {
                    Button("Leave server…", role: .destructive) { confirmingLeave = true }
                        .buttonStyle(.bordered)
                        .tint(AuraTheme.Colors.danger)
                    Text("Deletes your account on \(serverHost ?? "this server"). Your key stays on this device.")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    // MARK: - Components
    
    private var outputVolume: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("OUTPUT VOLUME")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .kerning(AuraTheme.Typography.monoLabelKerning)
                    .foregroundStyle(AuraTheme.Colors.textDim)
                Spacer()
                Text("\(Int((masterVolume * 100).rounded()))%")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t11))
                    .foregroundStyle(AuraTheme.Colors.accent)
            }

            HStack(spacing: 10) {
                Image(systemName: "speaker.fill")
                    .foregroundStyle(.secondary)
                Slider(value: $masterVolume, in: 0...1)
                    .tint(AuraTheme.Colors.accent)
                    .accessibilityLabel("Output volume")
                    .onChange(of: masterVolume) { _, newValue in
                        NotificationCenter.default.post(
                            name: .audioSettingsChanged,
                            object: ["masterVolume": Float(newValue)]
                        )
                    }
                Image(systemName: "speaker.wave.3.fill")
                    .foregroundStyle(.secondary)
            }
        }
    }

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
