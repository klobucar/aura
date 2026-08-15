import SwiftUI

// MARK: - Participant

/// One occupant of the current voice channel, flattened into the shape the
/// voice surface renders. The local user is a participant like anyone else —
/// the rail must not special-case "you" out of the lanes.
struct VoiceParticipant: Identifiable, Equatable {
    let id: UInt32
    let displayName: String
    let avatarData: Data?
    let isLocal: Bool
    let isMuted: Bool
    let isDeafened: Bool
    let isDisconnected: Bool

    var initial: String {
        String(displayName.prefix(1)).uppercased()
    }
}

// MARK: - Context

/// Everything the voice surface needs from the rest of the app, gathered once
/// by `ContentView` so the rail components do not each reach into the network
/// client, audio settings and hotkey manager independently.
struct VoiceRailContext {
    let participants: [VoiceParticipant]
    let latencyMs: Int?
    let isReconnecting: Bool

    // Command HUD
    let isMicEnabled: Bool
    let isDeafened: Bool
    let isPTTHeld: Bool
    let pttHeldSince: Date?
    /// Right-hand text of the PTT chip's binding, e.g. `⌥ Space`, or the
    /// transmission mode when push-to-talk is not the active mode.
    let transmissionLabel: String
    let isPushToTalk: Bool

    func participant(_ id: UInt32) -> VoiceParticipant? {
        participants.first { $0.id == id }
    }
}

/// Level metering shared by the HUD meter and the speaker waveform.
///
/// The levels themselves are read straight off the network client inside the
/// leaf views that draw them. `@Observable` tracks per-body, so a 50Hz meter
/// invalidates the meter and nothing above it — routing the value through
/// `VoiceRailContext` would redraw the whole window instead.
enum VoiceLevel {
    /// Mirrors `QuicNetworkClient.levelFloorDb`.
    static let floorDb: Float = -60

    /// dBFS → 0…1 across the meter floor.
    static func normalized(_ db: Float) -> Float {
        min(1, max(0, (db - floorDb) / -floorDb))
    }
}

/// Callbacks back into the existing mute / deafen / per-user volume plumbing.
struct VoiceRailActions {
    var toggleMic: () -> Void
    var toggleDeafen: () -> Void
    var localVolume: (UInt32) -> Float
    var setLocalVolume: (UInt32, Float) -> Void
}

// MARK: - Shared pieces

/// Circular avatar with the initial-letter fallback the handoff asks for
/// (never a spinner while an image is missing).
struct VoiceAvatar: View {
    let participant: VoiceParticipant?
    let size: CGFloat

    var body: some View {
        ZStack {
            if let data = participant?.avatarData, let image = NSImage(data: data) {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
                    .frame(width: size, height: size)
                    .clipShape(Circle())
            } else {
                Circle()
                    .fill(AuraTheme.Gradients.primary)
                    .frame(width: size, height: size)
                    .overlay {
                        Text(participant?.initial ?? "?")
                            .font(AuraTheme.Typography.ui(size * 0.44, weight: .bold))
                            .foregroundStyle(.white)
                    }
            }
        }
        .frame(width: size, height: size)
        .opacity(participant?.isDisconnected == true ? 0.5 : 1.0)
    }
}

/// Mono section label — `LAST 3 MIN`, `SILENT`, `INPUT`.
struct VoiceSectionLabel: View {
    let text: String

    var body: some View {
        Text(text)
            .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
            .kerning(AuraTheme.Typography.monoLabelKerning)
            .foregroundStyle(AuraTheme.Colors.textDim)
    }
}

/// Ring that breathes while somebody has the floor.
struct SpeakingRing: View {
    /// Diameter of the avatar the ring wraps.
    let avatarSize: CGFloat
    /// How far outside the avatar the ring sits (the handoff's negative inset).
    let inset: CGFloat

    @State private var pulse = false

    var body: some View {
        Circle()
            .strokeBorder(AuraTheme.Colors.accent.opacity(0.5), lineWidth: 2)
            .frame(width: avatarSize + inset * 2, height: avatarSize + inset * 2)
            .opacity(pulse ? 0.85 : 0.35)
            .scaleEffect(pulse ? 1.05 : 1.0)
            .onAppear {
                withAnimation(AuraTheme.Motion.speakingPulse) { pulse = true }
            }
    }
}

