import SwiftUI

/// Who a profile card is about — a snapshot taken when the roster row is
/// clicked.
struct ProfileCardSubject {
    let sessionId: UInt32
    let uuid: String?
    let displayName: String
    let bio: String
    let avatarData: Data?
    let isLocal: Bool
    /// `nil` when we hold no key for them: only members of our own channel's
    /// MLS group have one we can vouch for.
    let trust: TrustState?
    let identityKey: Data?
}

/// User comment card (handoff §6): avatar, name + trust pill, fingerprint,
/// markdown comment, and the two actions — Local volume and Compare keys.
///
/// Shown in a popover, so it carries no glass of its own: the popover is
/// already the translucent layer.
struct ProfileCardView: View {
    let subject: ProfileCardSubject
    let client: QuicNetworkClient
    var compareKeys: (String) -> Void
    var editProfile: () -> Void

    @State private var showsVolume = false

    private var fullFingerprint: String? {
        guard let key = subject.identityKey, !key.isEmpty else { return nil }
        return try? identityFingerprint(publicKey: key)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 13) {
            HStack(spacing: AuraTheme.Spacing.s12) {
                AuraAvatar(name: subject.displayName, data: subject.avatarData, size: 46)

                VStack(alignment: .leading, spacing: AuraTheme.Spacing.s3) {
                    HStack(spacing: AuraTheme.Spacing.s8) {
                        Text(subject.displayName)
                            .font(AuraTheme.Typography.ui(AuraTheme.Typography.t15, weight: .bold))
                            .kerning(AuraTheme.Typography.titleKerning)
                            .foregroundStyle(AuraTheme.Colors.text)
                            .lineLimit(1)
                        trustPill
                    }

                    if let full = fullFingerprint {
                        Text(ChatFormatting.cardFingerprint(full))
                            .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                            .foregroundStyle(AuraTheme.Colors.textDim)
                            .textSelection(.enabled)
                            .help(full)
                    } else if !subject.isLocal {
                        Text("key visible once you share a channel")
                            .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                            .foregroundStyle(AuraTheme.Colors.textFaint)
                    }
                }
            }

            if subject.bio.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                Text(subject.isLocal ? "You haven't written a comment yet." : "No comment.")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.textFaint)
            } else {
                AuraMarkdownText(
                    source: subject.bio,
                    size: AuraTheme.Typography.t12,
                    foreground: AuraTheme.Colors.textMid,
                    lineSpacing: 3
                )
            }

            if showsVolume && !subject.isLocal {
                volumeRow
            }

            HStack(spacing: AuraTheme.Spacing.s8) {
                if subject.isLocal {
                    AuraChipButton(title: "Edit profile", systemImage: "pencil", action: editProfile)
                } else {
                    AuraChipButton(
                        title: "Local volume",
                        systemImage: "speaker.wave.2.fill",
                        isActive: showsVolume
                    ) {
                        withAnimation(AuraTheme.Motion.crossFade) { showsVolume.toggle() }
                    }

                    AuraChipButton(
                        title: "Compare keys",
                        systemImage: "key.fill",
                        tint: subject.trust == .keyChanged ? AuraTheme.Colors.caution : nil
                    ) {
                        if let uuid = subject.uuid { compareKeys(uuid) }
                    }
                    .disabled(subject.uuid == nil || subject.trust == nil)
                    .opacity(subject.trust == nil ? 0.5 : 1)
                    .help(subject.trust == nil
                          ? "Join their channel to compare keys"
                          : "Compare safety words out of band")
                }
            }
        }
        .padding(AuraTheme.Spacing.s16)
        .frame(width: 330, alignment: .leading)
    }

    @ViewBuilder
    private var trustPill: some View {
        if subject.isLocal {
            VoiceStatePill(text: "you", color: AuraTheme.Colors.accent, filled: true)
        } else {
            switch subject.trust {
            case .verified:
                VoiceStatePill(text: "verified", color: AuraTheme.Colors.accent, filled: true)
            case .keyChanged:
                VoiceStatePill(text: "key changed", color: AuraTheme.Colors.cautionText, filled: true)
            case .blocked:
                VoiceStatePill(text: "blocked", color: AuraTheme.Colors.danger, filled: true)
            case .tofu:
                VoiceStatePill(text: "TOFU · not compared", color: AuraTheme.Colors.textDim, filled: true)
            case nil:
                EmptyView()
            }
        }
    }

    /// This person's local playback gain — not master output. Local only:
    /// nothing about it leaves this machine.
    private var volumeRow: some View {
        let id = subject.sessionId
        let volume = Binding<Double>(
            get: { Double(client.localVolume(for: id)) },
            set: { client.setLocalVolume(sessionId: id, volume: Float($0)) }
        )
        let isMuted = client.isLocallyMuted(sessionId: id)

        return HStack(spacing: AuraTheme.Spacing.s8) {
            Button {
                client.toggleLocalMute(sessionId: id)
            } label: {
                Image(systemName: isMuted ? "speaker.slash.fill" : "speaker.wave.2.fill")
                    .font(.system(size: 11))
                    .foregroundStyle(isMuted ? AuraTheme.Colors.warn : AuraTheme.Colors.textDim)
                    .frame(width: 18)
            }
            .buttonStyle(.plain)
            .help(isMuted ? "Unmute locally" : "Mute locally")

            Slider(value: volume, in: 0...2)
                .controlSize(.small)
                .tint(AuraTheme.Colors.accent)
                .disabled(isMuted)

            Text("\(Int((volume.wrappedValue * 100).rounded()))%")
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .frame(width: 36, alignment: .trailing)
        }
        .transition(.opacity)
    }
}
