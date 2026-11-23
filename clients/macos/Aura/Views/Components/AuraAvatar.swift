import SwiftUI

/// Circular avatar with the initial-letter fallback. The initial circle is
/// the resting state, never a spinner (handoff §6 "While loading").
struct AuraAvatar: View {
    let name: String
    let data: Data?
    let size: CGFloat
    var dimmed = false

    private var initial: String {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "?" : String(trimmed.prefix(1)).uppercased()
    }

    var body: some View {
        Group {
            if let data, !data.isEmpty, let image = NSImage(data: data) {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
            } else {
                Circle()
                    .fill(AuraTheme.Gradients.primary)
                    .overlay {
                        Text(initial)
                            .font(AuraTheme.Typography.ui(size * 0.44, weight: .bold))
                            .foregroundStyle(.white)
                    }
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .opacity(dimmed ? 0.5 : 1)
        .accessibilityHidden(true)
    }
}

/// Small capsule action used by the profile card and banners — radius 16,
/// glass-rim, `auraFluidHover`.
struct AuraChipButton: View {
    let title: String
    var systemImage: String?
    var isActive = false
    var tint: Color?
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: AuraTheme.Spacing.s5) {
                if let systemImage {
                    Image(systemName: systemImage)
                        .font(.system(size: 10, weight: .semibold))
                }
                Text(title)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .medium))
            }
            .foregroundStyle(tint ?? AuraTheme.Colors.text)
            .padding(.horizontal, AuraTheme.Spacing.s12)
            .padding(.vertical, 6)
            .background {
                RoundedRectangle(cornerRadius: AuraTheme.Radii.r16, style: .continuous)
                    .fill(isActive ? AuraTheme.Colors.primary.opacity(0.2) : Color.white.opacity(0.06))
            }
            .overlay {
                RoundedRectangle(cornerRadius: AuraTheme.Radii.r16, style: .continuous)
                    .strokeBorder((tint ?? AuraTheme.Colors.panelBorder).opacity(tint == nil ? 1 : 0.4), lineWidth: 0.5)
            }
            .contentShape(RoundedRectangle(cornerRadius: AuraTheme.Radii.r16))
        }
        .buttonStyle(.plain)
        .auraFluidHover()
    }
}
