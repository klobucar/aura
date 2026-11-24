import AuraCore
import SwiftUI

/// The admin moderation console: every account on the server, with the
/// actions the server offers on each (verify, move to lobby, ban, admin).
///
/// The server decides what an admin may do on every request, and the client
/// doesn't learn its own permission set, so every action shows and a refusal
/// is surfaced inline.
struct ModerationView: View {
    let client: QuicNetworkClient
    var initialSelection: String?

    @Environment(\.dismiss) private var dismiss

    @State private var query = ""
    @State private var filter: ModerationFilter = .all
    @State private var selection: String?
    @State private var banReason = ""
    @State private var confirmingBan = false
    @State private var showingGrant = false
    @State private var showingMove = false
    @State private var moveReason = ""

    // MARK: Derived

    private var members: [ModerationMember] { client.moderationMembers }

    private var onlineIds: Set<String> {
        Set(members.filter { client.moderationPresence(of: $0.id).isOnline }.map(\.id))
    }

    private var selected: ModerationMember? {
        members.first { $0.id == selection }
    }

    private var host: String {
        (client.connectedHost ?? "server").uppercased()
    }

    // MARK: Body

    var body: some View {
        let online = onlineIds
        let counts = ModerationLogic.counts(of: members, onlineIds: online)
        let visible = ModerationLogic.filter(members, by: filter, query: query, onlineIds: online)

        VStack(spacing: 0) {
            header

            Divider().opacity(0.1)

            HStack(spacing: 0) {
                VStack(alignment: .leading, spacing: AuraTheme.Spacing.s12) {
                    searchField
                    chips(counts: counts)
                    table(visible)
                }
                .padding(AuraTheme.Spacing.s16)
                .frame(maxWidth: .infinity)

                Divider().opacity(0.1)

                detailPanel
                    .frame(width: 330)
            }
        }
        .frame(minWidth: 820, idealWidth: 900, minHeight: 520, idealHeight: 600)
        .background(AuraTheme.Colors.background)
        .task {
            if let initialSelection { selection = initialSelection }
            await client.refreshModerationMembers()
        }
        .onChange(of: client.isAdmin) { _, isAdmin in
            if !isAdmin { dismiss() }
        }
        .onChange(of: selection) { _, _ in
            banReason = ""
        }
        .alert(
            banAlertTitle,
            isPresented: $confirmingBan,
            presenting: selected
        ) { member in
            Button("Ban \(member.name)", role: .destructive) {
                let reason = banReason
                Task { await client.banUser(member.id, banned: true, reason: reason) }
            }
            Button("Cancel", role: .cancel) {}
        } message: { member in
            Text("\(member.name) is disconnected now and can't log in again until you unban them.")
        }
        .alert("Move to Lobby", isPresented: $showingMove, presenting: selected) { member in
            TextField("Reason (shown to them)", text: $moveReason)
            Button("Move to Lobby") {
                let reason = moveReason
                moveReason = ""
                Task { await client.kickUser(member.id, reason: reason) }
            }
            Button("Cancel", role: .cancel) { moveReason = "" }
        } message: { member in
            Text("Moves \(member.name) out of their channel. They can rejoin.")
        }
        .sheet(isPresented: $showingGrant) {
            if let member = selected {
                AdminPermissionsSheet(member: member) { permissions in
                    Task { await client.grantAdmin(member.id, permissions: permissions) }
                }
            }
        }
    }