/// Compact round-trip latency indicator.
/// `nil` latency (haven't heard back yet) reads as "…",
/// <80ms = green, <200ms = yellow, >=200ms = red.
struct LatencyPill: View {
    let latencyMs: Int?
    /// While reconnecting the pill goes dim rather than red.
    var isReconnecting: Bool = false

    private var color: Color {
        guard !isReconnecting, let ms = latencyMs else { return AuraTheme.Colors.textDim }
        if ms < 80 { return AuraTheme.Colors.good }
        if ms < 200 { return AuraTheme.Colors.caution }
        return AuraTheme.Colors.danger
    }

    var body: some View {
        HStack(spacing: AuraTheme.Spacing.s5) {
            Circle()
                .fill(color)
                .frame(width: 6, height: 6)
            Text(latencyMs.map { "\($0) ms" } ?? "… ms")
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .foregroundStyle(AuraTheme.Colors.textDim)
        }
        .padding(.horizontal, AuraTheme.Spacing.s8)
        .padding(.vertical, AuraTheme.Spacing.s3)
        .background(Capsule().fill(Color.white.opacity(0.06)))
        .overlay(Capsule().strokeBorder(AuraTheme.Colors.glassBorder.opacity(0.6), lineWidth: 0.5))
        .help("Round-trip to server")
    }
}

/// `verified` / `muted` / `verify key` style trailing pill.
struct VoiceStatePill: View {
    let text: String
    let color: Color
    var filled: Bool = false

    var body: some View {
        Text(text)
            .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
            .foregroundStyle(color)
            .padding(.horizontal, AuraTheme.Spacing.s8)
            .padding(.vertical, AuraTheme.Spacing.s2)
            .background {
                if filled {
                    Capsule().fill(color.opacity(0.13))
                }
            }
            .overlay {
                if filled {
                    Capsule().strokeBorder(color.opacity(0.30), lineWidth: 0.5)
                }
            }
    }
}

// MARK: - Metrics

/// The rail and the stage are the same components at two sizes. Everything
/// that differs between Chat-first and Voice-focus lives here so the views
/// stay single-implementation.
struct VoiceMetrics {
    // Current speaker card
    let heroAvatar: CGFloat
    let heroRingInset: CGFloat
    let heroNameSize: CGFloat
    let waveformBars: Int
    let waveformHeight: CGFloat

    // Lanes
    let laneAvatar: CGFloat
    let laneHeight: CGFloat
    let segmentHeight: CGFloat
    let segmentTop: CGFloat
    let segmentRadius: CGFloat
    let nameColumnWidth: CGFloat?
    let showsTimeAxis: Bool

    /// Chat-first: 336px rail. Lanes lose the time axis and the name column;
    /// the talk-share column carries the load.
    static let rail = VoiceMetrics(
        heroAvatar: 52,
        heroRingInset: 5,
        heroNameSize: AuraTheme.Typography.t15,
        waveformBars: 16,
        waveformHeight: 30,
        laneAvatar: 16,
        laneHeight: 16,
        segmentHeight: 8,
        segmentTop: 4,
        segmentRadius: 4,
        nameColumnWidth: nil,
        showsTimeAxis: false
    )

    /// Voice-focus: the same components with room to breathe.
    static let stage = VoiceMetrics(
        heroAvatar: 88,
        heroRingInset: 7,
        heroNameSize: AuraTheme.Typography.t19,
        waveformBars: 26,
        waveformHeight: 44,
        laneAvatar: 20,
        laneHeight: 22,
        segmentHeight: 12,
        segmentTop: 5,
        segmentRadius: 6,
        nameColumnWidth: 74,
        showsTimeAxis: true
    )

    static func forMode(_ mode: AuraLayoutMode) -> VoiceMetrics {
        mode == .chatFirst ? .rail : .stage
    }
}

// MARK: - Lane palette

extension AuraTheme {
    /// Lane and segment color by talk-share rank, matching the mock: the
    /// floor-holder is accent, second is secondary, third is primary, then the
    /// ramp repeats.
    static func laneColor(rank: Int) -> Color {
        switch rank % 3 {
        case 0: return Colors.accent
        case 1: return Colors.secondary
        default: return Colors.primary
        }
    }
}
