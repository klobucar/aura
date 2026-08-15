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
    @State private var channelPendingDeletion: ChannelModel?
    @State private var pttCancellable: AnyCancellable?
    @State private var pttErrorMessage: String?
    /// When the PTT key went down, for the HUD's `held 0:04` readout.
    @State private var pttHeldSince: Date?

    // Chat state
    @State private var messageText = ""
    @State private var replyingTo: ChatMessage?

    // Layout + voice surface state
    @StateObject private var appSettings = AppSettings.shared
    @StateObject private var talkStore = TalkSegmentStore()

    // Management views
    @State private var showingServerManagement = false
    @State private var showingProfileManagement = false
    @StateObject private var serverManager = ServerManager()
    @StateObject private var profileManager = ProfileManager()
    
    @FocusState private var isInputFocused: Bool
    
    
    var body: some View {
        Group {
            if isConnected, let client = client, let identity = identity {
                connectedView(client: client, identity: identity)
            } else {
                loginView
            }
        }
        .alert(
            "Push-to-Talk",
            isPresented: Binding(
                get: { pttErrorMessage != nil },
                set: { if !$0 { pttErrorMessage = nil } }
            ),
            presenting: pttErrorMessage
        ) { _ in
            Button("OK", role: .cancel) { pttErrorMessage = nil }
        } message: { msg in
            Text(msg)
        }
        .confirmationDialog(
            "Delete Channel",
            isPresented: Binding(
                get: { channelPendingDeletion != nil },
                set: { if !$0 { channelPendingDeletion = nil } }
            ),
            presenting: channelPendingDeletion
        ) { channel in
            Button("Delete \"\(channel.name)\"", role: .destructive) {
                let id = channel.id
                channelPendingDeletion = nil
                Task { await client?.deleteChannel(id: id) }
            }
            Button("Cancel", role: .cancel) { channelPendingDeletion = nil }
        } message: { channel in
            Text("This permanently deletes \"\(channel.name)\" and removes everyone from it. This cannot be undone.")
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
                .frame(height: AuraTheme.Layout.headerHeight)
                .padding(.top, 28) // Synced top offset
                
                // User info header
                userHeader(identity: identity, client: client)
                
                // Channels list
                channelList(client: client)
                    .padding(.top, 8)
            }
            .frame(width: AuraTheme.Layout.rosterWidth)
            .background(VisualEffectBlur(auraMaterial: .sidebar, blendingMode: .withinWindow).opacity(0.8))
            .navigationSplitViewColumnWidth(AuraTheme.Layout.rosterWidth)
        } detail: {
            // Main content area
            ZStack(alignment: .bottom) {
                channelDetailView(client: client, identity: identity)

            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background {
            ZStack {
                AuraTheme.Colors.backgroundGradient
                
                // Animated Aura field (bleeds across sidebar and detail)
                Group {
                    Circle()
                        .fill(AuraTheme.Colors.primary.opacity(0.1))
                        .frame(width: 800, height: 800)
                        .blur(radius: 100)
                        .offset(x: -300, y: -200)
                    
                    Circle()
                        .fill(AuraTheme.Colors.secondary.opacity(0.08))
                        .frame(width: 600, height: 600)
                        .blur(radius: 80)
                        .offset(x: 300, y: 100)
                    
                    Circle()
                        .fill(AuraTheme.Colors.accent.opacity(0.05))
                        .frame(width: 400, height: 400)
                        .blur(radius: 60)
                        .offset(x: 0, y: 300)
                }
            }
            .ignoresSafeArea()
        }
        .sheet(isPresented: $showingProfileEditor) {
            ProfileView(client: client)
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
        .padding(12)
        .auraGlass(cornerRadius: 14)
        .padding(.horizontal, 8)
        .padding(.top, 4)
    }
    
    // MARK: - Channel List
    
    @ViewBuilder
    private func channelList(client: QuicNetworkClient) -> some View {
        let lobbyId = client.channels.first(where: { $0.isLobby })?.id
        let currentId: String = client.currentChannelId ?? lobbyId ?? client.channels.first?.id ?? "0"
        
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
            .padding(.horizontal, 8)
            .padding(.bottom, 12)
        }
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.3))
        .sheet(isPresented: $showingSettings) {
            SettingsView(settings: audioSettings, ttsManager: tts)
        }
        .onAppear {
            autoJoinLobbyIfNeeded(client: client)
        }
        // The channel list is empty until the ServerState snapshot lands after
        // auth, which is usually *after* this view first appears — so onAppear
        // alone would find no lobby to join and never fire again, leaving
        // currentChannelId nil. Nothing looks wrong (the header falls back to
        // the first channel) but chat is filtered on currentChannelId, so every
        // message silently vanishes. Re-check whenever the roster arrives.
        .onChange(of: client.channels) { _, _ in
            autoJoinLobbyIfNeeded(client: client)
        }
        .onChange(of: client.isConnected) { _, _ in
            autoJoinLobbyIfNeeded(client: client)
        }
    }

    /// Join the lobby once we are connected and know what channels exist.
    /// Safe to call repeatedly — `switchChannel` no-ops once we are in a
    /// channel, and this does nothing until the roster is known.
    private func autoJoinLobbyIfNeeded(client: QuicNetworkClient) {
        guard client.isConnected, client.currentChannelId == nil else { return }
        guard let target = client.channels.first(where: { $0.isLobby })?.id
            ?? client.channels.first?.id
        else { return }
        switchChannel(to: target, client: client)
    }

    
    // MARK: - Channel Detail View
    
    @ViewBuilder
    private func channelDetailView(client: QuicNetworkClient, identity: UserIdentity) -> some View {
        // Each column carries its own 52px header so the two line up into one
        // unified toolbar with the divider running through it, as in the mock.
        channelColumns(client: client, identity: identity)
    }

    // MARK: - Channel Columns
    //
    // Chat-first: chat flex · voice rail 336. Voice-focus: voice flex · chat
    // 320. Only the two widths animate; the roster is untouched by the swap.

    /// The two swappable columns of the channel window.
    private enum ChannelColumn: Hashable {
        case chat
        case voice
    }

    private var layoutMode: AuraLayoutMode { appSettings.layoutMode }

    /// Floors for a cramped window — the column that is supposed to be wide
    /// keeps at least this much before the other one starts giving ground.
    private static let minChatWidth: CGFloat = 280
    private static let minRailWidth: CGFloat = 300

    private var columnOrder: [ChannelColumn] {
        layoutMode == .chatFirst ? [.chat, .voice] : [.voice, .chat]
    }

    @ViewBuilder
    private func channelColumns(client: QuicNetworkClient, identity: UserIdentity) -> some View {
        GeometryReader { geometry in
            // Both widths are concrete numbers so the spring has something to
            // interpolate; handing one column a nil frame and letting the
            // stack decide would snap instead of glide. The narrow column
            // yields first on a cramped window so neither ever hits zero.
            let total = geometry.size.width
            let chatWidth: CGFloat = layoutMode == .chatFirst
                ? max(0, total - min(AuraTheme.Layout.voiceRailWidth, max(0, total - Self.minChatWidth)))
                : min(AuraTheme.Layout.chatNarrowWidth, max(0, total - Self.minRailWidth))
            let voiceWidth = max(0, total - chatWidth)

            HStack(spacing: 0) {
                // ForEach over the order — rather than an if/else — so each
                // column keeps its identity across the swap. That is what
                // preserves the chat scroll position the handoff asks for.
                ForEach(columnOrder, id: \.self) { column in
                    switch column {
                    case .chat:
                        chatColumn(client: client)
                            .frame(width: chatWidth)
                    case .voice:
                        voiceRail(client: client, identity: identity)
                            .frame(width: voiceWidth)
                            // Seam between the two columns, on whichever side
                            // chat happens to be.
                            .overlay(alignment: layoutMode == .chatFirst ? .leading : .trailing) {
                                Rectangle()
                                    .fill(Color.white.opacity(0.08))
                                    .frame(width: 1)
                            }
                    }
                }
            }
            .animation(AuraTheme.Motion.modeSwap, value: layoutMode)
        }
    }

    @ViewBuilder
    private func chatColumn(client: QuicNetworkClient) -> some View {
        VStack(spacing: 0) {
            channelHeader(client: client)

            Divider().opacity(0.1)

            chatPanel(client: client)
        }
    }

    @ViewBuilder
    private func voiceRail(client: QuicNetworkClient, identity: UserIdentity) -> some View {
        VoiceRailView(
            store: talkStore,
            client: client,
            context: voiceContext(client: client, identity: identity),
            actions: VoiceRailActions(
                toggleMic: { toggleMic(client: client) },
                toggleDeafen: { toggleDeafen(client: client) },
                localVolume: { client.localVolume(for: $0) },
                setLocalVolume: { client.setLocalVolume(sessionId: $0, volume: $1) }
            ),
            mode: layoutMode
        )
        .onAppear {
            // The store owns the sampling cadence; it just needs to know where
            // the level comes from. Local capture level while you hold the
            // floor, mixed receive level while somebody else does.
            talkStore.levelProvider = { [weak client, weak talkStore] in
                guard let client = client, let talkStore = talkStore else { return 0 }
                let db = talkStore.currentSpeakerId == client.sessionId
                    ? client.inputLevelDb
                    : client.outputLevelDb
                return VoiceLevel.normalized(db)
            }
            pushSpeakingState(client: client)
        }
        .onChange(of: client.activeSpeakers) { _, _ in
            pushSpeakingState(client: client)
        }
        .onChange(of: client.isLocalSpeaking) { _, _ in
            pushSpeakingState(client: client)
        }
        .onChange(of: client.currentChannelId) { _, _ in
            // A different channel is a different conversation; the lanes must
            // not carry the previous room's history over.
            talkStore.reset()
        }
        .onChange(of: hotkeyManager.isPTTActive) { _, isActive in
            pttHeldSince = isActive ? Date() : nil
        }
    }

    /// Everything the voice surface needs, gathered in one place.
    private func voiceContext(client: QuicNetworkClient, identity: UserIdentity) -> VoiceRailContext {
        let localAvatar = client.sessionId
            .flatMap { client.profiles[$0]?.avatarData }
            .flatMap { $0.isEmpty ? nil : $0 }

        var participants: [VoiceParticipant] = []
        if let sessionId = client.sessionId {
            participants.append(VoiceParticipant(
                id: sessionId,
                displayName: identity.displayName,
                avatarData: localAvatar,
                isLocal: true,
                isMuted: !isMicEnabled,
                isDeafened: isDeafened,
                isDisconnected: false
            ))
        }
        let currentId = currentChannelId(for: client)
        for user in client.usersByChannel[currentId] ?? [] {
            participants.append(VoiceParticipant(
                id: user.id,
                displayName: user.displayName,
                avatarData: user.avatarData,
                isLocal: false,
                isMuted: user.isMuted,
                isDeafened: user.isDeafened,
                isDisconnected: user.isDisconnected
            ))
        }

        let isPTT = audioSettings.transmissionMode == .pushToTalk
        let transmissionLabel = isPTT
            ? (audioSettings.pttHotkey?.displayString ?? "unset")
            : audioSettings.transmissionMode.displayName.lowercased()

        return VoiceRailContext(
            participants: participants,
            latencyMs: client.latencyMs,
            isReconnecting: !client.isConnected,
            isMicEnabled: isMicEnabled,
            isDeafened: isDeafened,
            isPTTHeld: hotkeyManager.isPTTActive,
            pttHeldSince: pttHeldSince,
            transmissionLabel: transmissionLabel,
            isPushToTalk: isPTT
        )
    }

    /// Feeds both talking sources into the segment store. `activeSpeakers` is
    /// remote-only by construction — it is populated from the receiver's mixer —
    /// so the local lane comes from `isLocalSpeaking` instead.
    private func pushSpeakingState(client: QuicNetworkClient) {
        var speaking = client.activeSpeakers
        if client.isLocalSpeaking, let sessionId = client.sessionId {
            speaking.insert(sessionId)
        }
        talkStore.setSpeaking(speaking)
    }


    @ViewBuilder
    private func channelHeader(client: QuicNetworkClient) -> some View {
        let channel = currentChannel(for: client)
        let lobbyId = client.channels.first(where: { $0.isLobby })?.id
        let currentId: String = client.currentChannelId ?? lobbyId ?? client.channels.first?.id ?? "0"
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
                .offset(y: -2) // Pixel-perfect horizontal alignment with "Aura" text
            
            Spacer()

            layoutModeToggle

            // User count badge
            Text("\(userCount)")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .auraGlass(cornerRadius: 10)
        }
        .padding(.horizontal, 16)
        .frame(height: AuraTheme.Layout.headerHeight)
        .padding(.top, 28) // Synced with sidebar header at 28
        .background(VisualEffectBlur(auraMaterial: .header, blendingMode: .withinWindow))
    }
    
    /// Flips the channel window between Chat-first and Voice-focus. The icon
    /// shows the mode you are in, following the header's existing idiom.
    private var layoutModeToggle: some View {
        Button {
            withAnimation(AuraTheme.Motion.modeSwap) {
                appSettings.toggleLayoutMode()
            }
        } label: {
            Image(systemName: layoutMode.iconName)
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(AuraTheme.Colors.primary)
        }
        .buttonStyle(.plain)
        .auraFluidHover()
        .keyboardShortcut("v", modifiers: [.command, .option])
        .help("Switch to \(layoutMode.other.displayName) layout (⌘⌥V)")
    }

    @ViewBuilder
    private func chatPanel(client: QuicNetworkClient) -> some View {
        let currentMessages = computedChatMessages(client: client)
        
        return VStack(spacing: 0) {
            // Messages list
            ScrollViewReader { scrollProxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(currentMessages) { message in
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
                .onChange(of: currentMessages.count) { _, _ in
                    if let lastMessage = currentMessages.last {
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
        .background(AuraTheme.Colors.background.opacity(0.5))
        .onChange(of: client.receivedMessages) { oldValue, newValue in
            let newMsgs = newValue.filter { newMsg in oldValue.first(where: { $0.id == newMsg.id }) == nil }
            for msg in newMsgs where msg.channelId == client.currentChannelId && msg.senderSessionId != client.sessionId {
                tts.speakMessage(sender: msg.senderName, content: msg.content)
            }
        }
        .onChange(of: client.systemEvents) { oldValue, newValue in
            let newEvents = newValue.filter { newEvent in oldValue.first(where: { $0.id == newEvent.id }) == nil }
            for event in newEvents where event.channelId == client.currentChannelId || event.channelId == "0" {
                if event.content.contains("joined") {
                    let name = event.content.replacingOccurrences(of: " joined the channel", with: "")
                    tts.speakJoin(name: name)
                } else if event.content.contains("left") {
                    let name = event.content.replacingOccurrences(of: " left the channel", with: "")
                    tts.speakLeave(name: name)
                }
            }
        }
    }
    
    // Combined chat messages for the current channel (computed dynamically)
    private func computedChatMessages(client: QuicNetworkClient) -> [ChatMessage] {
        guard let currentChannelId = client.currentChannelId else { return [] }
        var messages: [ChatMessage] = []
        
        // Add text messages
        for msg in client.receivedMessages where msg.channelId == currentChannelId {
            var chatMsg = ChatMessage(
                id: msg.id,
                senderName: msg.senderName,
                content: msg.content,
                timestamp: msg.timestamp,
                isOutgoing: msg.senderSessionId == client.sessionId
            )
            chatMsg.channelId = msg.channelId
            
            if let replyId = msg.replyToId, 
               let originalMsg = client.receivedMessages.first(where: { $0.id == replyId }) {
                chatMsg.replyToId = replyId
                chatMsg.replyToSender = originalMsg.senderName
                chatMsg.replyToPreview = String(originalMsg.content.prefix(50))
            }
            messages.append(chatMsg)
        }
        
        // Add system events
        for event in client.systemEvents where event.channelId == currentChannelId || event.channelId == "0" {
            var message = ChatMessage(
                id: "sys_\(event.id.uuidString)",
                senderName: "System",
                content: event.content,
                timestamp: event.timestamp,
                isOutgoing: false
            )
            message.type = .info
            message.channelId = event.channelId
            messages.append(message)
        }
        
        // Sort by timestamp
        return messages.sorted { $0.timestamp < $1.timestamp }
    }
    
    private func infoMessageRow(_ text: String) -> some View {
        HStack {
            Spacer()
            Text(text)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(.secondary.opacity(0.8))
                .padding(.horizontal, 14)
                .padding(.vertical, 6)
                .background {
                    Capsule()
                        .fill(AuraTheme.Colors.ultraFrosted)
                        .auraGlass(cornerRadius: 15)
                        .auraGlow(color: AuraTheme.Colors.primary.opacity(0.15), radius: 6)
                }
            Spacer()
        }
        .padding(.vertical, 10)
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
                .focused($isInputFocused)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(Color.primary.opacity(isInputFocused ? 0.08 : 0.02))
                        .auraGlow(color: AuraTheme.Colors.primary.opacity(0.4), radius: isInputFocused ? 4 : 0)
                }
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
        .padding(8)
        .auraGlass(cornerRadius: 24, material: .hudWindow)
        .auraGlow(color: AuraTheme.Colors.primary.opacity(isInputFocused ? 0.3 : 0), radius: 15)
        .padding(.horizontal, 16)
        .padding(.bottom, 16)
    }
    
    private func divider() -> some View {
        Divider().opacity(0.1)
    }
    
    // MARK: - Helpers
    
    private func currentChannel(for client: QuicNetworkClient) -> ChannelModel? {
        let channelId = currentChannelId(for: client)
        return client.channels.first { $0.id == channelId }
    }

    private func currentChannelId(for client: QuicNetworkClient) -> String {
        client.currentChannelId
            ?? client.channels.first(where: { $0.isLobby })?.id
            ?? client.channels.first?.id
            ?? "0"
    }
    
    private func switchChannel(to channelId: String, client: QuicNetworkClient) {
        guard channelId != client.currentChannelId else { return }
        
        // Capture old/new names for divider
        let oldChannelId: String = client.currentChannelId ?? client.channels.first?.id ?? "0"
        let oldChannelName = client.channels.first(where: { $0.id == oldChannelId })?.name ?? "Unknown"
        let newChannelName = client.channels.first(where: { $0.id == channelId })?.name ?? "Unknown"
        
        // Add divider if we have chat history
        let currentMessages = computedChatMessages(client: client)
        if !currentMessages.isEmpty {
            let text = "Joined #\(newChannelName)"
            client.systemEvents.append(SystemEvent(content: text, channelId: channelId))
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
                hotkeyManager.unregisterHotkey()
                audioCapture.stop()
                client.clearLocalSpeaking()
                isMicEnabled = false
                client.isMuted = true
                Task { await client.updateStatus(isMuted: true, isDeafened: isDeafened) }
            } else {
                // Enable PTT — reject up front if prerequisites are missing
                // instead of silently installing a subscription that will
                // never fire.
                guard let hotkey = audioSettings.pttHotkey else {
                    pttErrorMessage = "Set a Push-to-Talk hotkey in Settings → Audio before enabling this mode."
                    return
                }
                hotkeyManager.registerHotkey(hotkey)
                if !hotkeyManager.hasAccessibilityPermission {
                    // registerHotkey has already queued the pending
                    // registration and prompted for permission; tell the
                    // user so they know where to go.
                    pttErrorMessage = "Aura needs Accessibility permission to detect your Push-to-Talk key. Grant it in System Settings → Privacy & Security → Accessibility, then press Enable again."
                    return
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
                            client.clearLocalSpeaking()
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
                client.clearLocalSpeaking()
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
                client.clearLocalSpeaking()
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
        client?.clearLocalSpeaking()
        client?.disconnect()
        client = nil
        identity = nil
        isConnected = false
        isMicEnabled = false
        messageText = ""
        talkStore.reset()
    }
    
    private func sendMessage(client: QuicNetworkClient) {
        guard !messageText.isEmpty else { return }
        
        // Without a channel there is nowhere to put the message: the optimistic
        // copy would be filed under a placeholder id that the chat filter never
        // matches, so it would vanish on send. Say so instead of losing it.
        guard let channelId = client.currentChannelId else {
            client.systemEvents.append(
                SystemEvent(content: "Not in a channel yet — message not sent", channelId: "0"))
            return
        }

        let content = messageText
        let replying = replyingTo
        messageText = "" // Clear immediately for UX
        replyingTo = nil // Clear reply state
        let timestamp = Date()
        let sessionId = client.sessionId ?? 0

        // Use UUID message ID
        let messageId = "msg_\(UUID().uuidString)"
        
        // Optimistically add to client.receivedMessages
        let msg = ReceivedTextMessage(
            id: messageId,
            senderSessionId: sessionId,
            senderName: identity?.displayName ?? "You",
            channelId: channelId,
            content: content,
            timestamp: timestamp,
            rawPacket: Data(),
            replyToId: replying?.id
        )
        client.receivedMessages.append(msg)
        
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
    private func channelRow(channel: ChannelModel, currentId: String, client: QuicNetworkClient) -> some View {
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
                                        HStack(spacing: 4) {
                                            Text(channel.name)
                                                .foregroundStyle(channel.id == currentId ? Color.primary : Color.secondary)
                                                .fontWeight(channel.id == currentId ? .semibold : .regular)
                                            
                                            if channel.isLobby {
                                                Text("LOBBY")
                                                    .font(.system(size: 8, weight: .bold))
                                                    .padding(.horizontal, 4)
                                                    .padding(.vertical, 1)
                                                    .background(Capsule().fill(Color.blue.opacity(0.1)))
                                                    .foregroundStyle(.blue)
                                            }
                                        }
                                        
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
                                            channelPendingDeletion = channel
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
                                        UserRowView(user: user, isActiveSpeaker: client.activeSpeakers.contains(user.id), client: client)
                                    }
                                }
                                .padding(.top, 2)
                            }
                        }
                        .padding(.vertical, 2)
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
    var channelId: String = "0"
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
    let client: QuicNetworkClient

    private var isLocallyMuted: Bool {
        client.isLocallyMuted(sessionId: user.id)
    }

    private var localVolume: Float {
        client.localVolume(for: user.id)
    }

    var body: some View {
    HStack(spacing: 8) {
        // Avatar
        ZStack {
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

            if user.isDisconnected {
                Circle()
                    .fill(.black.opacity(0.3))
                    .frame(width: 18, height: 18)
                Image(systemName: "xmark")
                    .font(.system(size: 8, weight: .black))
                    .foregroundStyle(.white)
            }
        }

        VStack(alignment: .leading, spacing: 0) {
            Text(user.displayName)
                .font(.system(size: 13))
                .foregroundStyle(user.isDisconnected ? Color.secondary.opacity(0.5) : (isActiveSpeaker ? AuraTheme.Colors.accent : Color.secondary))

            if !user.bio.isEmpty && !user.isDisconnected {
                Text(user.bio)
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary.opacity(0.7))
                    .lineLimit(1)
            }
        }

        if isActiveSpeaker && !user.isDisconnected && !isLocallyMuted {
            Image(systemName: "waves.at.tail")
                .foregroundStyle(AuraTheme.Gradients.lushIndigo)
                .font(.system(size: 10))
                .transition(.scale.combined(with: .opacity))
        }

        Spacer()

        // Local-only volume indicator so users know why someone sounds loud/quiet
        if !user.isDisconnected && abs(localVolume - 1.0) >= 0.01 {
            Text("\(Int(localVolume * 100))%")
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                .foregroundStyle(.secondary.opacity(0.7))
        }

        if isLocallyMuted && !user.isDisconnected {
            Image(systemName: "speaker.slash.fill")
                .font(.system(size: 10))
                .foregroundStyle(.orange.opacity(0.8))
                .help("Muted locally")
        } else if user.isDisconnected {
            Text("LEFT")
                .font(.system(size: 8, weight: .bold))
                .foregroundStyle(.red.opacity(0.6))
        } else if user.isDeafened {
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
    .grayscale(user.isDisconnected ? 1.0 : 0.0)
    .opacity(user.isDisconnected ? 0.5 : 1.0)
    .help(user.bio.isEmpty ? user.displayName : "\(user.displayName): \(user.bio)")
    .contextMenu {
        if !user.isDisconnected {
            Button(isLocallyMuted ? "Unmute Locally" : "Mute Locally") {
                client.toggleLocalMute(sessionId: user.id)
            }
            Menu("Volume") {
                ForEach([0, 25, 50, 75, 100, 125, 150, 200], id: \.self) { pct in
                    Button("\(pct)%\(Int(localVolume * 100) == pct ? " ✓" : "")") {
                        client.setLocalVolume(sessionId: user.id, volume: Float(pct) / 100.0)
                    }
                }
            }
        }
    }
    }
}

func calculateUserCount(usersCount: Int, channelId: String, currentId: String?) -> String {
    return String(usersCount + (channelId == currentId ? 1 : 0))
}