    private var banAlertTitle: String {
        "Ban \(selected?.name ?? "member")?"
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .center) {
            VStack(alignment: .leading, spacing: AuraTheme.Spacing.s3) {
                Text("\(host) · ADMIN")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .kerning(AuraTheme.Typography.monoLabelKerning)
                    .foregroundStyle(AuraTheme.Colors.textDim)
                Text("Server members")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t19, weight: .bold))
                    .kerning(AuraTheme.Typography.titleKerning)
                    .foregroundStyle(AuraTheme.Colors.text)
            }
            Spacer()
            if client.moderationIsLoading {
                ProgressView().controlSize(.small)
            }
            Button {
                Task { await client.refreshModerationMembers() }
            } label: {
                Image(systemName: "arrow.clockwise")
                    .foregroundStyle(AuraTheme.Colors.textMid)
            }
            .buttonStyle(.plain)
            .help("Refresh")
            .accessibilityLabel("Refresh members")
            .disabled(client.moderationIsLoading)

            Button { dismiss() } label: {
                Image(systemName: "xmark")
                    .foregroundStyle(AuraTheme.Colors.textMid)
            }
            .buttonStyle(.plain)
            .keyboardShortcut(.cancelAction)
            .help("Close")
            .accessibilityLabel("Close")
            .padding(.leading, AuraTheme.Spacing.s8)
        }
        .padding(.horizontal, AuraTheme.Spacing.s20)
        .padding(.vertical, AuraTheme.Spacing.s14)
    }

    // MARK: Search and chips

    private var searchField: some View {
        HStack(spacing: AuraTheme.Spacing.s8) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(AuraTheme.Colors.textDim)
            TextField("Search by name or user id", text: $query)
                .textFieldStyle(.plain)
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13))
                .accessibilityLabel("Search members")
        }
        .padding(.horizontal, AuraTheme.Spacing.s12)
        .padding(.vertical, AuraTheme.Spacing.s8)
        .auraPanel(cornerRadius: AuraTheme.Radii.r10)
    }

    private func chips(counts: [ModerationFilter: Int]) -> some View {
        HStack(spacing: AuraTheme.Spacing.s8) {
            ForEach(ModerationFilter.allCases) { chip in
                let isOn = chip == filter
                Button { filter = chip } label: {
                    Text("\(chip.title) · \(counts[chip] ?? 0)")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11, weight: .medium))
                        .foregroundStyle(isOn ? AuraTheme.Colors.text : AuraTheme.Colors.textMid)
                        .padding(.horizontal, AuraTheme.Spacing.s11)
                        .padding(.vertical, AuraTheme.Spacing.s5)
                        .background {
                            Capsule().fill(isOn ? AuraTheme.Colors.primary.opacity(0.22) : Color.clear)
                        }
                        .overlay {
                            Capsule().strokeBorder(
                                isOn ? AuraTheme.Colors.primary.opacity(0.5) : AuraTheme.Colors.panelBorder,
                                lineWidth: 0.5)
                        }
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(isOn ? .isSelected : [])
            }
            Spacer()
        }
    }

    // MARK: Table

    private func table(_ rows: [ModerationMember]) -> some View {
        VStack(spacing: 0) {
            HStack {
                columnLabel("MEMBER").frame(maxWidth: .infinity, alignment: .leading)
                columnLabel("STANDING").frame(width: 100, alignment: .leading)
                columnLabel("STATUS").frame(width: 120, alignment: .leading)
            }
            .padding(.horizontal, AuraTheme.Spacing.s12)
            .padding(.vertical, AuraTheme.Spacing.s8)

            Divider().opacity(0.1)

            if rows.isEmpty {
                Text(client.moderationIsLoading ? "Loading…" : "No members match.")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.textDim)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(rows) { member in
                            row(member)
                        }
                    }
                }
            }
        }
        .auraPanel(cornerRadius: AuraTheme.Radii.r14)
        .frame(maxHeight: .infinity)
    }

    private func columnLabel(_ text: String) -> some View {
        Text(text)
            .font(AuraTheme.Typography.mono(9))
            .kerning(AuraTheme.Typography.monoLabelKerning)
            .foregroundStyle(AuraTheme.Colors.textDim)
    }

    private func row(_ member: ModerationMember) -> some View {
        let presence = client.moderationPresence(of: member.id)
        let isSelected = member.id == selection
        return Button { selection = member.id } label: {
            HStack {
                HStack(spacing: AuraTheme.Spacing.s9) {
                    AuraAvatar(name: member.name, data: nil, size: 28, dimmed: member.banned)
                    VStack(alignment: .leading, spacing: AuraTheme.Spacing.s2) {
                        Text(member.name)
                            .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13, weight: .medium))
                            .foregroundStyle(member.banned ? AuraTheme.Colors.textDim : AuraTheme.Colors.text)
                            .lineLimit(1)
                        Text(member.shortId)
                            .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                            .foregroundStyle(AuraTheme.Colors.textFaint)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                StandingBadge(standing: member.standing)
                    .frame(width: 100, alignment: .leading)

                HStack(spacing: AuraTheme.Spacing.s5) {
                    Circle()
                        .fill(presence.isOnline ? AuraTheme.Colors.good : AuraTheme.Colors.textFaint)
                        .frame(width: 6, height: 6)
                    Text(presence.label)
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11))
                        .foregroundStyle(AuraTheme.Colors.textMid)
                        .lineLimit(1)
                }
                .frame(width: 120, alignment: .leading)
            }
            .padding(.horizontal, AuraTheme.Spacing.s12)
            .padding(.vertical, AuraTheme.Spacing.s8)
            .background(isSelected ? AuraTheme.Colors.primary.opacity(0.14) : Color.clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(member.name), \(member.standing.label), \(presence.label)")
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    // MARK: Detail

    @ViewBuilder
    private var detailPanel: some View {
        if let member = selected {
            detail(for: member)
        } else {
            VStack {
                Text("Select a member")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.textDim)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func detail(for member: ModerationMember) -> some View {
        let presence = client.moderationPresence(of: member.id)
        let isSelf = member.id == client.userUuid
        let busy = client.moderationBusy

        return ScrollView {
            VStack(alignment: .leading, spacing: AuraTheme.Spacing.s14) {
                HStack(spacing: AuraTheme.Spacing.s12) {
                    AuraAvatar(name: member.name, data: nil, size: 46, dimmed: member.banned)
                    VStack(alignment: .leading, spacing: AuraTheme.Spacing.s3) {
                        Text(member.name)
                            .font(AuraTheme.Typography.ui(AuraTheme.Typography.t15, weight: .bold))
                            .kerning(AuraTheme.Typography.titleKerning)
                            .foregroundStyle(AuraTheme.Colors.text)
                            .lineLimit(2)
                        Text("\(ModerationLogic.joinedLabel(member.createdAt)) · \(presence.label)")
                            .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                            .foregroundStyle(AuraTheme.Colors.textDim)
                        Text(member.id)
                            .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                            .foregroundStyle(AuraTheme.Colors.textFaint)
                            .textSelection(.enabled)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }

                StandingBadge(standing: member.standing)

                Text("Server verification is the server vouching for this account. It's separate from comparing safety words with them, which is how you check their key yourself.")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.textMid)
                    .fixedSize(horizontal: false, vertical: true)

                if isSelf {
                    Text("This is your account. The server won't let you act on it.")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11))
                        .foregroundStyle(AuraTheme.Colors.cautionText)
                }

                VStack(spacing: AuraTheme.Spacing.s8) {
                    actionButton("Verify account", systemImage: "checkmark.seal") {
                        Task { await client.verifyUser(member.id) }
                    }
                    .disabled(busy || member.verified || member.banned)

                    actionButton("Move to lobby", systemImage: "arrow.uturn.backward") {
                        showingMove = true
                    }
                    .disabled(busy || isSelf || !presence.canMoveToLobby)
                    .help(presence.canMoveToLobby
                          ? "Take them out of their channel"
                          : "They need to be online in a channel other than the lobby")
                }

                banSection(member, isSelf: isSelf, busy: busy)

                if let error = client.moderationError {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                        .foregroundStyle(AuraTheme.Colors.danger)
                        .padding(AuraTheme.Spacing.s9)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(
                            RoundedRectangle(cornerRadius: AuraTheme.Radii.r10, style: .continuous)
                                .fill(AuraTheme.Colors.dangerFill))
                        .overlay(
                            RoundedRectangle(cornerRadius: AuraTheme.Radii.r10, style: .continuous)
                                .strokeBorder(AuraTheme.Colors.dangerBorder, lineWidth: 0.5))
                        .accessibilityLabel("Error: \(error)")
                } else if let notice = client.moderationNotice {
                    Label(notice, systemImage: "checkmark.circle.fill")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                        .foregroundStyle(AuraTheme.Colors.good)
                }

                Divider().opacity(0.1)

                Button { showingGrant = true } label: {
                    Text(member.isAdmin ? "Admin permissions…" : "Make admin…")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .medium))
                        .foregroundStyle(AuraTheme.Colors.textMid)
                }
                .buttonStyle(.plain)
                .disabled(busy || isSelf)
            }
            .padding(AuraTheme.Spacing.s16)
        }
    }

    private func banSection(_ member: ModerationMember, isSelf: Bool, busy: Bool) -> some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s8) {
            if !member.banned {
                Text("REASON (SERVER LOG ONLY)")
                    .font(AuraTheme.Typography.mono(9))
                    .kerning(AuraTheme.Typography.monoLabelKerning)
                    .foregroundStyle(AuraTheme.Colors.textDim)
                TextField("Reason (server log only)", text: $banReason)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Ban reason, server log only")
            }

            if member.banned {
                actionButton("Unban \(member.name)", systemImage: "arrow.uturn.left.circle") {
                    Task { await client.banUser(member.id, banned: false, reason: banReason) }
                }
                .disabled(busy || isSelf)
            } else {
                Button { confirmingBan = true } label: {
                    Label("Ban \(member.name)", systemImage: "nosign")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.danger)
                        .lineLimit(1)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, AuraTheme.Spacing.s8)
                        .background(
                            RoundedRectangle(cornerRadius: AuraTheme.Radii.r10, style: .continuous)
                                .fill(AuraTheme.Colors.dangerFill))
                        .overlay(
                            RoundedRectangle(cornerRadius: AuraTheme.Radii.r10, style: .continuous)
                                .strokeBorder(AuraTheme.Colors.dangerBorder, lineWidth: 0.5))
                }
                .buttonStyle(.plain)
                .disabled(busy || isSelf)
                .opacity(busy || isSelf ? 0.5 : 1)
            }
        }
    }

    private func actionButton(_ title: String, systemImage: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .semibold))
                .lineLimit(1)
                .frame(maxWidth: .infinity)
                .padding(.vertical, AuraTheme.Spacing.s8)
        }
        .buttonStyle(.bordered)
    }
}

