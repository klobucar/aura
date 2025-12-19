//
//  ContentView.swift
//  Aura
//

import SwiftUI
import Combine

struct ContentView: View {
    @State private var isConnected = false
    @State private var client: QuicNetworkClient?
    @State private var identity: UserIdentity?
    @StateObject private var audioCapture = AudioCapture()
    @StateObject private var tts = TtsManager.shared
    @StateObject private var audioSettings = AudioSettings()
    @StateObject private var hotkeyManager = HotkeyManager.shared
    @State private var isMicEnabled = false
    @State private var isDeafened = false
    @StateObject private var appSettings = AppSettings.shared
    @State private var showingSettings = false
    @State private var pttCancellable: AnyCancellable?
    
    // Chat state
    @State private var chatMessages: [ChatMessage] = []
    @State private var messageText = ""
    @State private var showingChat = true
    @State private var replyingTo: ChatMessage?
    
    // Channel definitions
    private let channels: [Channel] = [
        Channel(id: 1, name: "General", icon: "speaker.wave.2"),
        Channel(id: 2, name: "Gaming", icon: "gamecontroller"),
        Channel(id: 3, name: "Music", icon: "music.note"),
        Channel(id: 4, name: "AFK", icon: "moon.zzz")
    ]
    
    var body: some View {
        Group {
            if isConnected, let client = client, let identity = identity {
                connectedView(client: client, identity: identity)
            } else {
                loginView
            }
        }
    }
    
    // MARK: - Login View (Centered)
    
    @ViewBuilder
    private var loginView: some View {
        ZStack {
            // High-fidelity background
            AuraTheme.Colors.background
                .ignoresSafeArea()
            
            // Animated background elements (aura-style)
            Circle()
                .fill(AuraTheme.Gradients.lushIndigo.opacity(0.15))
                .frame(width: 600, height: 600)
                .blur(radius: 80)
                .offset(x: -200, y: -200)
            
            Circle()
                .fill(AuraTheme.Gradients.lushMint.opacity(0.1))
                .frame(width: 400, height: 400)
                .blur(radius: 60)
                .offset(x: 200, y: 200)
            
            // Centered login card
            LoginView { connectedClient, connectedIdentity in
                withAnimation(.spring(response: 0.5, dampingFraction: 0.8)) {
                    self.client = connectedClient
                    self.identity = connectedIdentity
                    self.isConnected = true
                }
            }
            .frame(width: 460, height: 680)
            .modifier(AuraTheme.Shadows.deep())
        }
        .frame(minWidth: 600, minHeight: 750)
    }
    
    // MARK: - Connected View
    
    @ViewBuilder
    private func connectedView(client: QuicNetworkClient, identity: UserIdentity) -> some View {
        NavigationSplitView {
            // Sidebar
            VStack(alignment: .leading, spacing: 0) {
                // User info header
                userHeader(identity: identity, client: client)
                
                Divider()
                
                // Channels list
                channelList(client: client)
            }
            .frame(minWidth: 220, maxWidth: 280)
            .toolbar {
                ToolbarItem(placement: .navigation) {
                    HStack {
                        Image(systemName: "wave.3.right.circle.fill")
                            .foregroundStyle(.linearGradient(colors: [.blue, .purple], startPoint: .topLeading, endPoint: .bottomTrailing))
                        Text("Aura")
                            .font(.headline)
                    }
                }
            }
        } detail: {
            // Main content area
            ZStack(alignment: .bottom) {
                channelDetailView(client: client)
                
                // Background message handlers
                setupMessageHandlers(client: client)
            }
        }
        .navigationTitle("")
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(AuraTheme.Colors.background)
    }
    
    // MARK: - User Header
    
    @ViewBuilder
    private func userHeader(identity: UserIdentity, client: QuicNetworkClient) -> some View {
        HStack(spacing: 12) {
            // Avatar
            Circle()
                .fill(AuraTheme.Gradients.primary)
                .frame(width: 40, height: 40)
                .modifier(AuraTheme.Shadows.soft())
                .overlay(
                    Text(identity.displayName.prefix(1).uppercased())
                        .font(.headline)
                        .foregroundColor(.white)
                )
            
            VStack(alignment: .leading, spacing: 2) {
                Text(identity.displayName)
                    .font(.headline)
                Text(client.connectionStatus)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
            }
            
            Spacer()
            
            // Disconnect button
            Button(action: disconnect) {
                Image(systemName: "rectangle.portrait.and.arrow.right")
                    .foregroundColor(.secondary)
            }
            .buttonStyle(.plain)
            .help("Disconnect")
        }
        .padding(10)
        .auraGlass(cornerRadius: 12)
        .padding(.horizontal, 8)
        .padding(.top, 8)
    }
    
