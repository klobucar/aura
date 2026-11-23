import AuraCore
import SwiftUI

/// Callbacks from the roster back into `ContentView`, which owns the sheets
/// and the channel-switching logic.
struct RosterActions {
    var selectChannel: (String) -> Void
    var editProfile: () -> Void
    var disconnect: () -> Void
    var openSettings: () -> Void
    var createChannel: () -> Void
    var editChannel: (ChannelModel) -> Void
    var deleteChannel: (ChannelModel) -> Void
    /// Open the trust sheet, optionally focused on a user UUID.
    var compareKeys: (String?) -> Void
    /// Ask to delete this account on the server ("Leave server…").
    var leaveServer: () -> Void
}

/// One occupant row, flattened from `ChannelUser` (or the local user) plus the
/// state that only the client and the trust model know.
struct RosterMember: Identifiable {
    let id: UInt32
    let uuid: String?
    let displayName: String
    let bio: String
    let avatarData: Data?
    let isLocal: Bool
    let isMuted: Bool
    let isDeafened: Bool
    let isDisconnected: Bool
    let isSpeaking: Bool
    let isLocallyMuted: Bool
    let localVolume: Float
    let trust: TrustState?
    let identityKey: Data?

    var cardSubject: ProfileCardSubject {
        ProfileCardSubject(
            sessionId: id,
            uuid: uuid,
            displayName: displayName,
            bio: bio,
            avatarData: avatarData,
            isLocal: isLocal,
            trust: trust,
            identityKey: identityKey
        )
    }
}

// MARK: - Sidebar

/// Roster sidebar (handoff §1) — identical in both layout modes, and the only
/// place the whole server is listed. Every channel is expanded with its
/// occupants, including channels you are not in: the Mumble affordance.
struct RosterSidebar: View {
    let client: QuicNetworkClient
    @ObservedObject var identity: UserIdentity
    @ObservedObject var rosterTrust: RosterTrustModel
    let currentChannelId: String
    let isMicEnabled: Bool
    let isDeafened: Bool
    let actions: RosterActions

