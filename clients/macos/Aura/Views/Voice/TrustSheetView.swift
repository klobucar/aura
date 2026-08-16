import SwiftUI
import Combine

/// One row of the sheet's MEMBERS list.
struct TrustMember: Identifiable, Equatable {
    let id: String          // the user's UUID
    let publicKey: Data
    let isSelf: Bool
    var name: String
    var trust: TrustState
    var shortFingerprint: String

    /// The trailing label. The handoff allows exactly three readings plus a
    /// block, each phrased to say what the user can act on rather than what the
    /// crypto did.
    var stateLabel: String {
        switch trust {
        case .verified:   return "verified · \(shortFingerprint)"
        case .keyChanged: return "key changed"
        case .blocked:    return "blocked"
        case .tofu:       return "TOFU · not compared"
        }
    }

    var stateColor: Color {
        switch trust {
        case .verified:   return AuraTheme.Colors.accent
        case .keyChanged: return AuraTheme.Colors.cautionText
        case .blocked:    return AuraTheme.Colors.danger
        case .tofu:       return AuraTheme.Colors.textDim
        }
    }
}

/// State behind the trust sheet (§7).
///
/// Every key here comes from `MlsWrapper.groupMembers` — the authenticated
/// ratchet tree — never from the server's user list. Display names are only
/// ever labels for a key, never the thing being trusted.
@MainActor
final class TrustSheetModel: ObservableObject {
    @Published var isPresented = false
    @Published var channelName = ""
    @Published var securityLine = ""
    @Published var members: [TrustMember] = []
    @Published var selectedId: String?
    @Published var safetyWords: [String] = []
    @Published var localFingerprint = ""
    @Published var remoteFingerprint = ""
    @Published var pairLabel = ""
    @Published var isDeriving = false
    @Published var error: String?

    private let trust: TrustStore

    /// Resolves an MLS credential identity to a roster name. Set by the view
    /// before opening rather than injected at init — the network client does
    /// not exist yet when this model is constructed.
    var nameForUuid: (String) -> String? = { _ in nil }

    private var localKey = Data()
    private var localUuid = ""

    /// Bumped on every selection so a slow derivation can tell it has been
    /// superseded; a stale result would show one member's words under another's
    /// name.
    private var derivationToken = 0

    init(trust: TrustStore = TrustStore()) {
        self.trust = trust
    }

    var selected: TrustMember? {
        members.first { $0.id == selectedId }
    }

    /// The amber banner, shown only when the selected member's key moved.
    var showsKeyChangedBanner: Bool {
        selected?.trust == .keyChanged
    }

    var keyChangedTitle: String {
        "\(selected?.name ?? "This user")'s identity key changed"
    }

    let keyChangedBody = "Same display name, new Ed25519 key. Could be a reinstall — "
        + "or a name takeover. Compare the words below out of band before you speak."

    var canAct: Bool {
        guard let s = selected else { return false }
        return !s.isSelf && !isDeriving && !safetyWords.isEmpty
    }

    /// Open for a channel, rebuilding the member list from the MLS group.
    /// `focusUuid` preselects a member — the rail's "verify key" row and the
    /// roster both arrive here with someone in mind.
    func open(mls: MlsWrapper?, channelId: String, channelName: String, focusUuid: String? = nil) async {
        self.channelName = channelName
        error = nil
        isPresented = true

        guard let mls else {
            error = "Connect to a channel before verifying identities."
            members = []
            clearDerived()
            return
        }

        let records: [MlsGroupMemberRecord]
        let epoch: UInt64
        do {
            records = try mls.groupMembers(channelId: channelId, isVoice: true)
            epoch = try mls.currentEpoch(channelId: channelId, isVoice: true)
        } catch {
            // Not in the group yet, or no group at all. Say so plainly rather
            // than showing an empty sheet that reads as "nobody to verify".
            self.error = "Can't read the group's keys: \(error.localizedDescription)"
            members = []
            clearDerived()
            return
        }

        securityLine = "MLS · X25519 · AES-128-GCM · Ed25519 · epoch \(epoch)"

        if let me = records.first(where: { $0.isSelf }) {
            localKey = me.signatureKey
            localUuid = me.identity
        }

        members = records
            .map { record in
                // Record every key we see before judging it — that is what makes
                // a later disagreement detectable at all.
                trust.observe(userUuid: record.identity, publicKey: record.signatureKey)

                return TrustMember(
                    id: record.identity,
                    publicKey: record.signatureKey,
                    isSelf: record.isSelf,
                    name: displayName(for: record.identity),
                    trust: trust.evaluate(userUuid: record.identity, publicKey: record.signatureKey),
                    shortFingerprint: (try? identityFingerprintShort(publicKey: record.signatureKey)) ?? ""
                )
            }
            .sorted { lhs, rhs in
                if lhs.isSelf != rhs.isSelf { return !lhs.isSelf }
                return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
            }

        let focus = focusUuid.flatMap { id in members.first { $0.id == id } }
            ?? members.first { $0.trust == .keyChanged }
            ?? members.first { !$0.isSelf }

        await select(focus)
    }

