import SwiftUI

/// Bottom-pinned transport: the mic/deafen pair, the input meter, and the
/// push-to-talk binding. All three drive the existing plumbing in
/// `ContentView` / `HotkeyManager` — this is a re-skin and a relocation, not a
/// second implementation.
struct CommandHUDView: View {
    /// Read here rather than through `VoiceRailContext` so the 50Hz input
    /// level only invalidates this view — see `VoiceLevel`.
    let client: QuicNetworkClient
    let context: VoiceRailContext
    let actions: VoiceRailActions

    /// Bars in the input meter.
    private static let meterBars = 9

    var body: some View {
        VStack(spacing: AuraTheme.Spacing.s11) {
            HStack(spacing: AuraTheme.Spacing.s12) {
                segmentedPair
                inputMeter
            }
            pttRow
        }
        .padding(.vertical, 13)
        .padding(.horizontal, AuraTheme.Spacing.s14)
        .auraGlass(cornerRadius: AuraTheme.Radii.r24)
    }

    // MARK: Mic / deafen

    private var segmentedPair: some View {
        HStack(spacing: AuraTheme.Spacing.s2) {
            segment(
                icon: context.isMicEnabled ? "mic.fill" : "mic.slash.fill",
                isActive: context.isMicEnabled,
                tint: AuraTheme.Colors.accent,
                help: context.isMicEnabled ? "Mute" : "Unmute",
                action: actions.toggleMic
            )
            segment(
                icon: context.isDeafened ? "headphones.slash" : "headphones",
                isActive: context.isDeafened,
                tint: AuraTheme.Colors.danger,
                help: context.isDeafened ? "Undeafen" : "Deafen",
                action: actions.toggleDeafen
            )
        }
        .padding(AuraTheme.Spacing.s3)
        .background {
            RoundedRectangle(cornerRadius: AuraTheme.Radii.r20)
                .fill(Color.white.opacity(0.16))
                .overlay {
                    RoundedRectangle(cornerRadius: AuraTheme.Radii.r20)
                        .strokeBorder(Color.black.opacity(0.25), lineWidth: 1)
                        .blur(radius: 1)
                        .mask(RoundedRectangle(cornerRadius: AuraTheme.Radii.r20))
                }
        }
    }

    private func segment(
        icon: String,
        isActive: Bool,
        tint: Color,
        help: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 13, weight: .bold))
                .foregroundStyle(isActive ? Color.white : AuraTheme.Colors.textDim)
                .frame(width: 42, height: 34)
                .background {
                    if isActive {
                        RoundedRectangle(cornerRadius: AuraTheme.Radii.r16)
                            .fill(AuraTheme.Gradients.primary)
                            .shadow(color: tint.opacity(0.55), radius: 14, x: 0, y: 4)
                    }
                }
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
    }

    // MARK: Input meter

    private var inputMeter: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s5) {
            HStack {
                VoiceSectionLabel(text: "INPUT")
                Spacer()
                Text(dbLabel)
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.textDim)
            }

            HStack(spacing: AuraTheme.Spacing.s3) {
                let level = VoiceLevel.normalized(client.inputLevelDb)
                let lit = Int((Double(level) * Double(Self.meterBars)).rounded())
                ForEach(0..<Self.meterBars, id: \.self) { index in
                    RoundedRectangle(cornerRadius: 1.5)
                        .fill(index < lit ? AuraTheme.Colors.accent : Color.white.opacity(0.12))
                        .frame(height: index < lit ? 14 : 5)
                        .frame(height: 14, alignment: .bottom)
                }
            }
            .animation(.linear(duration: 0.1), value: client.inputLevelDb)
        }
    }

    private var dbLabel: String {
        guard client.inputLevelDb > VoiceLevel.floorDb else { return "−∞ dB" }
        // U+2212 minus, to match the mock's numerals.
        return "−\(Int(-client.inputLevelDb.rounded())) dB"
    }

    // MARK: Push-to-talk

    private var pttRow: some View {
        HStack(spacing: AuraTheme.Spacing.s8) {
            if context.isPushToTalk {
                Text("PTT")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.textDim)
                Text("·")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.textFaint)
            }

            Text(context.transmissionLabel)
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .foregroundStyle(context.isPushToTalk ? AuraTheme.Colors.accent : AuraTheme.Colors.textDim)

            Spacer()

            if context.isPTTHeld, let since = context.pttHeldSince {
                TimelineView(.periodic(from: .now, by: 1)) { timeline in
                    let held = Int(max(0, timeline.date.timeIntervalSince(since)))
                    Text(String(format: "held %d:%02d", held / 60, held % 60))
                        .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                }
                .transition(.opacity)
            }
        }
        .padding(.horizontal, AuraTheme.Spacing.s12)
        .padding(.vertical, AuraTheme.Spacing.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: AuraTheme.Radii.r20)
                .fill(Color.white.opacity(context.isPTTHeld ? 0.12 : 0.05))
        }
        .overlay {
            RoundedRectangle(cornerRadius: AuraTheme.Radii.r20)
                .strokeBorder(
                    context.isPTTHeld ? AuraTheme.Colors.accent.opacity(0.4) : Color.white.opacity(0.08),
                    lineWidth: 1
                )
        }
        .animation(.easeInOut(duration: 0.12), value: context.isPTTHeld)
    }
}
