//
//  ContentView.swift
//  Aura
//

import AuraCore
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
    /// Outgoing messages the server never acknowledged taking. Kept in the
    /// list, dimmed, with a retry — never silently dropped.
    @State private var failedMessageIds: Set<String> = []
    /// Channel comment cards the user collapsed this session.
    @State private var collapsedComments: Set<String> = []

    // Layout + voice surface state
    @StateObject private var appSettings = AppSettings.shared
    @StateObject private var talkStore = TalkSegmentStore()

    // Management views
    @State private var showingServerManagement = false
    @State private var showingProfileManagement = false

    /// Identity verification (§7) and the roster's view of the same trust
    /// state. One `TrustStore` behind both, so a verification made in the
    /// sheet shows on the roster without a reload.
    @StateObject private var trustSheet: TrustSheetModel
    @StateObject private var rosterTrust: RosterTrustModel
    @StateObject private var serverManager = ServerManager()
    @StateObject private var profileManager = ProfileManager()

    @FocusState private var isInputFocused: Bool

    // Invites
    /// An invite to follow from the login screen.
    @State private var pendingInvite: AuraInviteLink?
    /// An invite to another server, waiting for "Leave … and join …?".
    @State private var inviteToConfirm: AuraInviteLink?
    @State private var inviteError: String?
    /// Channel to join once the invite's server has sent its channel list.
    @State private var pendingInviteChannelId: String?
    @State private var showingInvite = false

    // Mentions
    @AppStorage(NotificationPrefs.keywordsKey) private var notifyKeywords = ""
    /// Whether the chat is scrolled to (or near) its last message.
    @State private var isChatAtBottom = true
    /// Messages mentioning you that arrived below a chat scrolled up, oldest
    /// first.
    @State private var unseenMentionIds: [String] = []

    // Leaving a server
    @State private var confirmingLeave = false
    @State private var accountNotice: String?
    @State private var leaveError: String?

    init() {
        let trust = TrustStore()
        _trustSheet = StateObject(wrappedValue: TrustSheetModel(trust: trust))
        _rosterTrust = StateObject(wrappedValue: RosterTrustModel(trust: trust))
    }

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
        // aura:// invite links. Preferring every event keeps an open window
        // handling the link instead of SwiftUI opening a second one.
        .onOpenURL(perform: handleOpenURL)
        .handlesExternalEvents(preferring: ["*"], allowing: ["*"])
        .alert(
            "Invalid invite link",
            isPresented: Binding(get: { inviteError != nil }, set: { if !$0 { inviteError = nil } }),
            presenting: inviteError
        ) { _ in
            Button("OK", role: .cancel) {}
        } message: { message in
            Text(message)
        }
        .alert(
            ChatFormatting.leaveAndJoinTitle(
                currentHost: client?.connectedHost ?? "",
                newHost: inviteToConfirm?.host ?? ""
            ),
            isPresented: Binding(get: { inviteToConfirm != nil }, set: { if !$0 { inviteToConfirm = nil } }),
            presenting: inviteToConfirm
        ) { invite in
            Button("Cancel", role: .cancel) {}
            Button("Leave and Join") { followInviteElsewhere(invite) }
        }
        .alert(
            accountNotice ?? "",
            isPresented: Binding(get: { accountNotice != nil }, set: { if !$0 { accountNotice = nil } })
        ) {
            Button("OK", role: .cancel) {}
        }
        .alert(
            "Couldn't delete your account",
            isPresented: Binding(get: { leaveError != nil }, set: { if !$0 { leaveError = nil } }),
            presenting: leaveError
        ) { _ in
            Button("OK", role: .cancel) {}
        } message: { message in
            Text(message)
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
            LoginView(
                invite: pendingInvite,
                onDismissInvite: { pendingInvite = nil }
            ) { connectedClient, connectedIdentity in
                // Followed an invite to its server: open its channel once the
                // channel list arrives.
                if let invite = pendingInvite,
                   invite.matches(host: connectedClient.connectedHost ?? "", port: connectedClient.connectedPort ?? 0) {
                    pendingInviteChannelId = invite.channelId
                }
                pendingInvite = nil
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
        let currentId = currentChannelId(for: client)

        NavigationSplitView {
            RosterSidebar(
                client: client,
                identity: identity,
                rosterTrust: rosterTrust,
                currentChannelId: currentId,
                isMicEnabled: isMicEnabled,
                isDeafened: isDeafened,
                actions: rosterActions(client: client)
            )
            .frame(width: AuraTheme.Layout.rosterWidth)
            .background(VisualEffectBlur(auraMaterial: .sidebar, blendingMode: .withinWindow).opacity(0.8))
            .navigationSplitViewColumnWidth(AuraTheme.Layout.rosterWidth)
            .onAppear {
                autoJoinLobbyIfNeeded(client: client)
            }
            // The channel list is empty until the ServerState snapshot lands
            // after auth, which is usually *after* this view first appears — so
            // onAppear alone would find no lobby to join and never fire again,
            // leaving currentChannelId nil. Nothing looks wrong (the header
            // falls back to the first channel) but chat is filtered on
            // currentChannelId, so every message silently vanishes. Re-check
            // whenever the roster arrives.
            .onChange(of: client.channels) { _, _ in
                autoJoinLobbyIfNeeded(client: client)
            }
            .onChange(of: client.isConnected) { _, _ in
                autoJoinLobbyIfNeeded(client: client)
            }
        } detail: {
            channelColumns(client: client, identity: identity)
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
        // Member keys arrive with the MLS Welcome, some time after the join,
        // and change as people come and go — poll the (small) group rather
        // than chase every event that could move it.
        .task(id: currentId) {
            while !Task.isCancelled {
                rosterTrust.refresh(mls: client.mls, channelId: client.currentChannelId)
                try? await Task.sleep(for: .seconds(3))
            }
        }
        .onChange(of: trustSheet.isPresented) { _, isPresented in
            if !isPresented {
                rosterTrust.refresh(mls: client.mls, channelId: client.currentChannelId)
            }
        }
        .sheet(isPresented: $showingSettings) {
            SettingsView(
                settings: audioSettings,
                ttsManager: tts,
                identity: identity,
                serverHost: client.isAuthenticated ? client.connectedHost : nil,
                onLeaveServer: { leaveServer(client: client) }
            )
        }
        .leaveServerAlert(
            isPresented: $confirmingLeave,
            host: client.connectedHost ?? "",
            name: identity.displayName
        ) {
            leaveServer(client: client)
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
        // Trust sheet (§7). Presented over the channel so the room stays
        // visible behind it, which is the point of the material.
        .sheet(isPresented: $trustSheet.isPresented) {
            TrustSheetView(model: trustSheet)
        }
    }

    private func rosterActions(client: QuicNetworkClient) -> RosterActions {
        RosterActions(
            selectChannel: { switchChannel(to: $0, client: client) },
            editProfile: { showingProfileEditor = true },
            disconnect: disconnect,
            openSettings: { showingSettings = true },
            createChannel: {
                editingChannel = nil
                showingChannelEditor = true
            },
            editChannel: { channel in
                editingChannel = channel
                showingChannelEditor = true
            },
            deleteChannel: { channelPendingDeletion = $0 },
            compareKeys: { openTrustSheet(focusUuid: $0) },
            leaveServer: { confirmingLeave = true }
        )
    }

    /// Open the trust sheet for the current channel, optionally focused on one
    /// member — the roster and the rail's `verify key` row both arrive here
    /// with someone in mind.
    private func openTrustSheet(focusUuid: String? = nil) {
        guard let client else { return }
        let channelId = currentChannelId(for: client)
        let name = client.channels.first { $0.id == channelId }?.name ?? ""

        trustSheet.nameForUuid = { [weak client] uuid in
            client?.displayName(forUuid: uuid)
        }

        Task {
            await trustSheet.open(
                mls: client.mls,
                channelId: channelId,
                channelName: name,
                focusUuid: focusUuid
            )
        }
    }

    /// Join the lobby once we are connected and know what channels exist.
    /// Safe to call repeatedly — `switchChannel` no-ops once we are in a
    /// channel, and this does nothing until the roster is known.
    private func autoJoinLobbyIfNeeded(client: QuicNetworkClient) {
        // An invite's channel comes first, once the channel list is in. If
        // the channel no longer exists, fall through to the lobby.
        if let target = pendingInviteChannelId, client.isConnected, !client.channels.isEmpty {
            pendingInviteChannelId = nil
            if client.channels.contains(where: { $0.id == target }) {
                switchChannel(to: target, client: client)
                return
            }
        }

        // Also covers a channel that vanished while we were away: a reconnect
        // rejoins the channel we were in, and the server accepts a join to an
        // id it no longer lists, so check against the fresh channel list.
        let isInListedChannel = client.currentChannelId.map { current in
            client.channels.isEmpty || client.channels.contains { $0.id == current }
        } ?? false
        guard client.isConnected, !isInListedChannel else { return }
        guard let target = client.channels.first(where: { $0.isLobby })?.id
            ?? client.channels.first?.id
        else { return }
        switchChannel(to: target, client: client)
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
    private var isChatFirst: Bool { layoutMode == .chatFirst }

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
        let messages = computedChatMessages(client: client)
        let channel = currentChannel(for: client)
        let showsComment = channel.map { !$0.comment.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty } ?? false
        let showsBanner = !client.isConnected

        VStack(spacing: 0) {
            chatHeader(client: client, messages: messages)

            Divider().opacity(0.1)

            VStack(spacing: 0) {
                if showsBanner || showsComment {
                    VStack(spacing: 10) {
                        if showsBanner {
                            ConnectionBanner(
                                isRetrying: client.isRetrying,
                                status: client.connectionStatus,
                                onRetry: { client.retryConnectionNow() }
                            )
                            .transition(.move(edge: .top).combined(with: .opacity))
                        }
                        if showsComment, let channel {
                            ChannelCommentCard(
                                channel: channel,
                                isCollapsed: Binding(
                                    get: { collapsedComments.contains(channel.id) },
                                    set: { collapsed in
                                        if collapsed {
                                            collapsedComments.insert(channel.id)
                                        } else {
                                            collapsedComments.remove(channel.id)
                                        }
                                    }
                                )
                            )
                        }
                    }
                    .padding(.horizontal, isChatFirst ? 22 : 14)
                    .padding(.top, AuraTheme.Spacing.s12)
                }

                chatPanel(client: client, messages: messages)
            }
            .background(AuraTheme.Colors.background.opacity(0.5))
            .animation(.easeOut(duration: 0.2), value: showsBanner)
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
                setLocalVolume: { client.setLocalVolume(sessionId: $0, volume: $1) },
                verifyKey: { openTrustSheet(focusUuid: $0) }
            ),
            mode: layoutMode,
            stageTitle: AnyView(channelTitle(client: client))
        )
        .onAppear {
            // The store owns the sampling cadence; it just needs to know where
            // the level comes from. The hero is always someone else now, so the
            // waveform is always the mixed receive level — our own capture
            // level belongs to the HUD's input meter, not here.
            talkStore.levelProvider = { [weak client] in
                guard let client else { return 0 }
                return VoiceLevel.normalized(client.outputLevelDb)
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
            .flatMap { client.profile(forSession: $0)?.avatarData }
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
            let uuid = client.uuidForSession(user.id)
            participants.append(VoiceParticipant(
                id: user.id,
                displayName: user.displayName,
                avatarData: user.avatarData,
                isLocal: false,
                isMuted: user.isMuted,
                isDeafened: user.isDeafened,
                isDisconnected: user.isDisconnected,
                userUuid: uuid,
                trust: rosterTrust.entry(forUuid: uuid)?.state
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
        // Our session id is learned after connect, so keep the store in step
        // rather than setting it once — until it knows, it cannot tell our lane
        // apart from anyone else's and would put us back in the hero card.
        talkStore.localSessionId = client.sessionId

        var speaking = client.activeSpeakers
        if client.isLocalSpeaking, let sessionId = client.sessionId {
            speaking.insert(sessionId)
        }
        talkStore.setSpeaking(speaking)
    }

    // MARK: - Headers

    /// Channel icon, name and the `E2EE · epoch N` pill. Lives in whichever
    /// column is wide: the chat header in Chat-first, the stage header in
    /// Voice-focus (mocks 3c / 3a).
    @ViewBuilder
    private func channelTitle(client: QuicNetworkClient) -> some View {
        let channel = currentChannel(for: client)

        HStack(spacing: AuraTheme.Spacing.s8) {
            if let emoji = channel?.iconEmoji, !emoji.isEmpty {
                Text(emoji)
                    .font(.system(size: 15))
            } else {
                Image(systemName: channel?.iconPresetId ?? "speaker.wave.2")
                    .font(.system(size: 14, weight: .bold))
                    .foregroundStyle(AuraTheme.Colors.primary)
            }

            Text(channel?.name ?? "Channel")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t15, weight: .bold))
                .kerning(AuraTheme.Typography.titleKerning)
                .foregroundStyle(AuraTheme.Colors.text)
                .lineLimit(1)

            epochPill(client: client)
        }
    }

    /// `E2EE · epoch 14`. Clicking opens the trust sheet — the badge that
    /// claims the channel is encrypted is also the way to check that claim,
    /// rather than a reassurance with nothing behind it. The epoch is read
    /// from the text group on a slow tick; it moves on joins and ratchets.
    private func epochPill(client: QuicNetworkClient) -> some View {
        TimelineView(.periodic(from: .now, by: 2)) { _ in
            let channelId = client.currentChannelId
            let epoch = channelId.flatMap { try? client.mls?.currentEpoch(channelId: $0, isVoice: false) }

            Button { openTrustSheet() } label: {
                Text(ChatFormatting.epochLabel(epoch: epoch))
                    .font(AuraTheme.Typography.mono(10.5))
                    .foregroundStyle(AuraTheme.Colors.accent)
                    .lineLimit(1)
                    .fixedSize()
                    .padding(.horizontal, 7)
                    .padding(.vertical, AuraTheme.Spacing.s2)
                    .background(Capsule().fill(AuraTheme.Colors.accent.opacity(0.13)))
                    .overlay(Capsule().strokeBorder(AuraTheme.Colors.accent.opacity(0.30), lineWidth: 0.5))
            }
            .buttonStyle(.plain)
            .help("Verify who you are talking to")
            .accessibilityLabel("End-to-end encrypted. Verify identities")
            .auraFluidHover()
        }
    }

    @ViewBuilder
    private func chatHeader(client: QuicNetworkClient, messages: [ChatMessage]) -> some View {
        let today = ChatFormatting.countToday(
            messages.filter { $0.type != .info }.map(\.timestamp)
        )

        HStack(spacing: AuraTheme.Spacing.s8) {
            Group {
                if isChatFirst {
                    channelTitle(client: client)
                } else {
                    Text("Chat")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.text)
                }
            }
            .transition(.opacity)

            Spacer(minLength: AuraTheme.Spacing.s8)

            Text(isChatFirst ? ChatFormatting.messagesTodayLabel(count: today) : "encrypted")
                .font(AuraTheme.Typography.mono(10.5))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .lineLimit(1)

            inviteButton(client: client)

            layoutModeToggle
        }
        .animation(AuraTheme.Motion.crossFade, value: layoutMode)
        .padding(.horizontal, AuraTheme.Spacing.s16)
        .frame(height: AuraTheme.Layout.headerHeight)
        .padding(.top, 28) // Synced with sidebar header at 28
        .background(VisualEffectBlur(auraMaterial: .header, blendingMode: .withinWindow))
    }

    /// Opens the invite popover: an `aura://` link to this server and,
    /// optionally, this channel.
    private func inviteButton(client: QuicNetworkClient) -> some View {
        let channel = currentChannel(for: client)

        return Button { showingInvite.toggle() } label: {
            HStack(spacing: AuraTheme.Spacing.s5) {
                Image(systemName: "person.badge.plus")
                    .font(.system(size: 12, weight: .semibold))
                Text("Invite")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .semibold))
            }
            .foregroundStyle(AuraTheme.Colors.primary)
            .frame(height: 24)
            .padding(.horizontal, 6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .auraFluidHover()
        .disabled(client.connectedHost == nil)
        .help("Invite someone to this server")
        .accessibilityLabel("Invite")
        .popover(isPresented: $showingInvite, arrowEdge: .bottom) {
            if let host = client.connectedHost {
                InvitePopover(
                    host: host,
                    port: client.connectedPort ?? AuraInviteLink.defaultPort,
                    channelId: channel?.id,
                    channelName: channel?.name,
                    pinnedFingerprint: appSettings.pinnedFingerprint(forHost: host),
                    password: client.connectionPassword
                )
            }
        }
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
                .frame(width: 24, height: 24)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .auraFluidHover()
        .keyboardShortcut("v", modifiers: [.command, .option])
        .help("Switch to \(layoutMode.other.displayName) layout (⌘⌥V)")
        .accessibilityLabel("Switch to \(layoutMode.other.displayName) layout")
    }

    // MARK: - Chat

    /// The chat column had no empty state at all — a full-height void with the
    /// composer stranded at the bottom, which is the first thing every new user
    /// sees. The dead space now carries the one claim worth making here.
    private var chatEmptyState: some View {
        VStack(spacing: AuraTheme.Spacing.s8) {
            Text("No messages yet")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13, weight: .semibold))
                .foregroundStyle(AuraTheme.Colors.text)
            Text("Messages here are end-to-end encrypted. Your server relays ciphertext only.")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, AuraTheme.Spacing.s20)
        .padding(.vertical, AuraTheme.Spacing.s16)
        .frame(maxWidth: 360)
        .auraGlass(cornerRadius: AuraTheme.Radii.r20)
    }

    /// Id of the empty row after the last message, and the scroll view's
    /// coordinate space: together they tell whether the chat is at its end.
    private static let chatBottomId = "chat_bottom"
    nonisolated private static let chatScrollSpace = "chatScroll"

    private func chatPanel(client: QuicNetworkClient, messages: [ChatMessage]) -> some View {
        VStack(spacing: 0) {
            ScrollViewReader { scrollProxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: isChatFirst ? AuraTheme.Spacing.s16 : AuraTheme.Spacing.s12) {
                        ForEach(messages) { message in
                            if message.type == .info {
                                ChatInfoRow(text: message.content)
                                    .id(message.id)
                            } else {
                                ChatMessageRow(
                                    message: message,
                                    mode: layoutMode,
                                    onReply: { msg in
                                        replyingTo = msg
                                        isInputFocused = true
                                    },
                                    onRetry: { retrySend($0, client: client) },
                                    onJumpToReply: { id in
                                        withAnimation { scrollProxy.scrollTo(id, anchor: .center) }
                                    }
                                )
                                .id(message.id)
                            }
                        }

                        // At the end when this row is in (or just below) view.
                        Color.clear
                            .frame(height: 1)
                            .id(Self.chatBottomId)
                            .onGeometryChange(for: Bool.self) { proxy in
                                let frame = proxy.frame(in: .named(Self.chatScrollSpace))
                                let viewport = proxy.bounds(of: .named(Self.chatScrollSpace))?.height ?? 0
                                return frame.minY <= viewport + 60
                            } action: { atBottom in
                                setChatAtBottom(atBottom)
                            }
                            .onDisappear { setChatAtBottom(false) }
                    }
                    .padding(.horizontal, isChatFirst ? 22 : 14)
                    .padding(.vertical, AuraTheme.Spacing.s20)
                }
                .coordinateSpace(.named(Self.chatScrollSpace))
                // Join notices alone still leave the column looking unused, so
                // they do not count as content here.
                .overlay {
                    if !messages.contains(where: { $0.type != .info }) {
                        chatEmptyState
                    }
                }
                .overlay(alignment: .bottom) {
                    if !unseenMentionIds.isEmpty {
                        MentionsBelowPill(count: unseenMentionIds.count) {
                            let first = unseenMentionIds.removeFirst()
                            withAnimation { scrollProxy.scrollTo(first, anchor: .center) }
                        }
                        .padding(.bottom, AuraTheme.Spacing.s12)
                        .transition(.opacity.combined(with: .move(edge: .bottom)))
                    }
                }
                .animation(AuraTheme.Motion.crossFade, value: unseenMentionIds.isEmpty)
                .onAppear {
                    if let last = messages.last {
                        scrollProxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
                .onChange(of: messages.last?.id) { _, lastId in
                    guard let lastId else { return }
                    // Follow new messages only from the end of the chat (or
                    // when you sent one): someone reading back up stays put.
                    guard isChatAtBottom || messages.last?.isOutgoing == true else { return }
                    withAnimation {
                        scrollProxy.scrollTo(lastId, anchor: .bottom)
                    }
                }
            }

            if let replying = replyingTo {
                ChatReplyBar(
                    sender: replying.isOutgoing ? "you" : replying.senderName,
                    preview: replying.content,
                    onCancel: { replyingTo = nil }
                )
            }

            ChatComposer(
                text: $messageText,
                focus: $isInputFocused,
                placeholder: ChatFormatting.composerPlaceholder(channelName: currentChannel(for: client)?.name),
                showsShortcutHint: isChatFirst,
                onSend: { sendMessage(client: client) },
                onCancelReply: replyingTo == nil ? nil : { replyingTo = nil },
                mentionCandidates: mentionCandidates(client: client)
            )
            .padding(.horizontal, AuraTheme.Spacing.s16)
            .padding(.top, AuraTheme.Spacing.s8)
            .padding(.bottom, AuraTheme.Spacing.s16)
        }
        .onChange(of: client.receivedMessages) { oldValue, newValue in
            let known = Set(oldValue.map(\.id))
            let keywords = Mentions.keywords(from: notifyKeywords)
            let ownName = identity?.displayName ?? ""
            for msg in newValue where !known.contains(msg.id) && msg.senderSessionId != client.sessionId {
                let isMention = Mentions.mentionsYou(
                    msg.content, displayName: ownName, keywords: keywords, isOwnMessage: false)

                if msg.channelId == client.currentChannelId {
                    tts.speakMessage(sender: msg.senderName, content: msg.content)
                    if isMention && !isChatAtBottom {
                        unseenMentionIds.append(msg.id)
                    }
                }

                let channelName = client.channels.first { $0.id == msg.channelId }?.name ?? "Aura"
                MessageNotifier.shared.didReceive(
                    sender: msg.senderName, channel: channelName, text: msg.content, isMention: isMention)
            }
        }
        .onChange(of: client.currentChannelId) { _, _ in
            // Mentions below belong to the channel being left.
            unseenMentionIds = []
            isChatAtBottom = true
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

    private func setChatAtBottom(_ atBottom: Bool) {
        guard isChatAtBottom != atBottom else { return }
        isChatAtBottom = atBottom
        if atBottom {
            // Reaching the end means everything below has been seen.
            unseenMentionIds = []
        }
    }

    /// Names offered after `@` in the composer: the others in this channel.
    private func mentionCandidates(client: QuicNetworkClient) -> [String] {
        (client.usersByChannel[currentChannelId(for: client)] ?? [])
            .filter { !$0.isDisconnected && $0.id != client.sessionId }
            .map(\.displayName)
    }

    // Combined chat messages for the current channel (computed dynamically)
    private func computedChatMessages(client: QuicNetworkClient) -> [ChatMessage] {
        guard let currentChannelId = client.currentChannelId else { return [] }
        var messages: [ChatMessage] = []

        let localAvatar = client.sessionId
            .flatMap { client.profile(forSession: $0)?.avatarData }
            .flatMap { $0.isEmpty ? nil : $0 }
        let ownName = identity?.displayName ?? ""
        let keywords = Mentions.keywords(from: notifyKeywords)

        // Add text messages
        for msg in client.receivedMessages where msg.channelId == currentChannelId {
            let isOutgoing = msg.senderSessionId == client.sessionId
            var chatMsg = ChatMessage(
                id: msg.id,
                senderName: msg.senderName,
                content: msg.content,
                timestamp: msg.timestamp,
                isOutgoing: isOutgoing
            )
            chatMsg.mentionsYou = Mentions.mentionsYou(
                msg.content, displayName: ownName, keywords: keywords, isOwnMessage: isOutgoing)
            chatMsg.mentionName = isOutgoing ? nil : ownName
            chatMsg.channelId = msg.channelId
            chatMsg.senderSessionId = msg.senderSessionId
            chatMsg.avatarData = isOutgoing
                ? localAvatar
                : client.profile(forSession: msg.senderSessionId).map(\.avatarData).flatMap { $0.isEmpty ? nil : $0 }
            chatMsg.deliveryFailed = failedMessageIds.contains(msg.id)

            if let replyId = msg.replyToId,
               let originalMsg = client.receivedMessages.first(where: { $0.id == replyId }) {
                chatMsg.replyToId = replyId
                chatMsg.replyToSender = originalMsg.senderSessionId == client.sessionId ? "you" : originalMsg.senderName
                chatMsg.replyToPreview = String(originalMsg.content.prefix(80))
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

        // Add divider if we have chat history
        let currentMessages = computedChatMessages(client: client)
        if !currentMessages.isEmpty {
            let newChannelName = client.channels.first(where: { $0.id == channelId })?.name ?? "Unknown"
            let text = "Joined #\(newChannelName)"
            client.systemEvents.append(SystemEvent(content: text, channelId: channelId))
        }

        // A reply quotes a message from the channel being left.
        replyingTo = nil

        Task {
            do {
                try await client.joinChannel(channelId)
                print("[ContentView] Switched to channel \(channelId)")
            } catch {
                print("[ContentView] Failed to switch channel: \(error)")
                client.systemEvents.append(SystemEvent(
                    content: "Couldn't join that channel: \(error.localizedDescription)",
                    channelId: client.currentChannelId ?? "0"
                ))
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
                                client.sendAudioFrame(pcmData)
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
                    client.sendAudioFrame(pcmData)
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
                    client.sendAudioFrame(pcmData)
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
        replyingTo = nil
        failedMessageIds = []
        unseenMentionIds = []
        talkStore.reset()
    }

    // MARK: - Invites

    /// An `aura://` link opened from anywhere. Same server: go to the
    /// channel. Another server: ask before leaving this one. Not connected:
    /// the login screen, filled in from the link.
    private func handleOpenURL(_ url: URL) {
        let invite: AuraInviteLink
        do {
            invite = try AuraInviteLink(url: url)
        } catch {
            inviteError = error.localizedDescription
            return
        }

        guard isConnected, let client else {
            pendingInvite = invite
            return
        }

        if invite.matches(host: client.connectedHost ?? "", port: client.connectedPort ?? 0) {
            if let channelId = invite.channelId, client.channels.contains(where: { $0.id == channelId }) {
                switchChannel(to: channelId, client: client)
            }
        } else {
            inviteToConfirm = invite
        }
    }

    /// "Leave and Join": drop this server and follow the invite from the
    /// login screen.
    private func followInviteElsewhere(_ invite: AuraInviteLink) {
        disconnect()
        pendingInvite = invite
    }

    // MARK: - Leaving a server

    /// Delete this account on the server, then disconnect and forget the
    /// saved server entry. The certificate pin stays.
    private func leaveServer(client: QuicNetworkClient) {
        let host = client.connectedHost ?? ""
        let port = client.connectedPort ?? AuraInviteLink.defaultPort
        Task {
            do {
                try await client.deleteOwnAccount()
                disconnect()
                Self.forgetSavedServer(host: host, port: port)
                accountNotice = ChatFormatting.accountDeletedNotice(host: host)
            } catch {
                leaveError = error.localizedDescription
            }
        }
    }

    /// Remove saved server entries for `host`:`port`. Reads the list afresh
    /// rather than trusting this view's copy, which may be older than an
    /// edit made in the server list since.
    private static func forgetSavedServer(host: String, port: UInt16) {
        let manager = ServerManager()
        let target = AuraInviteLink.normalizedHost(host)
        for server in manager.servers
        where AuraInviteLink.normalizedHost(server.host) == target && server.port == port {
            manager.deleteServer(id: server.id)
        }
    }

    private func sendMessage(client: QuicNetworkClient) {
        let content = messageText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !content.isEmpty else { return }

        // Without a channel there is nowhere to put the message: the optimistic
        // copy would be filed under a placeholder id that the chat filter never
        // matches, so it would vanish on send. Say so instead of losing it.
        guard let channelId = client.currentChannelId else {
            client.systemEvents.append(
                SystemEvent(content: "Not in a channel yet — message not sent", channelId: "0"))
            return
        }

        let replying = replyingTo
        messageText = "" // Clear immediately for UX
        replyingTo = nil // Clear reply state
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
            timestamp: Date(),
            rawPacket: Data(),
            replyToId: replying?.id
        )
        client.receivedMessages.append(msg)

        deliver(messageId: messageId, content: content, replyToId: replying?.id, client: client)
    }

    /// Re-send a message that failed, under the same id, so a late success
    /// and the retry cannot show up as two messages.
    private func retrySend(_ message: ChatMessage, client: QuicNetworkClient) {
        guard failedMessageIds.contains(message.id),
              let original = client.receivedMessages.first(where: { $0.id == message.id })
        else { return }
        deliver(messageId: original.id, content: original.content, replyToId: original.replyToId, client: client)
    }

    private func deliver(messageId: String, content: String, replyToId: String?, client: QuicNetworkClient) {
        failedMessageIds.remove(messageId)
        Task {
            do {
                try await client.sendTextMessage(content, messageId: messageId, replyToId: replyToId)
            } catch {
                print("[ContentView] Failed to send message: \(error)")
                failedMessageIds.insert(messageId)
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

            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("Icon")
                    TextField("Emoji", text: $emoji)
                        .frame(width: 50)
                    Spacer()
                }

                TextField("Channel Name", text: $name)
                    .textFieldStyle(.roundedBorder)

                VStack(alignment: .leading, spacing: 4) {
                    Text("Comment")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    TextEditor(text: $comment)
                        .font(.system(size: 12))
                        .frame(height: 80)
                        .scrollContentBackground(.hidden)
                        .padding(4)
                        .background(RoundedRectangle(cornerRadius: 6).fill(Color.primary.opacity(0.05)))
                    Text("Markdown: **bold**, *italic*, `code`, [links](https://…), - lists, > quotes.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            .padding()
            .auraGlass()

            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.bordered)
                    .keyboardShortcut(.cancelAction)

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
                .keyboardShortcut(.defaultAction)
                .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(30)
        .frame(width: 380)
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
    var senderSessionId: UInt32?
    var avatarData: Data?
    /// The send threw; shown dimmed with a retry.
    var deliveryFailed = false
    /// Mentions you by name or keyword (see `Mentions`): caution tint and a
    /// `mentions you` label.
    var mentionsYou = false
    /// Your display name, whose `@` tokens are highlighted; nil on your own
    /// messages.
    var mentionName: String?

    // Reply-to threading
    var replyToId: String?
    var replyToSender: String?
    var replyToPreview: String?
}

enum MessageType: Equatable, Codable {
    case regular
    case args(String) // For future extensibility if needed, but for now simple cases
    case info // Divider/System message
}

#Preview {
    ContentView()
}