// MARK: - Standing badge

private struct StandingBadge: View {
    let standing: ModerationStanding

    private var color: Color {
        switch standing {
        case .admin: return AuraTheme.Colors.primary
        case .verified: return AuraTheme.Colors.good
        case .unverified: return AuraTheme.Colors.cautionText
        case .banned: return AuraTheme.Colors.danger
        }
    }

    var body: some View {
        Text(standing.label)
            .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10, weight: .medium))
            .foregroundStyle(color)
            .padding(.horizontal, AuraTheme.Spacing.s8)
            .padding(.vertical, AuraTheme.Spacing.s2)
            .background(Capsule().fill(color.opacity(0.14)))
            .overlay(Capsule().strokeBorder(color.opacity(0.38), lineWidth: 0.5))
            .fixedSize()
    }
}

// MARK: - Admin permissions

/// Four toggles, all off to start: the client doesn't learn what an account
/// already holds. The server replaces the target's permissions with these, and
/// none at all revokes admin.
private struct AdminPermissionsSheet: View {
    let member: ModerationMember
    var onApply: (AdminPermissionsRecord) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var verifyUsers = false
    @State private var banUsers = false
    @State private var grantAdmin = false
    @State private var manageChannels = false

    private var permissions: AdminPermissionsRecord {
        AdminPermissionsRecord(
            verifyUsers: verifyUsers, banUsers: banUsers,
            grantAdmin: grantAdmin, manageChannels: manageChannels)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s14) {
            Text("Admin permissions for \(member.name)")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t15, weight: .bold))
                .foregroundStyle(AuraTheme.Colors.text)

            Text("This replaces what they have now. Their current permissions aren't shown here, so start from what you want them to have. Turning everything off removes admin.")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                .foregroundStyle(AuraTheme.Colors.textMid)
                .fixedSize(horizontal: false, vertical: true)

            Toggle("Verify users", isOn: $verifyUsers)
            Toggle("Ban, unban and move users", isOn: $banUsers)
            Toggle("Grant admin and act on other admins", isOn: $grantAdmin)
            Toggle("Manage channels", isOn: $manageChannels)

            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(ModerationLogic.grantButtonTitle(isAdmin: member.isAdmin, permissions: permissions)) {
                    onApply(permissions)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!member.isAdmin && !ModerationLogic.hasAny(permissions))
            }
        }
        .padding(AuraTheme.Spacing.s20)
        .frame(width: 400)
    }
}

