import SwiftUI

/// Who has the floor: avatar with a breathing ring, how long they have held
/// it, their share of the window, a live waveform, and the local-only volume
/// you have set for them.
struct CurrentSpeakerCard: View {
    @ObservedObject var store: TalkSegmentStore
    /// Separately observed so the 10Hz level sampling redraws the bars only.
    @ObservedObject var waveformBuffer: WaveformBuffer
    let context: VoiceRailContext
    let actions: VoiceRailActions
    let metrics: VoiceMetrics

    var body: some View {
        if let speakerId = store.currentSpeakerId, let speaker = context.participant(speakerId) {
            card(speaker: speaker)
        } else {
            emptyChip
        }
    }

    // MARK: Card

    private func card(speaker: VoiceParticipant) -> some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s11) {
            HStack(alignment: .center, spacing: AuraTheme.Spacing.s12) {
                ZStack {
                    VoiceAvatar(participant: speaker, size: metrics.heroAvatar)
                    SpeakingRing(avatarSize: metrics.heroAvatar, inset: metrics.heroRingInset)
                }

                identity(speaker: speaker)

                if metrics.showsTimeAxis {
                    Spacer(minLength: AuraTheme.Spacing.s8)
                }
            }

            waveform

            volumeRow(speaker: speaker)
        }
        .padding(AuraTheme.Spacing.s14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: AuraTheme.Radii.r22)
                .fill(
                    LinearGradient(
                        colors: [
                            AuraTheme.Colors.accent.opacity(0.15),
                            Color.white.opacity(0.045)
                        ],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    )
                )
        }
        .overlay {
            RoundedRectangle(cornerRadius: AuraTheme.Radii.r22)
                .strokeBorder(AuraTheme.Colors.accent.opacity(0.36), lineWidth: 1)
        }
    }

    @ViewBuilder
    private func identity(speaker: VoiceParticipant) -> some View {
        // The name row and the meta line share one baseline in the rail and
        // split apart in the stage, where the meta is right-aligned.
        let meta = TimelineView(.periodic(from: .now, by: 1)) { timeline in
            Text(metaLine(speaker: speaker, now: timeline.date))
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .lineLimit(1)
        }

        if metrics.showsTimeAxis {
            HStack(alignment: .firstTextBaseline, spacing: AuraTheme.Spacing.s9) {
                nameRow(speaker: speaker)
                Spacer(minLength: AuraTheme.Spacing.s12)
                meta
            }
        } else {
            VStack(alignment: .leading, spacing: AuraTheme.Spacing.s3) {
                nameRow(speaker: speaker)
                meta
            }
        }
    }

    private func nameRow(speaker: VoiceParticipant) -> some View {
        HStack(spacing: AuraTheme.Spacing.s8) {
            Text(speaker.displayName)
                .font(AuraTheme.Typography.ui(metrics.heroNameSize, weight: .bold))
                .kerning(AuraTheme.Typography.titleKerning)
                .foregroundStyle(AuraTheme.Colors.text)
                .lineLimit(1)

            if speaker.isLocal {
                VoiceStatePill(text: "you", color: AuraTheme.Colors.textDim, filled: true)
            }
        }
    }

    /// `speaking 0:12 · 41%` in the rail, spelled out in the stage.
    private func metaLine(speaker: VoiceParticipant, now: Date) -> String {
        let share = store.talkShare[speaker.id] ?? 0
        let percent = Int((share * 100).rounded())
        let elapsed = store.currentSpeakerElapsed(now: now) ?? 0
        let clock = String(format: "%d:%02d", Int(elapsed) / 60, Int(elapsed) % 60)
        let suffix = metrics.showsTimeAxis ? "\(percent)% of last 3 min" : "\(percent)%"
        return "speaking \(clock) · \(suffix)"
    }

    // MARK: Waveform

    /// Bars driven by the actual audio level — the local capture level when you
    /// hold the floor, the mixed receive level when somebody else does.
    private var waveform: some View {
        let samples = Array(waveformBuffer.samples.suffix(metrics.waveformBars))

        return HStack(alignment: .center, spacing: 2) {
            ForEach(Array(samples.enumerated()), id: \.offset) { _, level in
                let height = max(2, CGFloat(level) * metrics.waveformHeight)
                RoundedRectangle(cornerRadius: 2)
                    .fill(AuraTheme.Colors.accent)
                    .opacity(level > 0.6 ? 1.0 : Double(0.4 + level * 0.35))
                    .frame(height: height)
            }
        }
        .frame(height: metrics.waveformHeight)
        .animation(.linear(duration: 0.1), value: samples)
    }

    // MARK: Local volume

    /// Per-listener volume for the speaker. Nothing here reaches the network —
    /// it drives the existing `setLocalVolume` plumbing only.
    private func volumeRow(speaker: VoiceParticipant) -> some View {
        let volume = actions.localVolume(speaker.id)
        // The plumbing allows 0…2; the track shows that whole range.
        let fraction = min(1, max(0, Double(volume) / 2.0))

        return HStack(spacing: AuraTheme.Spacing.s9) {
            Image(systemName: "speaker.wave.2.fill")
                .font(.system(size: 10))
                .foregroundStyle(AuraTheme.Colors.textDim)

            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    Capsule()
                        .fill(Color.white.opacity(0.16))
                    Capsule()
                        .fill(AuraTheme.Colors.accent)
                        .frame(width: geometry.size.width * fraction)
                        .shadow(color: AuraTheme.Colors.accent.opacity(0.5), radius: 8)
                }
                .contentShape(Rectangle())
                .gesture(
                    DragGesture(minimumDistance: 0)
                        .onChanged { value in
                            let ratio = min(1, max(0, value.location.x / max(1, geometry.size.width)))
                            actions.setLocalVolume(speaker.id, Float(ratio * 2.0))
                        }
                )
            }
            .frame(height: 5)

            Text("\(Int((volume * 100).rounded()))")
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .frame(width: 24, alignment: .trailing)
        }
        .help("Local playback volume for \(speaker.displayName) — never leaves this machine")
    }

    // MARK: Empty state

    /// A chip, never a spinner.
    private var emptyChip: some View {
        HStack {
            Spacer()
            Text("No one is speaking yet")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .padding(.horizontal, AuraTheme.Spacing.s16)
                .padding(.vertical, AuraTheme.Spacing.s11)
                .auraGlass(cornerRadius: AuraTheme.Radii.r20)
            Spacer()
        }
        .padding(.vertical, AuraTheme.Spacing.s8)
    }
}