    var body: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s8) {
            titleRow

            IdentityCard(
                name: identity.displayName,
                avatarData: localAvatar,
                shortFingerprint: shortFingerprint,
                actions: actions
            )

            ScrollView {
                LazyVStack(alignment: .leading, spacing: AuraTheme.Spacing.s3) {
                    sectionLabel

                    ForEach(client.channels) { channel in
                        RosterChannelBlock(
                            channel: channel,
                            isCurrent: channel.id == currentChannelId,
                            members: members(in: channel),
                            isAdmin: client.isAdmin,
                            client: client,
                            actions: actions
                        )
                    }
                }
                .padding(.bottom, AuraTheme.Spacing.s8)
            }
            .scrollIndicators(.never)

            settingsButton
        }
        .padding(.horizontal, 10)
        .padding(.bottom, AuraTheme.Spacing.s12)
    }

    // MARK: Pieces

    private var titleRow: some View {
        HStack {
            Spacer()
            Text("Aura")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t15, weight: .bold))
                .kerning(AuraTheme.Typography.titleKerning)
                .foregroundStyle(AuraTheme.Colors.text)
        }
        .padding(.horizontal, 6)
        .frame(height: AuraTheme.Layout.headerHeight)
        .padding(.top, 28) // Synced with the channel and voice headers
    }

    /// `ROSTER · 7 ONLINE`, with the admin's create-channel button.
    private var sectionLabel: some View {
        HStack {
            Text(ChatFormatting.rosterLabel(online: onlineCount))
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .kerning(AuraTheme.Typography.monoLabelKerning)
                .foregroundStyle(AuraTheme.Colors.textDim)
            Spacer()
            if client.isAdmin {
                Button(action: actions.createChannel) {
                    Image(systemName: "plus")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                        .frame(width: 18, height: 18)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Create channel")
                .accessibilityLabel("Create channel")
            }
        }
        .padding(.top, 6)
        .padding(.horizontal, AuraTheme.Spacing.s8)
        .padding(.bottom, AuraTheme.Spacing.s2)
    }

    private var settingsButton: some View {
        Button(action: actions.openSettings) {
            HStack(spacing: 7) {
                Image(systemName: "gearshape.fill")
                    .font(.system(size: 12, weight: .semibold))
                Text("Settings")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .semibold))
            }
            .foregroundStyle(AuraTheme.Colors.text)
            .frame(maxWidth: .infinity)
            .padding(10)
            .auraGlass(cornerRadius: AuraTheme.Radii.r18)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .auraFluidHover()
        .keyboardShortcut(",", modifiers: .command)
    }

    // MARK: Derived

    private var localAvatar: Data? {
        client.sessionId
            .flatMap { client.profile(forSession: $0)?.avatarData }
            .flatMap { $0.isEmpty ? nil : $0 }
    }

    private var shortFingerprint: String? {
        guard let key = identity.identityKey else { return nil }
        return try? identityFingerprintShort(publicKey: key)
    }

    /// Everyone connected, you included. Users lingering as "LEFT" are gone.
    private var onlineCount: Int {
        let others = client.usersByChannel.values.reduce(0) { total, users in
            total + users.filter { !$0.isDisconnected }.count
        }
        return others + (client.sessionId == nil ? 0 : 1)
    }

    private func members(in channel: ChannelModel) -> [RosterMember] {
        var result: [RosterMember] = (client.usersByChannel[channel.id] ?? []).map { user in
            let uuid = client.uuidForSession(user.id)
            let entry = rosterTrust.entry(forUuid: uuid)
            return RosterMember(
                id: user.id,
                uuid: uuid,
                displayName: user.displayName,
                bio: user.bio,
                avatarData: user.avatarData,
                isLocal: false,
                isMuted: user.isMuted,
                isDeafened: user.isDeafened,
                isDisconnected: user.isDisconnected,
                isSpeaking: client.activeSpeakers.contains(user.id),
                isLocallyMuted: client.isLocallyMuted(sessionId: user.id),
                localVolume: client.localVolume(for: user.id),
                trust: entry?.state,
                identityKey: entry?.identityKey
            )
        }

        // You appear in the channel you're in, under your own name, like
        // everyone else — the mock lists "you" in the room, not only in the card.
        if channel.id == currentChannelId, let sessionId = client.sessionId {
            result.append(RosterMember(
                id: sessionId,
                uuid: client.userUuid,
                displayName: identity.displayName,
                bio: client.profile(forSession: sessionId)?.bio ?? "",
                avatarData: localAvatar,
                isLocal: true,
                isMuted: !isMicEnabled,
                isDeafened: isDeafened,
                isDisconnected: false,
                isSpeaking: client.isLocalSpeaking,
                isLocallyMuted: false,
                localVolume: 1,
                trust: .verified,
                identityKey: identity.identityKey
            ))
        }
        return result
    }
}

// MARK: - Identity card

private struct IdentityCard: View {
    let name: String
    let avatarData: Data?
    let shortFingerprint: String?
    let actions: RosterActions

    var body: some View {
        HStack(spacing: 10) {
            AuraAvatar(name: name, data: avatarData, size: 34)

            VStack(alignment: .leading, spacing: AuraTheme.Spacing.s2) {
                Text(name)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13, weight: .semibold))
                    .foregroundStyle(AuraTheme.Colors.text)
                    .lineLimit(1)
                Text(ChatFormatting.identityLine(shortFingerprint: shortFingerprint))
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.accent)
                    .lineLimit(1)
                    .help("Your identity key fingerprint")
            }

            Spacer(minLength: 0)

            Menu {
                Button("Edit Profile…", systemImage: "pencil", action: actions.editProfile)
                Button("Verify Identities…", systemImage: "checkmark.shield") { actions.compareKeys(nil) }
                Divider()
                Button("Disconnect", systemImage: "rectangle.portrait.and.arrow.right", action: actions.disconnect)
                Button("Leave server…", systemImage: "person.crop.circle.badge.xmark", role: .destructive, action: actions.leaveServer)
            } label: {
                Image(systemName: "ellipsis.circle")
                    .font(.system(size: 14))
                    .foregroundStyle(AuraTheme.Colors.textDim)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Profile and connection")
            .accessibilityLabel("Profile and connection")
        }
        .padding(.horizontal, AuraTheme.Spacing.s12)
        .padding(.vertical, 10)
        .auraGlass(cornerRadius: AuraTheme.Radii.r18)
    }
}