    /// Focus a member and derive the words for them.
    func select(_ member: TrustMember?) async {
        selectedId = member?.id
        clearDerived()

        guard let member, !member.isSelf, !localKey.isEmpty else { return }

        derivationToken += 1
        let token = derivationToken
        isDeriving = true

        let localKey = self.localKey
        let localUuid = self.localUuid

        do {
            // scrypt at N=16384 — off the main actor, or the sheet visibly stalls.
            let derived = try await Task.detached(priority: .userInitiated) {
                try derivePairwiseVerification(
                    localPublicKey: localKey,
                    localId: localUuid,
                    remotePublicKey: member.publicKey,
                    remoteId: member.id
                )
            }.value

            guard token == derivationToken else { return }

            safetyWords = derived.safetyWords
            localFingerprint = derived.localFingerprint
            remoteFingerprint = derived.remoteFingerprint
            pairLabel = "SAFETY WORDS — \(member.name) ↔ you"
            isDeriving = false
        } catch {
            guard token == derivationToken else { return }
            self.error = "Couldn't derive safety words: \(error.localizedDescription)"
            isDeriving = false
        }
    }

    /// "They match — mark verified".
    func markVerified() {
        guard let member = selected, !member.isSelf else { return }
        trust.markVerified(userUuid: member.id, publicKey: member.publicKey)
        refreshTrust(for: member.id)
    }

    /// Block — the negative outcome of the same comparison.
    func block() {
        guard let member = selected, !member.isSelf else { return }
        trust.block(publicKey: member.publicKey)
        refreshTrust(for: member.id)
    }

    func close() {
        isPresented = false
        members = []
        selectedId = nil
        clearDerived()
    }

    private func refreshTrust(for id: String) {
        guard let index = members.firstIndex(where: { $0.id == id }) else { return }
        members[index].trust = trust.evaluate(
            userUuid: members[index].id,
            publicKey: members[index].publicKey
        )
    }

    private func clearDerived() {
        safetyWords = []
        localFingerprint = ""
        remoteFingerprint = ""
        pairLabel = ""
        isDeriving = false
    }

    private func displayName(for uuid: String) -> String {
        if let name = nameForUuid(uuid), !name.trimmingCharacters(in: .whitespaces).isEmpty {
            return name
        }
        // A member we have no roster entry for. Showing a truncated uuid beats
        // inventing a name for someone we cannot identify.
        return String(uuid.prefix(8))
    }
}

/// The trust sheet: a glass sheet over the channel that makes E2EE legible.
struct TrustSheetView: View {
    @ObservedObject var model: TrustSheetModel

    private let columns = Array(
        repeating: GridItem(.flexible(), spacing: AuraTheme.Spacing.s8),
        count: 3
    )