    // MARK: - Channel List
    
    @ViewBuilder
    private func channelList(client: QuicNetworkClient) -> some View {
        let currentId = client.currentChannelId ?? 1
        
        VStack(spacing: 0) {
            List {
                Section("Voice Channels") {
                    ForEach(channels) { channel in
                        VStack(alignment: .leading, spacing: 4) {
                            // Channel header
                            Button(action: {
                                switchChannel(to: channel.id, client: client)
                            }) {
                                HStack {
                                    Image(systemName: channel.icon)
                                        .foregroundColor(channel.id == currentId ? .blue : .secondary)
                                        .frame(width: 20)
                                    Text(channel.name)
                                        .foregroundColor(channel.id == currentId ? .primary : .secondary)
                                        .fontWeight(channel.id == currentId ? .semibold : .regular)
                                    
                                    Spacer()
                                    
                                    // User count
                                    if let users = client.usersByChannel[channel.id], !users.isEmpty {
                                        Text("\(users.count + (channel.id == currentId ? 1 : 0))")
                                            .font(.system(size: 10, weight: .bold))
                                            .padding(.horizontal, 6)
                                            .padding(.vertical, 2)
                                            .background(Capsule().fill(Color.white.opacity(0.1)))
                                    } else if channel.id == currentId {
                                        Text("1")
                                            .font(.system(size: 10, weight: .bold))
                                            .padding(.horizontal, 6)
                                            .padding(.vertical, 2)
                                            .background(Capsule().fill(Color.white.opacity(0.1)))
                                    }
                                    
                                    // Active Indicator
                                    if channel.id == currentId {
                                        Circle()
                                            .fill(AuraTheme.Colors.accent)
                                            .frame(width: 6, height: 6)
                                            .modifier(AuraTheme.Shadows.glow(color: AuraTheme.Colors.accent))
                                    }
                                }
                                .padding(.horizontal, 8)
                                .padding(.vertical, 6)
                                .background(channel.id == currentId ? AuraTheme.Colors.primary.opacity(0.15) : Color.clear)
                                .cornerRadius(8)
                                .auraFluidHover()
                            }
                            .buttonStyle(.plain)
                            
                            // Users in this channel
                            if let users = client.usersByChannel[channel.id], !users.isEmpty {
                                VStack(alignment: .leading, spacing: 2) {
                                    // Show current user if in this channel
                                    if channel.id == currentId {
                                        HStack(spacing: 6) {
                                            Circle()
                                                .fill(isMicEnabled ? Color.green : Color.secondary)
                                                .frame(width: 6, height: 6)
                                            Text("You")
                                                .font(.caption)
                                                .foregroundColor(.secondary)
                                        }
                                        .padding(.leading, 26)
                                    }
                                    
                                    // Show other users
                                    ForEach(users) { user in
                                        HStack(spacing: 6) {
                                            // Speaking indicator: microphone icon that's green+pulsing if speaking, grey if silent
                                            let isSpeaking = client.activeSpeakers.contains(user.id)
                                            Image(systemName: isSpeaking ? "mic.fill" : "mic.slash.fill")
                                                .font(.system(size: 10))
                                                .foregroundColor(isSpeaking ? .green : .secondary.opacity(0.5))
                                                .scaleEffect(isSpeaking ? 1.2 : 1.0)
                                                .animation(.easeInOut(duration: 0.5).repeatForever(autoreverses: true), value: isSpeaking)
                                            Text(user.displayName)
                                                .font(.caption)
                                                .foregroundColor(isSpeaking ? .primary : .secondary)
                                        }
                                        .padding(.leading, 26)
                                    }
                                }
                                .padding(.top, 2)
                            } else if channel.id == currentId {
                                // Just show "You" if alone in channel
                                HStack(spacing: 6) {
                                    Circle()
                                        .fill(isMicEnabled ? Color.green : Color.secondary)
                                        .frame(width: 6, height: 6)
                                    Text("You")
                                        .font(.caption)
                                        .foregroundColor(.secondary)
                                }
                                .padding(.leading, 26)
                                .padding(.top, 2)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                }
            }
            .listStyle(.sidebar)
            
            Divider()
            
            // Settings Button
            Button(action: { showingSettings = true }) {
                HStack {
                    Image(systemName: "gearshape.fill")
                        .font(.system(size: 14, weight: .semibold))
                    Text("Settings")
                        .font(.system(size: 13, weight: .medium))
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 10)
                .auraGlass(cornerRadius: 10)
                .auraFluidHover()
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.3))
        .sheet(isPresented: $showingSettings) {
            SettingsView(settings: audioSettings, ttsManager: tts)
        }
    }

    
    // MARK: - Channel Detail View
    
    @ViewBuilder
    private func channelDetailView(client: QuicNetworkClient) -> some View {
        VStack(spacing: 0) {
            channelHeader(client: client)
            
            Divider().opacity(0.1)
            
            HSplitView {
                voiceStatusPanel(client: client)
                
                if showingChat {
                    chatPanel(client: client)
                }
            }
        }
    }
    
    @ViewBuilder
    private func channelHeader(client: QuicNetworkClient) -> some View {
        let channel = currentChannel(for: client)
        let userCount = (client.usersByChannel[client.currentChannelId ?? 1]?.count ?? 0) + 1
        
        HStack {
            Image(systemName: channel?.icon ?? "speaker.wave.2")
                .font(.system(size: 18, weight: .bold))
                .foregroundColor(AuraTheme.Colors.primary)
                .modifier(AuraTheme.Shadows.glow(color: AuraTheme.Colors.primary))
            
            Text(channel?.name ?? "Channel")
                .font(.system(size: 18, weight: .bold))
            
            Spacer()
            
            // Toggle chat button
            Button(action: { 
                withAnimation(.spring(response: 0.3, dampingFraction: 0.7)) {
                    showingChat.toggle()
                }
            }) {
                Image(systemName: showingChat ? "bubble.left.fill" : "bubble.left")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundColor(showingChat ? AuraTheme.Colors.primary : .secondary)
            }
            .buttonStyle(.plain)
            .auraFluidHover()
            
            // User count badge
            Text("\(userCount)")
                .font(.system(size: 10, weight: .bold))
                .foregroundColor(.secondary)
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .auraGlass(cornerRadius: 10)
        }
        .padding(16)
        .background(VisualEffectBlur(auraMaterial: .header, blendingMode: .behindWindow))
    }
    
    @ViewBuilder
    private func voiceStatusPanel(client: QuicNetworkClient) -> some View {
        VStack(spacing: 24) {
            Spacer()
            
            ZStack {
                // Outer glow - smaller and more subtle
                Circle()
                    .fill(isMicEnabled ? AuraTheme.Colors.accent.opacity(0.12) : Color.white.opacity(0.03))
                    .frame(width: 120, height: 120)
                    .blur(radius: 15)
                
                // Refined glass ring
                Circle()
                    .stroke(
                        LinearGradient(
                            colors: [Color.white.opacity(0.2), Color.white.opacity(0.05)],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        ),
                        lineWidth: 1
                    )
                    .frame(width: 100, height: 100)
                
                if !isMicEnabled {
                    Circle()
                        .fill(Color.primary.opacity(0.05))
                        .frame(width: 80, height: 80)
                        .auraGlass(cornerRadius: 40, material: .ultraThin)
                } else {
                    Circle()
                        .fill(AuraTheme.Gradients.lushMint)
                        .frame(width: 80, height: 80)
                        .modifier(AuraTheme.Shadows.glow(color: AuraTheme.Colors.accent))
                }
                
                Image(systemName: isMicEnabled ? "mic.fill" : "mic.slash.fill")
                    .font(.system(size: 32, weight: .bold))
                    .foregroundColor(.white)
            }
            .animation(.spring(response: 0.4, dampingFraction: 0.7), value: isMicEnabled)
            
            VStack(spacing: 8) {
                Text(isDeafened ? "Deafened" : (isMicEnabled ? "Transmitting" : "Muted"))
                    .font(.system(size: 18, weight: .bold))
                    .foregroundColor(isDeafened ? .secondary : .primary)
                
                if isMicEnabled && !isDeafened {
                    HStack(spacing: 4) {
                        Circle().fill(Color.green).frame(width: 6, height: 6)
                        Text("\(audioCapture.packetsSent) packets sent")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundColor(.secondary)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(Capsule().fill(Color.white.opacity(0.05)))
                } else if isDeafened {
                    Text("You cannot hear or speak")
                        .font(.system(size: 10))
                        .foregroundColor(.secondary.opacity(0.7))
                } else {
                    Text("Your audio is currently private")
                        .font(.system(size: 10))
                        .foregroundColor(.secondary.opacity(0.7))
                }
            }
            
            // Lush Control Duo: Compact Icon Capsule
            HStack(spacing: 0) {
                // Mic Toggle
                Button(action: { toggleMic(client: client) }) {
                    Image(systemName: isMicEnabled ? "mic.fill" : "mic.slash.fill")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundColor(isMicEnabled ? .white : .secondary)
                        .frame(width: 44, height: 38)
                        .background(
                            isMicEnabled ? 
                            AnyShapeStyle(AuraTheme.Gradients.lushIndigo) : 
                            AnyShapeStyle(Color.clear)
                        )
                        .cornerRadius(8)
                }
                .buttonStyle(.plain)
                .help(isMicEnabled ? "Mute" : "Unmute")
                
                Divider()
                    .frame(height: 20)
                    .padding(.horizontal, 8)
                
                // Deafen Toggle
                Button(action: { toggleDeafen(client: client) }) {
                    Image(systemName: isDeafened ? "headphones.slash" : "headphones")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundColor(isDeafened ? .white : .secondary)
                        .frame(width: 44, height: 38)
                        .background(isDeafened ? Color.red.opacity(0.6) : Color.clear)
                        .cornerRadius(8)
                }
                .buttonStyle(.plain)
                .help(isDeafened ? "Undeafen" : "Deafen")
            }
            .padding(6)
            .auraGlass(cornerRadius: 12)
            .auraFluidHover()
            
            Spacer()
        }
        .frame(minWidth: 200)
    }
        @ViewBuilder
    private func chatPanel(client: QuicNetworkClient) -> some View {
        VStack(spacing: 0) {
            // Messages list
            ScrollViewReader { scrollProxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(chatMessages) { message in
                            if message.type == .info {
                                infoMessageRow(message.content)
                                    .id(message.id)
                            } else {
                                MessageBubble(message: message) { msg in
                                    replyingTo = msg
                                }
                                .id(message.id)
                            }
                        }
                    }
                    .padding()
                }
                .onChange(of: chatMessages.count) { _, _ in
                    if let lastMessage = chatMessages.last {
                        withAnimation {
                            scrollProxy.scrollTo(lastMessage.id, anchor: .bottom)
                        }
                    }
                }
            }
            
            divider()
            
            // Reply preview bar
            if let replying = replyingTo {
                replyPreview(replying)
            }
            
            messageInputArea(client: client)
        }
        .frame(minWidth: 250)
        .background(AuraTheme.Colors.background.opacity(0.5))
    }
    
    private func infoMessageRow(_ content: String) -> some View {
        HStack {
            VStack { divider() }
            Text(content)
                .font(.system(size: 10, weight: .bold))
                .foregroundColor(.secondary)
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .auraGlass(cornerRadius: 10)
            VStack { divider() }
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 16)
    }
    
    private func replyPreview(_ message: ChatMessage) -> some View {
        HStack {
            Rectangle()
                .fill(AuraTheme.Colors.primary)
                .frame(width: 3)
            
            VStack(alignment: .leading, spacing: 2) {
                Text("Replying to \(message.senderName)")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundColor(AuraTheme.Colors.primary)
                Text(message.content)
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)
                    .lineLimit(1)
            }
            
            Spacer()
            
            Button(action: { replyingTo = nil }) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundColor(.secondary)
            }
            .buttonStyle(.plain)
            .auraFluidHover()
        }
        .padding(.horizontal)
        .padding(.vertical, 10)
        .background(VisualEffectBlur(auraMaterial: .thin, blendingMode: .withinWindow))
    }
    