// MARK: - Channel block

private struct RosterChannelBlock: View {
    let channel: ChannelModel
    let isCurrent: Bool
    let members: [RosterMember]
    let isAdmin: Bool
    let client: QuicNetworkClient
    let actions: RosterActions

    private var occupancy: Int {
        members.filter { !$0.isDisconnected }.count
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            channelRow

            ForEach(members) { member in
                RosterMemberRow(member: member, client: client, actions: actions)
            }
        }
        .padding(.bottom, AuraTheme.Spacing.s5)
    }

    private var channelRow: some View {
        let shape = RoundedRectangle(cornerRadius: AuraTheme.Radii.r14, style: .continuous)

        return Button { actions.selectChannel(channel.id) } label: {
            HStack(spacing: AuraTheme.Spacing.s8) {
                channelIcon
                    .frame(width: 18)

                Text(channel.name)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13, weight: isCurrent ? .semibold : .regular))
                    .foregroundStyle(isCurrent ? AuraTheme.Colors.text : AuraTheme.Colors.textMid)
                    .lineLimit(1)

                Spacer(minLength: 4)

                if occupancy > 0 {
                    Text("\(occupancy)")
                        .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                        .padding(.horizontal, 7)
                        .padding(.vertical, AuraTheme.Spacing.s2)
                        .background(Capsule().fill(Color.white.opacity(0.08)))
                }
            }
            .padding(.horizontal, 11)
            .padding(.vertical, AuraTheme.Spacing.s8)
            .background {
                if isCurrent {
                    shape.fill(AuraTheme.Colors.primary.opacity(0.20))
                }
            }
            .overlay {
                if isCurrent {
                    shape.strokeBorder(AuraTheme.Colors.primary.opacity(0.32), lineWidth: 1)
                    // Inset top highlight.
                    shape.strokeBorder(
                        LinearGradient(
                            colors: [Color.white.opacity(0.35), .clear],
                            startPoint: .top,
                            endPoint: UnitPoint(x: 0.5, y: 0.3)
                        ),
                        lineWidth: 1
                    )
                }
            }
            .contentShape(shape)
        }
        .buttonStyle(.plain)
        .auraFluidHover()
        .help(channelHelp)
        .accessibilityLabel("\(channel.name), \(occupancy) in channel\(isCurrent ? ", current" : "")")
        .contextMenu {
            if isAdmin {
                Button("Edit Channel", systemImage: "pencil") { actions.editChannel(channel) }
                Divider()
                Button("Delete Channel", systemImage: "trash", role: .destructive) { actions.deleteChannel(channel) }
            }
        }
    }

    @ViewBuilder
    private var channelIcon: some View {
        if let emoji = channel.iconEmoji, !emoji.isEmpty {
            Text(emoji)
                .font(.system(size: 13))
        } else {
            Image(systemName: channel.iconPresetId ?? "speaker.wave.2")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(isCurrent ? AuraTheme.Colors.primary : AuraTheme.Colors.textDim)
        }
    }

    private var channelHelp: String {
        var parts: [String] = []
        if channel.isLobby { parts.append("Lobby") }
        if !channel.comment.isEmpty { parts.append(channel.comment) }
        return parts.isEmpty ? channel.name : parts.joined(separator: " — ")
    }
}

// MARK: - Member row

private struct RosterMemberRow: View {
    let member: RosterMember
    let client: QuicNetworkClient
    let actions: RosterActions

    @State private var showingCard = false