    var body: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s14) {
            header

            if let error = model.error {
                banner(
                    text: error,
                    fill: AuraTheme.Colors.dangerFill,
                    border: AuraTheme.Colors.dangerBorder,
                    foreground: AuraTheme.Colors.danger
                )
            }

            if model.showsKeyChangedBanner {
                keyChangedBanner
            }

            ScrollView {
                VStack(alignment: .leading, spacing: AuraTheme.Spacing.s16) {
                    if !model.safetyWords.isEmpty { safetyWordsSection }
                    if model.isDeriving { derivingChip }
                    if !model.remoteFingerprint.isEmpty { fingerprintPair }
                    membersSection
                }
            }

            if model.canAct { actions }
        }
        .padding(AuraTheme.Spacing.s18)
        .frame(width: 560)
        .frame(maxHeight: 720)
        .background(AuraTheme.Colors.background)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s3) {
            HStack {
                Text("CHANNEL SECURITY")
                    .font(AuraTheme.Typography.mono(10))
                    .kerning(AuraTheme.Typography.monoLabelKerning)
                    .foregroundStyle(AuraTheme.Colors.textDim)
                Spacer()
                Button { model.close() } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 11, weight: .semibold))
                }
                .buttonStyle(.plain)
                .foregroundStyle(AuraTheme.Colors.textDim)
            }

            Text("\(model.channelName) is end-to-end encrypted")
                .font(.system(size: 15, weight: .bold))
                .foregroundStyle(AuraTheme.Colors.text)

            Text(model.securityLine)
                .font(AuraTheme.Typography.mono(10.5))
                .foregroundStyle(AuraTheme.Colors.textDim)

            Text("Your server relays ciphertext only.")
                .font(.system(size: 12))
                .foregroundStyle(AuraTheme.Colors.textFaint)
        }
    }

    /// Amber, not red: a reinstall and a takeover look identical from here, so
    /// it warns rather than accuses.
    private var keyChangedBanner: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s3) {
            Text(model.keyChangedTitle)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(AuraTheme.Colors.cautionText)
            Text(model.keyChangedBody)
                .font(.system(size: 12))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(AuraTheme.Spacing.s11)
        .background(AuraTheme.Colors.cautionFill)
        .clipShape(RoundedRectangle(cornerRadius: AuraTheme.Radii.r14))
        .overlay(
            RoundedRectangle(cornerRadius: AuraTheme.Radii.r14)
                .strokeBorder(AuraTheme.Colors.cautionBorder, lineWidth: 1)
        )
    }

    private func banner(text: String, fill: Color, border: Color, foreground: Color) -> some View {
        Text(text)
            .font(.system(size: 12))
            .foregroundStyle(foreground)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(AuraTheme.Spacing.s11)
            .background(fill)
            .clipShape(RoundedRectangle(cornerRadius: AuraTheme.Radii.r14))
            .overlay(
                RoundedRectangle(cornerRadius: AuraTheme.Radii.r14)
                    .strokeBorder(border, lineWidth: 1)
            )
    }

    private var safetyWordsSection: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s8) {
            Text(model.pairLabel)
                .font(AuraTheme.Typography.mono(10))
                .kerning(AuraTheme.Typography.monoLabelKerning)
                .foregroundStyle(AuraTheme.Colors.textDim)

            LazyVGrid(columns: columns, spacing: AuraTheme.Spacing.s8) {
                ForEach(model.safetyWords, id: \.self) { word in
                    Text(word)
                        .font(AuraTheme.Typography.mono(13))
                        .foregroundStyle(AuraTheme.Colors.text)
                        // Selectable: users paste these into whatever
                        // out-of-band channel they trust.
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, AuraTheme.Spacing.s8)
                        .background(Color.white.opacity(0.06))
                        .clipShape(RoundedRectangle(cornerRadius: AuraTheme.Radii.r14))
                }
            }
        }
    }

    /// Deriving costs an scrypt run; a chip, never a spinner.
    private var derivingChip: some View {
        Text("deriving safety words…")
            .font(AuraTheme.Typography.mono(10))
            .foregroundStyle(AuraTheme.Colors.textDim)
            .padding(.horizontal, AuraTheme.Spacing.s12)
            .padding(.vertical, AuraTheme.Spacing.s9)
            .background(Color.white.opacity(0.06))
            .clipShape(Capsule())
            .frame(maxWidth: .infinity)
    }

    /// The authoritative comparison the words are a convenience over.
    private var fingerprintPair: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s5) {
            Text(model.remoteFingerprint)
                .font(AuraTheme.Typography.mono(10))
                .foregroundStyle(AuraTheme.Colors.textMid)
                .textSelection(.enabled)
            Text(model.localFingerprint)
                .font(AuraTheme.Typography.mono(10))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .textSelection(.enabled)
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private var membersSection: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s5) {
            Text("MEMBERS")
                .font(AuraTheme.Typography.mono(10))
                .kerning(AuraTheme.Typography.monoLabelKerning)
                .foregroundStyle(AuraTheme.Colors.textDim)

            ForEach(model.members) { member in
                Button {
                    Task { await model.select(member) }
                } label: {
                    HStack(spacing: AuraTheme.Spacing.s9) {
                        RoundedRectangle(cornerRadius: 2)
                            .fill(AuraTheme.Colors.accent)
                            .frame(width: 3, height: 18)
                            .opacity(member.id == model.selectedId ? 1 : 0)

                        Text(member.name)
                            .font(.system(size: 13))
                            .foregroundStyle(AuraTheme.Colors.text)

                        Spacer()

                        Text(member.stateLabel)
                            .font(AuraTheme.Typography.mono(9.5))
                            .foregroundStyle(member.stateColor)
                    }
                    .padding(.vertical, AuraTheme.Spacing.s8)
                    .padding(.horizontal, AuraTheme.Spacing.s11)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .auraFluidHover()
            }
        }
    }

    private var actions: some View {
        HStack(spacing: AuraTheme.Spacing.s8) {
            Button { model.markVerified() } label: {
                Text("They match — mark verified")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, AuraTheme.Spacing.s9)
                    .background(AuraTheme.Gradients.primary)
                    .clipShape(RoundedRectangle(cornerRadius: AuraTheme.Radii.r16))
            }
            .buttonStyle(.plain)

            Button { model.block() } label: {
                Text("Block")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(AuraTheme.Colors.danger)
                    .padding(.horizontal, AuraTheme.Spacing.s16)
                    .padding(.vertical, AuraTheme.Spacing.s9)
                    .background(AuraTheme.Colors.dangerFill)
                    .clipShape(RoundedRectangle(cornerRadius: AuraTheme.Radii.r16))
                    .overlay(
                        RoundedRectangle(cornerRadius: AuraTheme.Radii.r16)
                            .strokeBorder(AuraTheme.Colors.dangerBorder, lineWidth: 1)
                    )
            }
            .buttonStyle(.plain)
        }
    }
}