    private func messageInputArea(client: QuicNetworkClient) -> some View {
        HStack(spacing: 12) {
            TextField("Message...", text: $messageText)
                .textFieldStyle(.plain)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(Color.primary.opacity(0.05))
                .cornerRadius(10)
                .onSubmit {
                    sendMessage(client: client)
                }
            
            Button(action: { sendMessage(client: client) }) {
                Image(systemName: "paperplane.fill")
                    .font(.system(size: 14, weight: .bold))
                    .foregroundColor(.white)
                    .frame(width: 36, height: 36)
                    .background(
                        Group {
                            if messageText.isEmpty {
                                Color.secondary.opacity(0.3)
                            } else {
                                AuraTheme.Gradients.lushIndigo
                            }
                        }
                    )
                    .clipShape(Circle())
                    .modifier(AuraTheme.Shadows.soft())
            }
            .buttonStyle(.plain)
            .disabled(messageText.isEmpty)
            .auraFluidHover()
        }
        .padding(12)
        .auraGlass(cornerRadius: 20, material: .thin)
        .padding(16)
    }
    
    private func divider() -> some View {
        Divider().opacity(0.1)
    }
    
    // MARK: - Message Handling
    
    private func setupMessageHandlers(client: QuicNetworkClient) -> some View {
        Color.clear
            .frame(width: 0, height: 0)
            .onChange(of: client.receivedMessages) { oldValue, newValue in
                // Add new incoming messages to chat
                for msg in newValue where !oldValue.contains(msg) {
                    guard msg.channelId == client.currentChannelId else { continue }
                    
                    // Use message ID from packet (msg_{UUID})
                    let messageId = msg.id
                    
                    // Skip if we already have this message (optimistic update or duplicate)
                    if chatMessages.contains(where: { $0.id == messageId }) {
                        continue
                    }
                    
                    var chatMsg = ChatMessage(
                        id: messageId,
                        senderName: msg.senderName,
                        content: msg.content,
                        timestamp: msg.timestamp,
                        isOutgoing: msg.senderSessionId == client.sessionId // Mark as outgoing if it's from us
                    )
                    chatMsg.channelId = msg.channelId
                    
                    // Lookup reply context if this is a reply
                    if let replyId = msg.replyToId,
                       let originalMsg = chatMessages.first(where: { $0.id == replyId }) {
                        chatMsg.replyToId = replyId
                        chatMsg.replyToSender = originalMsg.senderName
                        chatMsg.replyToPreview = String(originalMsg.content.prefix(50))
                    }
                    
                    chatMessages.append(chatMsg)
                    
                    // Speak the message
                    tts.speakMessage(sender: msg.senderName, content: msg.content)
                }
            }
            .onChange(of: client.systemEvents) { oldValue, newValue in
                // Add new system events to chat (like user disconnects)
                for event in newValue where !oldValue.contains(event) {
                    // Only show events for current channel or global (0)
                    guard event.channelId == client.currentChannelId || event.channelId == 0 else { continue }
                    
                    // Avoid duplicates
                    let messageId = "sys_\(event.id.uuidString)"
                    if chatMessages.contains(where: { $0.id == messageId }) { continue }
                    
                    var message = ChatMessage(
                        id: messageId,
                        senderName: "System",
                        content: event.content,
                        timestamp: event.timestamp,
                        isOutgoing: false
                    )
                    message.type = .info
                    message.channelId = event.channelId
                    
                    chatMessages.append(message)
                    
                    // Speak the system event
                    if event.content.contains("joined") {
                        // Extract name from "Name joined the channel"
                        let name = event.content.replacingOccurrences(of: " joined the channel", with: "")
                        tts.speakJoin(name: name)
                    } else if event.content.contains("left") {
                        let name = event.content.replacingOccurrences(of: " left the channel", with: "")
                        tts.speakLeave(name: name)
                    }
                }
            }
    }
    