    var body: some View {
        Button { showingCard = true } label: {
            HStack(spacing: AuraTheme.Spacing.s8) {
                ZStack {
                    AuraAvatar(name: member.displayName, data: member.avatarData, size: 20, dimmed: member.isDisconnected)
                    if member.isDisconnected {
                        Image(systemName: "xmark")
                            .font(.system(size: 8, weight: .black))
                            .foregroundStyle(.white)
                    }
                }

                Text(member.displayName)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: member.isSpeaking ? .medium : .regular))
                    .foregroundStyle(nameColor)
                    .lineLimit(1)

                Spacer(minLength: 4)

                trailing
            }
            .padding(.horizontal, AuraTheme.Spacing.s8)
            .padding(.vertical, AuraTheme.Spacing.s3)
            .background {
                if member.isSpeaking && !member.isDisconnected {
                    RoundedRectangle(cornerRadius: AuraTheme.Radii.r10, style: .continuous)
                        .fill(AuraTheme.Colors.accent.opacity(0.09))
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.leading, AuraTheme.Spacing.s12)
        .opacity(member.isDisconnected ? 0.5 : 1)
        .animation(.easeOut(duration: 0.15), value: member.isSpeaking)
        .help(member.isLocal ? "You" : member.displayName)
        .accessibilityLabel(accessibilityText)
        .popover(isPresented: $showingCard, arrowEdge: .trailing) {
            ProfileCardView(
                subject: member.cardSubject,
                client: client,
                compareKeys: { uuid in
                    showingCard = false
                    actions.compareKeys(uuid)
                },
                editProfile: {
                    showingCard = false
                    actions.editProfile()
                }
            )
        }
        .contextMenu {
            if !member.isLocal && !member.isDisconnected {
                Button(member.isLocallyMuted ? "Unmute Locally" : "Mute Locally") {
                    client.toggleLocalMute(sessionId: member.id)
                }
                Menu("Volume") {
                    ForEach([0, 25, 50, 75, 100, 125, 150, 200], id: \.self) { pct in
                        Button("\(pct)%\(Int((member.localVolume * 100).rounded()) == pct ? " ✓" : "")") {
                            client.setLocalVolume(sessionId: member.id, volume: Float(pct) / 100.0)
                        }
                    }
                }
                if member.trust != nil {
                    Divider()
                    Button("Compare Keys…") { actions.compareKeys(member.uuid) }
                }
            }
        }
    }

    private var nameColor: Color {
        if member.isDisconnected { return AuraTheme.Colors.textFaint }
        return member.isSpeaking ? AuraTheme.Colors.text : AuraTheme.Colors.textMid
    }

    @ViewBuilder
    private var trailing: some View {
        HStack(spacing: AuraTheme.Spacing.s5) {
            if member.isDisconnected {
                Text("LEFT")
                    .font(AuraTheme.Typography.mono(9))
                    .foregroundStyle(AuraTheme.Colors.textFaint)
            } else {
                if !member.isLocal && abs(member.localVolume - 1.0) >= 0.01 && !member.isLocallyMuted {
                    Text("\(Int((member.localVolume * 100).rounded()))%")
                        .font(AuraTheme.Typography.mono(9))
                        .foregroundStyle(AuraTheme.Colors.textFaint)
                        .help("Local volume")
                }

                if member.isLocallyMuted {
                    glyph("speaker.slash.fill", AuraTheme.Colors.warn, help: "Muted locally")
                } else if member.isDeafened {
                    glyph("headphones.slash", AuraTheme.Colors.warn, help: "Deafened")
                } else if member.isMuted {
                    glyph("mic.slash.fill", AuraTheme.Colors.warn, help: "Muted")
                }

                switch member.trust {
                case .keyChanged:
                    glyph("key.fill", AuraTheme.Colors.caution, help: "Identity key changed — compare keys")
                case .tofu:
                    glyph("key.fill", AuraTheme.Colors.caution.opacity(0.7), help: "Key not compared yet")
                case .blocked:
                    glyph("nosign", AuraTheme.Colors.danger, help: "Blocked key")
                case .verified, nil:
                    EmptyView()
                }

                if member.isSpeaking {
                    Circle()
                        .fill(AuraTheme.Colors.accent)
                        .frame(width: 6, height: 6)
                        .shadow(color: AuraTheme.Colors.accent.opacity(0.8), radius: 4)
                        .transition(.scale.combined(with: .opacity))
                }
            }
        }
    }

    private func glyph(_ name: String, _ color: Color, help: String) -> some View {
        Image(systemName: name)
            .font(.system(size: 10))
            .foregroundStyle(color)
            .help(help)
    }

    private var accessibilityText: String {
        var parts = [member.isLocal ? "\(member.displayName), you" : member.displayName]
        if member.isSpeaking { parts.append("speaking") }
        if member.isDeafened { parts.append("deafened") } else if member.isMuted { parts.append("muted") }
        if member.trust == .keyChanged { parts.append("identity key changed") }
        return parts.joined(separator: ", ")
    }
}
