import SwiftUI

/// The voice surface. 336px rail in Chat-first, the wide column in
/// Voice-focus — same four parts either way: who has the floor, who has been
/// talking, who is present but silent, and the transport.
///
/// This replaces the old avatar grid, which only duplicated the roster.
struct VoiceRailView: View {
    @ObservedObject var store: TalkSegmentStore
    /// Passed down to the HUD, which reads the input level directly so the
    /// meter's cadence stays local to it.
    let client: QuicNetworkClient
    let context: VoiceRailContext
    let actions: VoiceRailActions
    let mode: AuraLayoutMode

    private var metrics: VoiceMetrics { VoiceMetrics.forMode(mode) }

    /// Present-but-silent: nobody with talk time in the window, so the lanes
    /// already account for everyone else.
    private var silentParticipants: [VoiceParticipant] {
        context.participants.filter { (store.talkShare[$0.id] ?? 0) <= 0 }
    }

    var body: some View {
        VStack(spacing: 0) {
            header

            Divider().opacity(0.1)

            ScrollView {
                VStack(alignment: .leading, spacing: AuraTheme.Spacing.s12) {
                    CurrentSpeakerCard(
                        store: store,
                        waveformBuffer: store.waveform,
                        context: context,
                        actions: actions,
                        metrics: metrics
                    )

                    TalkLanesPanel(
                        store: store,
                        context: context,
                        metrics: metrics
                    )

                    if !silentParticipants.isEmpty {
                        silentList
                    }
                }
                .padding(.horizontal, AuraTheme.Spacing.s14)
                .padding(.top, AuraTheme.Spacing.s12)
                .padding(.bottom, AuraTheme.Spacing.s16)
            }

            CommandHUDView(client: client, context: context, actions: actions)
                .padding(.horizontal, AuraTheme.Spacing.s14)
                .padding(.bottom, AuraTheme.Spacing.s16)
        }
    }

    // MARK: Header

    private var header: some View {
        HStack {
            Text("Voice")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13, weight: .semibold))
                .foregroundStyle(AuraTheme.Colors.text)
            Spacer()
            LatencyPill(latencyMs: context.latencyMs, isReconnecting: context.isReconnecting)
        }
        .padding(.horizontal, AuraTheme.Spacing.s14)
        .frame(height: AuraTheme.Layout.headerHeight)
        .padding(.top, 28) // Synced with the roster and channel headers
        .background(VisualEffectBlur(auraMaterial: .header, blendingMode: .withinWindow))
    }

    // MARK: Silent list

    private var silentList: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s5) {
            VoiceSectionLabel(text: "SILENT")

            ForEach(silentParticipants) { participant in
                silentRow(participant)
            }
        }
    }

    private func silentRow(_ participant: VoiceParticipant) -> some View {
        HStack(spacing: AuraTheme.Spacing.s9) {
            VoiceAvatar(participant: participant, size: 22)

            Text(participant.isLocal ? "you" : participant.displayName)
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                .foregroundStyle(AuraTheme.Colors.textMid)
                .lineLimit(1)

            Spacer()

            if participant.isDeafened {
                VoiceStatePill(text: "deafened", color: AuraTheme.Colors.warn)
            } else if participant.isMuted {
                VoiceStatePill(text: "muted", color: AuraTheme.Colors.warn)
            }
        }
        .padding(.horizontal, AuraTheme.Spacing.s12)
        .padding(.vertical, AuraTheme.Spacing.s8)
        .background {
            RoundedRectangle(cornerRadius: AuraTheme.Radii.r16)
                .fill(Color.white.opacity(0.04))
        }
        .auraFluidHover(scale: 1.01)
    }
}