    // MARK: - Helpers
    
    private func currentChannel(for client: QuicNetworkClient) -> Channel? {
        let channelId = client.currentChannelId ?? 1
        return channels.first { $0.id == channelId }
    }
    
    private func switchChannel(to channelId: UInt32, client: QuicNetworkClient) {
        guard channelId != client.currentChannelId else { return }
        
        // Capture old/new names for divider
        let oldChannelId = client.currentChannelId ?? 1
        let oldChannelName = channels.first(where: { $0.id == oldChannelId })?.name ?? "Unknown"
        let newChannelName = channels.first(where: { $0.id == channelId })?.name ?? "Unknown"
        
        // Add divider if we have chat history
        if !chatMessages.isEmpty {
            let truncatedOld = String(oldChannelName.prefix(25))
            let truncatedNew = String(newChannelName.prefix(25))
            let text = "Left \(truncatedOld) joined \(truncatedNew)"
            
            var divider = ChatMessage(
                id: "div_\(UUID().uuidString)",
                senderName: "System",
                content: text,
                timestamp: Date(),
                isOutgoing: false
            )
            divider.type = .info
            chatMessages.append(divider)
        }
        
        Task {
            do {
                try await client.joinChannel(channelId)
                print("[ContentView] Switched to channel \(channelId)")
            } catch {
                print("[ContentView] Failed to switch channel: \(error)")
            }
        }
    }
    
