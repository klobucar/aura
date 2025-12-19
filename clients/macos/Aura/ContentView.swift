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
            // Background gradient
            LinearGradient(
                colors: [
                    Color(nsColor: .windowBackgroundColor),
                    Color(nsColor: .controlBackgroundColor)
                ],
                startPoint: .top,
                endPoint: .bottom
            )
            .ignoresSafeArea()
            
            // Centered login card
            LoginView { connectedClient, connectedIdentity in
                self.client = connectedClient
                self.identity = connectedIdentity
                self.isConnected = true
            }
            .frame(width: 400, height: 550)
            .clipShape(RoundedRectangle(cornerRadius: 16))
            .shadow(color: .black.opacity(0.3), radius: 20, x: 0, y: 10)
        }
        .frame(minWidth: 500, minHeight: 600)
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
            channelDetailView(client: client)
        }
        .navigationTitle("")
    }
    
    // MARK: - User Header
    
    @ViewBuilder
    private func userHeader(identity: UserIdentity, client: QuicNetworkClient) -> some View {
        HStack(spacing: 12) {
            // Avatar
            Circle()
                .fill(LinearGradient(colors: [.blue, .purple], startPoint: .topLeading, endPoint: .bottomTrailing))
                .frame(width: 40, height: 40)
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
        .padding()
        .background(Color(nsColor: .controlBackgroundColor))
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
                                        .fontWeight(channel.id == currentId ? .medium : .regular)
                                    
                                    Spacer()
                                    
                                    // User count
                                    if let users = client.usersByChannel[channel.id], !users.isEmpty {
                                        Text("\(users.count + (channel.id == currentId ? 1 : 0))")
                                            .font(.caption2)
                                            .foregroundColor(.secondary)
                                    } else if channel.id == currentId {
                                        Text("1")
                                            .font(.caption2)
                                            .foregroundColor(.secondary)
                                    }
                                }
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
                    Text("Settings")
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
                .background(Color.blue.opacity(0.1))
                .cornerRadius(8)
            }
            .buttonStyle(.plain)
            .padding()
        }
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.3))
        .sheet(isPresented: $showingSettings) {
            SettingsView(settings: audioSettings, ttsManager: tts)
        }
    }

    
    // MARK: - Channel Detail View
    
    @ViewBuilder
    private func channelDetailView(client: QuicNetworkClient) -> some View {
        let channel = currentChannel(for: client)
        let userCount = (client.usersByChannel[client.currentChannelId ?? 1]?.count ?? 0) + 1
        
        VStack(spacing: 0) {
            // Channel header
            HStack {
                Image(systemName: channel?.icon ?? "speaker.wave.2")
                    .font(.title2)
                    .foregroundColor(.blue)
                
                Text(channel?.name ?? "Channel")
                    .font(.title2)
                    .fontWeight(.semibold)
                
                Spacer()
                
                // Toggle chat button
                Button(action: { showingChat.toggle() }) {
                    Image(systemName: showingChat ? "bubble.left.fill" : "bubble.left")
                        .foregroundColor(showingChat ? .blue : .secondary)
                }
                .buttonStyle(.plain)
                .help(showingChat ? "Hide Chat" : "Show Chat")
                
                // User count badge
                Text("\(userCount)")
                    .font(.caption)
                    .fontWeight(.medium)
                    .foregroundColor(.secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(Color.secondary.opacity(0.1))
                    .clipShape(Capsule())
            }
            .padding()
            .background(Color(nsColor: .controlBackgroundColor).opacity(0.5))
            
            Divider()
            
            // Main content - Voice and Chat
            HSplitView {
                // Voice status (left)
                VStack(spacing: 24) {
                    Spacer()
                    
                    ZStack {
                        Circle()
                            .fill(isMicEnabled ? Color.green.opacity(0.1) : Color.secondary.opacity(0.05))
                            .frame(width: 100, height: 100)
                        
                        Image(systemName: isMicEnabled ? "mic.fill" : "mic.slash.fill")
                            .font(.system(size: 40))
                            .foregroundColor(isMicEnabled ? .green : .secondary)
                    }
                    
                    VStack(spacing: 8) {
                        Text(isMicEnabled ? "Transmitting" : "Muted")
                            .font(.headline)
                            .fontWeight(.medium)
                        
                        if isMicEnabled {
                            Text("\(audioCapture.packetsSent) packets")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                    }
                    
                    Button(action: { toggleMic(client: client) }) {
                        Label(isMicEnabled ? "Mute" : "Unmute", 
                              systemImage: isMicEnabled ? "mic.fill" : "mic.slash")
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(isMicEnabled ? .green : .secondary)
                    .controlSize(.regular)
                    
                    Spacer()
                }
                .frame(minWidth: 200)
                
                // Chat panel (right)
                if showingChat {
                    VStack(spacing: 0) {
                        // Messages list
                        ScrollViewReader { scrollProxy in
                            ScrollView {
                                LazyVStack(alignment: .leading, spacing: 8) {
                                    ForEach(chatMessages) { message in
                                        if message.type == .info {
                                            HStack {
                                                VStack { Divider() }
                                                Text(message.content)
                                                    .font(.caption2)
                                                    .foregroundColor(.secondary)
                                                    .padding(.horizontal, 4)
                                                VStack { Divider() }
                                            }
                                            .padding(.vertical, 8)
                                            .padding(.horizontal, 16)
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
                        
                        Divider()
                        
                        // Reply preview bar
                        if let replying = replyingTo {
                            HStack {
                                Rectangle()
                                    .fill(Color.blue)
                                    .frame(width: 4)
                                
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("Replying to \(replying.senderName)")
                                        .font(.caption)
                                        .fontWeight(.semibold)
                                        .foregroundColor(.blue)
                                    Text(replying.content)
                                        .font(.caption)
                                        .foregroundColor(.secondary)
                                        .lineLimit(1)
                                }
                                
                                Spacer()
                                
                                Button(action: { replyingTo = nil }) {
                                    Image(systemName: "xmark.circle.fill")
                                        .foregroundColor(.secondary)
                                }
                                .buttonStyle(.plain)
                            }
                            .padding(.horizontal)
                            .padding(.vertical, 8)
                            .background(Color(nsColor: .controlBackgroundColor))
                        }
                        
                        // Message input
                        HStack(spacing: 8) {
                            TextField("Message...", text: $messageText)
                                .textFieldStyle(.plain)
                                .padding(8)
                                .background(Color(nsColor: .textBackgroundColor))
                                .cornerRadius(8)
                                .onSubmit {
                                    sendMessage(client: client)
                                }
                            
                            Button(action: { sendMessage(client: client) }) {
                                Image(systemName: "paperplane.fill")
                                    .foregroundColor(messageText.isEmpty ? .secondary : .blue)
                            }
                            .buttonStyle(.plain)
                            .disabled(messageText.isEmpty)
                        }
                        .padding()
                        .background(Color(nsColor: .controlBackgroundColor))
                    }
                    .frame(minWidth: 250)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(nsColor: .textBackgroundColor))
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
                                .fill(Color.blue.opacity(0.6))
                                .frame(width: 3)
                            
                            VStack(alignment: .leading, spacing: 1) {
                                Text(message.replyToSender ?? "")
                                    .font(.caption2)
                                    .fontWeight(.semibold)
                                    .foregroundColor(.blue)
                                Text(replyPreview)
                                    .font(.caption2)
                                    .foregroundColor(.secondary)
                                    .lineLimit(1)
                            }
                        }
                        .padding(.bottom, 2)
                    }
                    
                    // Message content with markdown rendering
                    MarkdownText(message.content, foregroundColor: message.isOutgoing ? .white : .primary)
                        .font(.body)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(
                    Group {
                        if message.isOutgoing {
                            // Blue gradient for outgoing (Messages.app style)
                            LinearGradient(
                                colors: [Color.blue, Color.blue.opacity(0.9)],
                                startPoint: .topLeading,
                                endPoint: .bottomTrailing
                            )
                        } else {
                            // Solid grey for incoming messages
                            Color(nsColor: .separatorColor)
                        }
                    }
                )
                .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                .shadow(color: .black.opacity(0.08), radius: 2, x: 0, y: 1)
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