// MARK: - Move to lobby from the roster

/// The reason prompt behind a roster "Move to Lobby…". A modifier of its own
/// so ContentView's already long modifier chain stays type-checkable.
private struct MoveToLobbyAlert: ViewModifier {
    @Binding var pendingUuid: String?
    let client: QuicNetworkClient?
    @State private var reason = ""

    func body(content: Content) -> some View {
        content.alert(
            "Move to Lobby",
            isPresented: Binding(
                get: { pendingUuid != nil },
                set: { if !$0 { pendingUuid = nil } }
            ),
            presenting: pendingUuid
        ) { uuid in
            TextField("Reason (shown to them)", text: $reason)
            Button("Move to Lobby") {
                let text = reason
                reason = ""
                pendingUuid = nil
                Task { await client?.kickUser(uuid, reason: text) }
            }
            Button("Cancel", role: .cancel) {
                reason = ""
                pendingUuid = nil
            }
        } message: { uuid in
            Text("Moves \(client?.displayName(forUuid: uuid) ?? "this member") out of their channel and into the lobby. They can rejoin.")
        }
    }
}

extension View {
    func moveToLobbyAlert(pendingUuid: Binding<String?>, client: QuicNetworkClient?) -> some View {
        modifier(MoveToLobbyAlert(pendingUuid: pendingUuid, client: client))
    }
}