    private func toggleMic(client: QuicNetworkClient) {
        // Cannot unmute if deafened
        if isDeafened && !isMicEnabled {
            return
        }
        
        switch audioSettings.transmissionMode {
        case .pushToTalk:
            // PTT is handled by hotkey subscription, this just toggles the subscription
            if pttCancellable != nil {
                // Disable PTT
                pttCancellable?.cancel()
                pttCancellable = nil
                audioCapture.stop()
                isMicEnabled = false
            } else {
                // Enable PTT - register hotkey and subscribe
                if let hotkey = audioSettings.pttHotkey {
                    hotkeyManager.registerHotkey(hotkey)
                }
                pttCancellable = hotkeyManager.$isPTTActive
                    .receive(on: DispatchQueue.main)
                    .sink { [weak audioCapture] isActive in
                        if isActive {
                            audioCapture?.start { pcmData in
                                Task {
                                    try? await client.sendAudioDatagram(pcmData)
                                }
                            }
                        } else {
                            audioCapture?.stop()
                        }
                    }
                isMicEnabled = true
            }
            
        case .alwaysOn:
            // Always transmit when enabled
            if isMicEnabled {
                audioCapture.stop()
                isMicEnabled = false
            } else {
                audioCapture.start { pcmData in
                    Task {
                        try? await client.sendAudioDatagram(pcmData)
                    }
                }
                isMicEnabled = true
            }
            
        case .voiceActivation:
            // VAD mode - audio capture with voice detection
            // For now, this works like always-on; full VAD integration would use Rust VAD
            if isMicEnabled {
                audioCapture.stop()
                isMicEnabled = false
            } else {
                audioCapture.start { pcmData in
                    Task {
                        try? await client.sendAudioDatagram(pcmData)
                    }
                }
                isMicEnabled = true
            }
        }
    }
    
