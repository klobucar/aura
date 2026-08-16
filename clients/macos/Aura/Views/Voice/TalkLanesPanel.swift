import SwiftUI

/// "LAST 3 MIN" — one lane per talker over the rolling window, with the
/// overlap ("talked over") intervals drawn on top of the later speaker's lane.
///
/// Lanes never render unbounded: the top five talkers get a lane and the rest
/// collapse behind an "N others" row that expands on click.
struct TalkLanesPanel: View {
    @ObservedObject var store: TalkSegmentStore
    let context: VoiceRailContext
    let metrics: VoiceMetrics

    /// How many lanes render before the overflow row takes over.
    private static let laneLimit = 5

    @State private var showsOverflow = false

    var body: some View {
        let lanes = store.lanes(limit: Self.laneLimit)
        let ranked = lanes.top + (showsOverflow ? lanes.overflow : [])

        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s9) {
            header

            if ranked.isEmpty {
                Text("no one has spoken in the last 3 minutes")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.textFaint)
                    .padding(.vertical, AuraTheme.Spacing.s5)
            } else {
                // `now` is resampled every second so the live segment grows and
                // the window slides — the handoff's "re-laid out every 1s".
                TimelineView(.periodic(from: .now, by: 1)) { timeline in
                    VStack(alignment: .leading, spacing: AuraTheme.Spacing.s9) {
                        ForEach(Array(ranked.enumerated()), id: \.element) { index, sessionId in
                            laneRow(sessionId: sessionId, rank: index, now: timeline.date)
                        }

                        if metrics.showsTimeAxis {
                            timeAxis
                                .transition(.opacity.animation(AuraTheme.Motion.crossFade))
                        }
                    }
                }
            }

            if !lanes.overflow.isEmpty {
                overflowRow(count: lanes.overflow.count)
            }
        }
        .padding(.vertical, 13)
        .padding(.horizontal, AuraTheme.Spacing.s14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .auraGlass(cornerRadius: AuraTheme.Radii.r22)
    }

    // MARK: Header

    private var header: some View {
        HStack {
            VoiceSectionLabel(text: metrics.showsTimeAxis ? "WHO'S BEEN TALKING" : "LAST 3 MIN")
            Spacer()
            legendChip
        }
    }

    /// The only key the lanes need: amber means somebody talked over somebody.
    private var legendChip: some View {
        HStack(spacing: AuraTheme.Spacing.s5) {
            RoundedRectangle(cornerRadius: 1.5)
                .fill(AuraTheme.Colors.caution)
                .frame(width: 7, height: 7)
            Text("overlap")
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .foregroundStyle(AuraTheme.Colors.textDim)
        }
    }

    // MARK: Lane row

    @ViewBuilder
    private func laneRow(sessionId: UInt32, rank: Int, now: Date) -> some View {
        let participant = context.participant(sessionId)
        let share = store.talkShare[sessionId] ?? 0
        // A barely-speaking member sits back so the lane block reads as a
        // ranking rather than a flat table.
        let dimmed = share < 0.05

        HStack(spacing: AuraTheme.Spacing.s8) {
            VoiceAvatar(participant: participant, size: metrics.laneAvatar)

            if let width = metrics.nameColumnWidth {
                Text(participant?.displayName ?? "unknown")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .regular))
                    .foregroundStyle(AuraTheme.Colors.textMid)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(width: width, alignment: .leading)
                    .transition(.opacity.animation(AuraTheme.Motion.crossFade))
            }

            lane(sessionId: sessionId, rank: rank, now: now)

            Text("\(Int((share * 100).rounded()))%")
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .frame(width: 26, alignment: .trailing)
        }
        .opacity(dimmed ? 0.6 : 1.0)
        .help("\(participant?.displayName ?? "unknown") — \(Int((share * 100).rounded()))% of the last 3 minutes")
    }

    /// The track plus its absolutely-positioned segments. Overlaps are drawn
    /// last so they sit on top of the speech they interrupted.
    private func lane(sessionId: UInt32, rank: Int, now: Date) -> some View {
        let color = AuraTheme.laneColor(rank: rank)

        return GeometryReader { geometry in
            let width = geometry.size.width

            ZStack(alignment: .topLeading) {
                RoundedRectangle(cornerRadius: AuraTheme.Radii.r6)
                    .fill(Color.white.opacity(0.05))
                    .overlay {
                        RoundedRectangle(cornerRadius: AuraTheme.Radii.r6)
                            .strokeBorder(Color.white.opacity(0.08), lineWidth: 1)
                    }

                ForEach(store.segments(for: sessionId)) { segment in
                    segmentBar(
                        start: segment.start,
                        end: segment.end ?? now,
                        now: now,
                        width: width,
                        color: color.opacity(segment.isLive ? 1.0 : 0.75)
                    )
                }

                ForEach(store.overlapSegments(for: sessionId)) { overlap in
                    segmentBar(
                        start: overlap.start,
                        end: overlap.end,
                        now: now,
                        width: width,
                        color: AuraTheme.Colors.caution.opacity(0.9)
                    )
                }
            }
        }
        .frame(height: metrics.laneHeight)
    }

    private func segmentBar(start: Date, end: Date, now: Date, width: CGFloat, color: Color) -> some View {
        let from = store.fraction(of: start, now: now)
        let to = store.fraction(of: end, now: now)
        let x = CGFloat(from) * width
        // Sub-pixel runs still deserve a visible pip.
        let barWidth = max(2, CGFloat(to - from) * width)

        return RoundedRectangle(cornerRadius: metrics.segmentRadius)
            .fill(color)
            .frame(width: barWidth, height: metrics.segmentHeight)
            .offset(x: x, y: metrics.segmentTop)
    }

    // MARK: Time axis (Voice-focus only)

    private var timeAxis: some View {
        HStack(spacing: 0) {
            ForEach(["−3 min", "−2 min", "−1 min", "now"], id: \.self) { label in
                Text(label)
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.textFaint)
                    .frame(maxWidth: .infinity, alignment: label == "now" ? .trailing : .leading)
            }
        }
        .padding(.leading, metrics.laneAvatar + (metrics.nameColumnWidth ?? 0) + AuraTheme.Spacing.s16)
        .padding(.trailing, 26 + AuraTheme.Spacing.s8)
    }

    // MARK: Overflow

    private func overflowRow(count: Int) -> some View {
        Button {
            withAnimation(.spring(response: 0.3, dampingFraction: 0.8)) {
                showsOverflow.toggle()
            }
        } label: {
            HStack(spacing: AuraTheme.Spacing.s8) {
                Image(systemName: showsOverflow ? "chevron.down" : "chevron.right")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(AuraTheme.Colors.textDim)
                Text(showsOverflow ? "show top \(Self.laneLimit)" : "\(count) others")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.textDim)
                Spacer()
            }
            .padding(.vertical, AuraTheme.Spacing.s3)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .auraFluidHover()
        .help(showsOverflow ? "Collapse to the top \(Self.laneLimit) talkers" : "Show the remaining \(count) talkers")
    }
}
