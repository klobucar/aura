//
//  ContentView.swift
//  Aura
//

import SwiftUI
import Combine
import UniformTypeIdentifiers

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
    @State private var showingSettings = false
    @State private var showingProfileEditor = false
    @State private var showingChannelEditor = false
    @State private var editingChannel: ChannelModel?
    @State private var pttCancellable: AnyCancellable?
    
    // Chat state
    @State private var chatMessages: [ChatMessage] = []
    @State private var messageText = ""
    @State private var showingChat = true
    @State private var replyingTo: ChatMessage?
    
    // Management views
    @State private var showingServerManagement = false
    @State private var showingProfileManagement = false
    @StateObject private var serverManager = ServerManager()
    @StateObject private var profileManager = ProfileManager()
    
    
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
                // Branded Header
                HStack(spacing: 8) {
                    Image(systemName: "wave.3.right.circle.fill")
                        .font(.system(size: 18, weight: .bold))
                        .foregroundStyle(.linearGradient(colors: [.blue, .purple], startPoint: .topLeading, endPoint: .bottomTrailing))
                    
                    Text("Aura")
                        .font(.system(size: 18, weight: .bold))
                        .foregroundStyle(Color.primary.opacity(0.9))
                    
                    Spacer()
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 14)
                .padding(.top, 24) // Clear traffic lights
                
                // User info header
                userHeader(identity: identity, client: client)
                
                Divider()
                
                // Channels list
                channelList(client: client)
            }
            .frame(minWidth: 220, maxWidth: 280)
        } detail: {
            // Main content area
            ZStack(alignment: .bottom) {
                channelDetailView(client: client)
                
                // Background message handlers
                setupMessageHandlers(client: client)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(AuraTheme.Colors.background)
        .sheet(isPresented: $showingProfileEditor) {
            ProfileEditView(client: client)
        }
        .sheet(isPresented: $showingChannelEditor) {
            ChannelEditView(client: client, channel: editingChannel)
        }
        .sheet(isPresented: $showingServerManagement) {
            ServerListView()
        }
        .sheet(isPresented: $showingProfileManagement) {
            ProfileListView()
        }
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
                        .foregroundStyle(.white)
                )
            
            VStack(alignment: .leading, spacing: 2) {
                Text(identity.displayName)
                    .font(.headline)
                Text(client.connectionStatus)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            
            Spacer()
            
            // Edit Profile button
            Button(action: { showingProfileEditor = true }) {
                Image(systemName: "pencil.circle")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Edit Profile")
            
            // Disconnect button
            Button(action: disconnect) {
                Image(systemName: "rectangle.portrait.and.arrow.right")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Disconnect")
        }
        .padding(10)
        .auraGlass(cornerRadius: 12)
        .padding(.horizontal, 10)
        .padding(.top, 10)
    }
    
    // MARK: - Channel List
    
    @ViewBuilder
    private func channelList(client: QuicNetworkClient) -> some View {
        let currentId = client.currentChannelId ?? client.channels.first?.id ?? 0
        
        VStack(spacing: 0) {
            List {
                Section(header: HStack {
                    Text("Voice Channels")
                    Spacer()
                    if client.isAdmin {
                        Button(action: { 
                            editingChannel = nil
                            showingChannelEditor = true 
                        }) {
                            Image(systemName: "plus.circle.fill")
                                .foregroundStyle(Color.accentColor)
                        }
                        .buttonStyle(.plain)
                    }
                }) {
                    ForEach(client.channels) { channel in
                        channelRow(channel: channel, currentId: currentId, client: client)
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
        let currentId = client.currentChannelId ?? client.channels.first?.id ?? 0
        let userCount = (client.usersByChannel[currentId]?.count ?? 0) + 1
        
        HStack {
            if let emoji = channel?.iconEmoji {
                Text(emoji)
                    .font(.system(size: 18))
            } else {
                Image(systemName: channel?.iconPresetId ?? "speaker.wave.2")
                    .font(.system(size: 18, weight: .bold))
                    .foregroundStyle(AuraTheme.Colors.primary)
                    .modifier(AuraTheme.Shadows.glow(color: AuraTheme.Colors.primary))
            }
            
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
                    .foregroundStyle(showingChat ? AuraTheme.Colors.primary : Color.secondary)
            }
            .buttonStyle(.plain)
            .auraFluidHover()
            
            // User count badge
            Text("\(userCount)")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .auraGlass(cornerRadius: 10)
    }
    .padding(.horizontal, 16)
    .padding(.bottom, 16)
    .padding(.top, 32) // Clear traffic lights area in detail view
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
                    .foregroundStyle(.white)
            }
            .animation(.spring(response: 0.4, dampingFraction: 0.7), value: isMicEnabled)
            
            VStack(spacing: 8) {
                Text(isDeafened ? "Deafened" : (isMicEnabled ? "Transmitting" : "Muted"))
                    .font(.system(size: 18, weight: .bold))
                    .foregroundStyle(isDeafened ? Color.secondary : Color.primary)
                
                if isMicEnabled && !isDeafened {
                    HStack(spacing: 4) {
                        Circle().fill(Color.green).frame(width: 6, height: 6)
                        Text("\(audioCapture.packetsSent) packets sent")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(Capsule().fill(Color.white.opacity(0.05)))
                } else if isDeafened {
                    Text("You cannot hear or speak")
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary.opacity(0.7))
                } else {
                    Text("Your audio is currently private")
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary.opacity(0.7))
                }
            }
            
            // Lush Control Duo: Compact Icon Capsule
            HStack(spacing: 0) {
                // Mic Toggle
                Button(action: { toggleMic(client: client) }) {
                    Image(systemName: isMicEnabled ? "mic.fill" : "mic.slash.fill")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundStyle(isMicEnabled ? .white : Color.secondary)
                        .frame(width: 44, height: 38)
                        .background(
                            isMicEnabled ? 
                            AnyShapeStyle(AuraTheme.Gradients.lushIndigo) : 
                            AnyShapeStyle(Color.clear)
                        )
                        .clipShape(.rect(cornerRadius: 8))
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
                        .foregroundStyle(isDeafened ? .white : Color.secondary)
                        .frame(width: 44, height: 38)
                        .background(isDeafened ? Color.red.opacity(0.6) : Color.clear)
                        .clipShape(.rect(cornerRadius: 8))
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
            Text(content)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(.ultraThinMaterial)
                .clipShape(Capsule())
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 12)
    }
    
    private func replyPreview(_ message: ChatMessage) -> some View {
        HStack {
            Rectangle()
                .fill(AuraTheme.Colors.primary)
                .frame(width: 3)
            
            VStack(alignment: .leading, spacing: 2) {
                Text("Replying to \(message.senderName)")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(AuraTheme.Colors.primary)
                Text(message.content)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            
            Spacer()
            
            Button(action: { replyingTo = nil }) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(.secondary)
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
                .clipShape(.rect(cornerRadius: 10))
                .onSubmit {
                    sendMessage(client: client)
                }
            
            Button(action: { sendMessage(client: client) }) {
                Image(systemName: "paperplane.fill")
                    .font(.system(size: 14, weight: .bold))
                    .foregroundStyle(.white)
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
    
    private func currentChannel(for client: QuicNetworkClient) -> ChannelModel? {
        let channelId = client.currentChannelId ?? client.channels.first?.id ?? 0
        return client.channels.first { $0.id == channelId }
    }
    
    private func switchChannel(to channelId: UInt32, client: QuicNetworkClient) {
        guard channelId != client.currentChannelId else { return }
        
        // Capture old/new names for divider
        let oldChannelId = client.currentChannelId ?? client.channels.first?.id ?? 0
        let oldChannelName = client.channels.first(where: { $0.id == oldChannelId })?.name ?? "Unknown"
        let newChannelName = client.channels.first(where: { $0.id == channelId })?.name ?? "Unknown"
        
        // Add divider if we have chat history
        if !chatMessages.isEmpty {
            let text = "Joined #\(newChannelName)"
            
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
                client.isMuted = true
                Task { await client.updateStatus(isMuted: true, isDeafened: isDeafened) }
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
                client.isMuted = false
                Task { await client.updateStatus(isMuted: false, isDeafened: isDeafened) }
            }
            
        case .alwaysOn:
            // Always transmit when enabled
            if isMicEnabled {
                audioCapture.stop()
                isMicEnabled = false
                client.isMuted = true
            } else {
                audioCapture.start { pcmData in
                    Task {
                        try? await client.sendAudioDatagram(pcmData)
                    }
                }
                isMicEnabled = true
                client.isMuted = false
            }
            Task { await client.updateStatus(isMuted: !isMicEnabled, isDeafened: isDeafened) }
            
        case .voiceActivation:
            // VAD mode - audio capture with voice detection
            if isMicEnabled {
                audioCapture.stop()
                isMicEnabled = false
                client.isMuted = true
            } else {
                audioCapture.start { pcmData in
                    Task {
                        try? await client.sendAudioDatagram(pcmData)
                    }
                }
                isMicEnabled = true
                client.isMuted = false
            }
            Task { await client.updateStatus(isMuted: !isMicEnabled, isDeafened: isDeafened) }
        }
    }
    
    private func toggleDeafen(client: QuicNetworkClient) {
        withAnimation(.spring(response: 0.3, dampingFraction: 0.7)) {
            isDeafened.toggle()
            client.isDeafened = isDeafened
            
            if isDeafened {
                // Auto-mute on deafen
                if isMicEnabled {
                    toggleMic(client: client)
                } else {
                    // Even if already muted, we need to sync the deafen state
                    Task { await client.updateStatus(isMuted: true, isDeafened: true) }
                }
            } else {
                // Sync undeafen state
                Task { await client.updateStatus(isMuted: !isMicEnabled, isDeafened: false) }
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

    @ViewBuilder
    private func channelRow(channel: ChannelModel, currentId: UInt32, client: QuicNetworkClient) -> some View {
                        VStack(alignment: .leading, spacing: 4) {
                            // Channel header
                            Button(action: {
                                switchChannel(to: channel.id, client: client)
                            }) {
                                HStack {
                                    if let emoji = channel.iconEmoji {
                                        Text(emoji)
                                            .frame(width: 20)
                                    } else {
                                        Image(systemName: channel.iconPresetId ?? "speaker.wave.2")
                                            .foregroundStyle(channel.id == currentId ? .blue : Color.secondary)
                                            .frame(width: 20)
                                    }
                                    
                                    VStack(alignment: .leading, spacing: 0) {
                                        Text(channel.name)
                                            .foregroundStyle(channel.id == currentId ? Color.primary : Color.secondary)
                                            .fontWeight(channel.id == currentId ? .semibold : .regular)
                                        
                                        if !channel.comment.isEmpty {
                                            Text(channel.comment)
                                                .font(.system(size: 10))
                                                .foregroundStyle(.secondary)
                                                .lineLimit(1)
                                        }
                                    }
                                    
                                    Spacer()
                                    
                                    // User count
                                    if let users = client.usersByChannel[channel.id], !users.isEmpty {
                                        Text(calculateUserCount(usersCount: users.count, channelId: channel.id, currentId: currentId))
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
                                .clipShape(.rect(cornerRadius: 8))
                                .auraFluidHover()
                                .contextMenu {
                                    if client.isAdmin {
                                        Button(action: {
                                            editingChannel = channel
                                            showingChannelEditor = true
                                        }) {
                                            Label("Edit Channel", systemImage: "pencil")
                                        }
                                        
                                        Divider()
                                        
                                        Button(role: .destructive, action: {
                                            // TODO: Implement delete
                                        }) {
                                            Label("Delete Channel", systemImage: "trash")
                                        }
                                    }
                                }
                            }
                            .buttonStyle(.plain)
                            
                            // Users in this channel
                            if let users = client.usersByChannel[channel.id] {
                                VStack(alignment: .leading, spacing: 4) {
                                    // Show current user if in this channel
                                    if channel.id == currentId {
                                        HStack(spacing: 8) {
                                            Circle()
                                                .fill(isMicEnabled ? Color.green : Color.secondary)
                                                .frame(width: 18, height: 18)
                                                .overlay(
                                                    Text(identity?.displayName.prefix(1).uppercased() ?? "U")
                                                        .font(.system(size: 8, weight: .bold))
                                                        .foregroundStyle(.white)
                                                )
                                            
                                            Text("You")
                                                .font(.system(size: 13))
                                                .foregroundStyle(.secondary)
                                            
                                            Spacer()
                                            
                                            if isDeafened {
                                                Image(systemName: "headphones.slash")
                                                    .font(.system(size: 10))
                                                    .foregroundStyle(.red)
                                            } else if !isMicEnabled {
                                                Image(systemName: "mic.slash.fill")
                                                    .font(.system(size: 10))
                                                    .foregroundStyle(.secondary)
                                            }
                                        }
                                        .padding(.leading, 24)
                                        .padding(.vertical, 2)
                                    }
                                    
                                    // Show other users
                                    ForEach(users) { user in
                                        UserRowView(user: user, isActiveSpeaker: client.activeSpeakers.contains(user.id))
                                    }
                                }
                                .padding(.top, 2)
                            }
                        }
                        .padding(.vertical, 2)
    }
}

// MARK: - Profile Edit View

struct ProfileEditView: View {
    @Environment(\.dismiss) var dismiss
    let client: QuicNetworkClient
    
    @State private var bio: String = ""
    @State private var avatarData: Data = Data()
    @State private var showingImagePicker = false
    
    init(client: QuicNetworkClient) {
        self.client = client
        // Initialize with current profile if available
        if let myProfile = client.profiles[client.sessionId ?? 0] {
            _bio = State(initialValue: myProfile.bio)
            _avatarData = State(initialValue: Data(myProfile.avatarData))
        }
    }
    
    var body: some View {
        VStack(spacing: 20) {
            Text("Edit Profile")
                .font(.title2.bold())
            
            VStack(spacing: 12) {
                // Avatar Preview
                ZStack(alignment: .bottomTrailing) {
                    if let image = NSImage(data: avatarData) {
                        Image(nsImage: image)
                            .resizable()
                            .aspectRatio(contentMode: .fill)
                            .frame(width: 80, height: 80)
                            .clipShape(Circle())
                    } else {
                        Circle()
                            .fill(AuraTheme.Gradients.primary)
                            .frame(width: 80, height: 80)
                    }
                    
                    Button(action: { selectImage() }) {
                        Image(systemName: "camera.fill")
                            .padding(6)
                            .background(Circle().fill(Color.blue))
                            .foregroundStyle(.white)
                    }
                    .buttonStyle(.plain)
                }
                
                VStack(alignment: .leading, spacing: 4) {
                    Text("Bio")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    TextEditor(text: $bio)
                        .frame(height: 100)
                        .padding(4)
                        .background(RoundedRectangle(cornerRadius: 8).stroke(Color.secondary.opacity(0.2)))
                }
            }
            .padding()
            .auraGlass()
            
            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.bordered)
                
                Spacer()
                
                Button("Save") {
                    Task {
                        await client.updateProfile(bio: bio, avatarData: avatarData)
                        dismiss()
                    }
                }
                .buttonStyle(.borderedProminent)
            }
        }
        .padding(30)
        .frame(width: 400)
    }
    
    private func selectImage() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowedContentTypes = [.image]
        
        if panel.runModal() == .OK {
            if let url = panel.url, let data = try? Data(contentsOf: url) {
                // Resize if needed (limit 128KB in proto)
                self.avatarData = data
            }
        }
    }
}

// MARK: - Channel Edit View

struct ChannelEditView: View {
    @Environment(\.dismiss) var dismiss
    let client: QuicNetworkClient
    let channel: ChannelModel?
    
    @State private var name: String = ""
    @State private var comment: String = ""
    @State private var emoji: String = "📁"
    
    init(client: QuicNetworkClient, channel: ChannelModel? = nil) {
        self.client = client
        self.channel = channel
        if let ch = channel {
            _name = State(initialValue: ch.name)
            _comment = State(initialValue: ch.comment)
            _emoji = State(initialValue: ch.iconEmoji ?? "📁")
        }
    }
    
    var body: some View {
        VStack(spacing: 20) {
            Text(channel == nil ? "Create Channel" : "Edit Channel")
                .font(.title2.bold())
            
            VStack(spacing: 12) {
                HStack {
                    Text("Icon")
                    TextField("Emoji", text: $emoji)
                        .frame(width: 50)
                    Spacer()
                }
                
                TextField("Channel Name", text: $name)
                    .textFieldStyle(.roundedBorder)
                
                TextField("Comment", text: $comment)
                    .textFieldStyle(.roundedBorder)
            }
            .padding()
            .auraGlass()
            
            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.bordered)
                
                Spacer()
                
                Button("Save") {
                    Task {
                        if let ch = channel {
                            await client.updateChannel(id: ch.id, name: name, comment: comment, emoji: emoji)
                        } else {
                            await client.createChannel(name: name, comment: comment, emoji: emoji)
                        }
                        dismiss()
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(name.isEmpty)
            }
        }
        .padding(30)
        .frame(width: 350)
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
                        .foregroundStyle(.secondary)
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
                                    .foregroundStyle(message.isOutgoing ? .white.opacity(0.8) : .blue)
                                Text(replyPreview)
                                    .font(.system(size: 10))
                                    .foregroundStyle(message.isOutgoing ? .white.opacity(0.6) : Color.secondary)
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
                    .foregroundStyle(.secondary)
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


struct UserRowView: View {
    let user: ChannelUser
    let isActiveSpeaker: Bool
    
    var body: some View {
    HStack(spacing: 8) {
        // Avatar
        if let avatarData = user.avatarData, let image = NSImage(data: avatarData) {
            Image(nsImage: image)
                .resizable()
                .aspectRatio(contentMode: .fill)
                .frame(width: 18, height: 18)
                .clipShape(Circle())
        } else {
            Circle()
                .fill(AuraTheme.Gradients.primary)
                .frame(width: 18, height: 18)
                .overlay(
                    Text(String(user.displayName.prefix(1).uppercased()))
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(.white)
                )
        }
        
        VStack(alignment: .leading, spacing: 0) {
            Text(user.displayName)
                .font(.system(size: 13))
                .foregroundStyle(isActiveSpeaker ? AuraTheme.Colors.accent : Color.secondary)
            
            if !user.bio.isEmpty {
                Text(user.bio)
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary.opacity(0.7))
                    .lineLimit(1)
            }
        }
        
        if isActiveSpeaker {
            Image(systemName: "waves.at.tail")
                .foregroundStyle(AuraTheme.Gradients.lushIndigo)
                .font(.system(size: 10))
                .transition(.scale.combined(with: .opacity))
        }
        
        Spacer()
        
        if user.isDeafened {
            Image(systemName: "headphones.slash")
                .font(.system(size: 10))
                .foregroundStyle(.red.opacity(0.7))
        } else if user.isMuted {
            Image(systemName: "mic.slash.fill")
                .font(.system(size: 10))
                .foregroundStyle(.secondary.opacity(0.7))
        }
    }
    .padding(.leading, 24)
    .padding(.vertical, 2)
    .help(user.bio.isEmpty ? user.displayName : "\(user.displayName): \(user.bio)")
    }
}

func calculateUserCount(usersCount: Int, channelId: UInt32, currentId: UInt32?) -> String {
    return String(usersCount + (channelId == currentId ? 1 : 0))
}