    private func toggleDeafen(client: QuicNetworkClient) {
        withAnimation(.spring(response: 0.3, dampingFraction: 0.7)) {
            isDeafened.toggle()
            
            if isDeafened {
                // Auto-mute on deafen
                if isMicEnabled {
                    toggleMic(client: client)
                }
                // TODO: Actually mute the output (AudioSettings/AudioPipeline)
            }
        }
    }
    
    private func disconnect() {
        audioCapture.stop()
        client?.disconnect()
        client = nil
        identity = nil
        isConnected = false
        isMicEnabled = false
        chatMessages = []
        messageText = ""
    }
    
    private func sendMessage(client: QuicNetworkClient) {
        guard !messageText.isEmpty else { return }
        
        let content = messageText
        let replying = replyingTo
        messageText = "" // Clear immediately for UX
        replyingTo = nil // Clear reply state
        let timestamp = Date()
        let sessionId = client.sessionId ?? 0
        let channelId = client.currentChannelId ?? 0
        
        // Use UUID message ID
        let messageId = "msg_\(UUID().uuidString)"
        
        // Add to local messages (optimistic update)
        var message = ChatMessage(
            id: messageId,
            senderName: identity?.displayName ?? "You",
            content: content,
            timestamp: timestamp,
            isOutgoing: true
        )
        message.channelId = channelId
        
        // Add reply context if we're replying
        if let replying = replying {
            message.replyToId = replying.id
            message.replyToSender = replying.senderName
            message.replyToPreview = String(replying.content.prefix(50))
        }
        
        chatMessages.append(message)
        
        // Send to server with reply info and message ID
        Task {
            do {
                try await client.sendTextMessage(content, messageId: messageId, replyToId: replying?.id)
            } catch {
                print("[ContentView] Failed to send message: \(error)")
            }
        }
    }
}

// MARK: - Shadow Provider Helper

struct ShadowProvider: ViewModifier {
    let isMicEnabled: Bool
    
    func body(content: Content) -> some View {
        if isMicEnabled {
            content.modifier(AuraTheme.Shadows.glow(color: AuraTheme.Colors.accent))
        } else {
            content.modifier(AuraTheme.Shadows.deep())
        }
    }
}

// MARK: - Channel Model

struct Channel: Identifiable, Hashable {
    let id: UInt32
    let name: String
    let icon: String
}

// MARK: - Chat Message Model

struct ChatMessage: Identifiable, Equatable {
    let id: String
    let senderName: String
    let content: String
    let timestamp: Date
    let isOutgoing: Bool
    
    // Context
    var channelId: UInt32 = 0
    var type: MessageType = .regular
    
    // Reply-to threading
    var replyToId: String?
    var replyToSender: String?
    var replyToPreview: String?
    
    var formattedTime: String {
        let formatter = DateFormatter()
        formatter.timeStyle = .short
        return formatter.string(from: timestamp)
    }
}

enum MessageType: Equatable, Codable {
    case regular
    case args(String) // For future extensibility if needed, but for now simple cases
    case info // Divider/System message
}

// MARK: - Message Bubble View

struct MessageBubble: View {
    let message: ChatMessage
    var onReply: ((ChatMessage) -> Void)?
    
    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            if message.isOutgoing { Spacer(minLength: 80) }
            
            VStack(alignment: message.isOutgoing ? .trailing : .leading, spacing: 2) {
                // Sender name (only for incoming)
                if !message.isOutgoing {
                    Text(message.senderName)
                        .font(.caption)
                        .fontWeight(.medium)
                        .foregroundColor(.secondary)
                        .padding(.leading, 12)
                }
                
                // Message bubble with optional reply context
                VStack(alignment: .leading, spacing: 4) {
                    // Reply context (if this is a reply)
                    if let replyPreview = message.replyToPreview {
                        HStack(spacing: 4) {
                            Rectangle()
                                .fill(message.isOutgoing ? .white.opacity(0.6) : .blue.opacity(0.6))
                                .frame(width: 3)
                            
                            VStack(alignment: .leading, spacing: 1) {
                                Text(message.replyToSender ?? "")
                                    .font(.system(size: 10, weight: .bold))
                                    .foregroundColor(message.isOutgoing ? .white.opacity(0.8) : .blue)
                                Text(replyPreview)
                                    .font(.system(size: 10))
                                    .foregroundColor(message.isOutgoing ? .white.opacity(0.6) : .secondary)
                                    .lineLimit(1)
                            }
                        }
                        .padding(.bottom, 2)
                    }
                    
                    // Message content with markdown rendering
                    MarkdownText(message.content, foregroundColor: message.isOutgoing ? .white : .primary)
                        .font(.system(size: 14, weight: .regular))
                }
                .auraMessageBubble(isOutgoing: message.isOutgoing)
                .transition(.asymmetric(
                    insertion: .opacity.combined(with: .move(edge: .bottom).combined(with: .scale(scale: 0.95))),
                    removal: .opacity
                ))
                .contextMenu {
                    Button(action: { onReply?(message) }) {
                        Label("Reply", systemImage: "arrowshape.turn.up.left")
                    }
                    Button(action: {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(message.content, forType: .string)
                    }) {
                        Label("Copy", systemImage: "doc.on.doc")
                    }
                }
                
                // Timestamp
                Text(message.formattedTime)
                    .font(.caption2)
                    .foregroundColor(.secondary)
                    .padding(.horizontal, 12)
            }
            
            if !message.isOutgoing { Spacer(minLength: 80) }
        }
        .padding(.horizontal, 4)
    }
}

// MARK: - Markdown Text View

struct MarkdownText: View {
    let text: String
    let foregroundColor: Color
    
    init(_ text: String, foregroundColor: Color = .primary) {
        self.text = text
        self.foregroundColor = foregroundColor
    }
    
    var body: some View {
        Text(attributedString)
            .textSelection(.enabled)
    }
    
    private var attributedString: AttributedString {
        // Try to parse as markdown (handles **bold**, *italic*, `code`, and links)
        if let parsed = try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)) {
            var result = parsed
            // Apply foreground color to all runs
            for run in result.runs {
                result[run.range].foregroundColor = foregroundColor
            }
            return result
        }
        
        // Fallback to plain text
        var result = AttributedString(text)
        result.foregroundColor = foregroundColor
        return result
    }
}

#Preview {
    ContentView()
}
