import SwiftUI

// MARK: - Aura Material Enum

enum AuraMaterial {
    case hudWindow, ultraThin, thin, regular, thick
    case sidebar, header, popover
    
    var nsValue: NSVisualEffectView.Material {
        switch self {
        case .hudWindow: return .popover // Popover is adaptive (light/dark), hudWindow is always dark
        case .ultraThin: return .selection
        case .thin: return .contentBackground
        case .regular: return .windowBackground
        case .thick: return .windowBackground
        case .sidebar: return .sidebar
        case .header: return .headerView
        case .popover: return .popover
        }
    }
}

// MARK: - Glass Modifier

struct AuraGlassModifier: ViewModifier {
    let cornerRadius: CGFloat
    let material: AuraMaterial
    
    func body(content: Content) -> some View {
        content
            .background(
                VisualEffectBlur(auraMaterial: material, blendingMode: .behindWindow)
                    .clipShape(RoundedRectangle(cornerRadius: cornerRadius))
            )
            .overlay {
                RoundedRectangle(cornerRadius: cornerRadius)
                    .strokeBorder(
                        LinearGradient(
                            colors: [
                                AuraTheme.Colors.glassBorder,
                                AuraTheme.Colors.glassHighlight.opacity(0.5),
                                .clear,
                                Color.black.opacity(0.05),
                                Color.black.opacity(0.15)
                            ],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        ),
                        lineWidth: 0.6
                    )
                    // Inner rim light for thick glass feel
                    .overlay(
                        RoundedRectangle(cornerRadius: cornerRadius)
                            .stroke(
                                LinearGradient(
                                    colors: [AuraTheme.Colors.rimLight, .clear, .clear],
                                    startPoint: .topLeading,
                                    endPoint: .bottomTrailing
                                ),
                                lineWidth: 1.5
                            )
                            .blur(radius: 0.5)
                            .padding(0.5)
                    )
            }
            .modifier(AuraTheme.Shadows.glass())
    }
}

// MARK: - Fluid Hover Modifier

struct AuraFluidHoverModifier: ViewModifier {
    @State private var isHovering = false
    var scale: CGFloat = 1.02
    var brightness: Double = 0.05
    
    func body(content: Content) -> some View {
        content
            .scaleEffect(isHovering ? scale : 1.0)
            .brightness(isHovering ? brightness : 0)
            .animation(.spring(response: 0.3, dampingFraction: 0.7), value: isHovering)
            .onHover { hovering in
                isHovering = hovering
            }
    }
}

// MARK: - Active Pulse Modifier

struct AuraActivePulseModifier: ViewModifier {
    let isActive: Bool
    @State private var pulse = false
    
    func body(content: Content) -> some View {
        content
            .overlay {
                if isActive {
                    RoundedRectangle(cornerRadius: AuraTheme.Layout.cornerRadius)
                        .stroke(AuraTheme.Colors.primary.opacity(pulse ? 0.3 : 0.1), lineWidth: 2)
                        .scaleEffect(pulse ? 1.05 : 1.0)
                        .blur(radius: pulse ? 2 : 1)
                        .onAppear {
                            withAnimation(.easeInOut(duration: 1.5).repeatForever(autoreverses: true)) {
                                pulse = true
                            }
                        }
                }
            }
    }
}

// MARK: - Aura Glow Modifier

struct AuraGlowModifier: ViewModifier {
    let color: Color
    let radius: CGFloat
    @State private var breathe = false
    
    func body(content: Content) -> some View {
        content
            .shadow(color: color.opacity(breathe ? 0.6 : 0.3), radius: breathe ? radius * 1.5 : radius, x: 0, y: 0)
            .animation(.easeInOut(duration: 3.0).repeatForever(autoreverses: true), value: breathe)
            .onAppear {
                breathe = true
            }
    }
}

// MARK: - View Extensions

extension View {
    /// Applies a premium glassmorphic effect with a thin light-catching border.
    func auraGlass(cornerRadius: CGFloat = AuraTheme.Layout.glassCornerRadius, material: AuraMaterial = .hudWindow) -> some View {
        self.modifier(AuraGlassModifier(cornerRadius: cornerRadius, material: material))
    }
    
    /// Adds a fluid scale and brightness effect on hover.
    func auraFluidHover(scale: CGFloat = 1.02, brightness: Double = 0.05) -> some View {
        self.modifier(AuraFluidHoverModifier(scale: scale, brightness: brightness))
    }
    
    /// Specialized modifier for chat message bubbles.
    func auraMessageBubble(isOutgoing: Bool) -> some View {
        let content = self
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(
                Group {
                    if isOutgoing {
                        AuraTheme.Gradients.lushIndigo
                    } else {
                        VisualEffectBlur(auraMaterial: .thin, blendingMode: .withinWindow)
                    }
                }
            )
            .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .strokeBorder(
                        LinearGradient(
                            colors: [
                                isOutgoing ? Color.white.opacity(0.2) : AuraTheme.Colors.glassBorder,
                                .clear
                            ],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        ),
                        lineWidth: 0.5
                    )
            }
        
        return Group {
            if isOutgoing {
                content.modifier(AuraTheme.Shadows.soft())
            } else {
                content.modifier(AuraTheme.Shadows.glass())
            }
        }
    }
    
    /// Standardized premium card style.
    func auraCard() -> some View {
        self
            .padding(AuraTheme.Layout.cardPadding)
            .auraGlass()
            .modifier(AuraTheme.Shadows.soft())
    }
    
    /// Native macOS 26 Liquid Glass effect.
    /// Uses system .glassEffect() when available, falls back to auraGlass().
    @ViewBuilder
    func liquidGlass(cornerRadius: CGFloat = AuraTheme.Layout.liquidGlassCornerRadius) -> some View {
        if #available(macOS 26, *) {
            self
                .glassEffect(.regular.interactive(), in: .rect(cornerRadius: cornerRadius))
        } else {
            self.auraGlass(cornerRadius: cornerRadius, material: .hudWindow)
        }
    }
    
    /// Standardized section container for Settings and Profile views.
    func auraGlassSection(title: String? = nil, icon: String? = nil) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            if let title = title {
                HStack(spacing: 8) {
                    if let icon = icon {
                        Image(systemName: icon)
                            .foregroundStyle(AuraTheme.Colors.primary)
                            .font(.system(size: 11, weight: .bold))
                    }
                    Text(title.uppercased())
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(.secondary)
                        .kerning(1)
                }
            }
            
            self
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(20)
        .background(AuraTheme.Colors.liquidFrosted)
        .overlay {
            RoundedRectangle(cornerRadius: AuraTheme.Layout.glassCornerRadius)
                .strokeBorder(AuraTheme.Colors.glassBorder.opacity(0.5), lineWidth: 0.5)
        }
    }
    
    /// Specialized pulse for active UI elements.
    func auraActivePulse(isActive: Bool) -> some View {
        self.modifier(AuraActivePulseModifier(isActive: isActive))
    }
    
    /// Applies a soft, ethereal colorful glow.
    func auraGlow(color: Color = AuraTheme.Colors.primary, radius: CGFloat = 8) -> some View {
        self.modifier(AuraGlowModifier(color: color, radius: radius))
    }
}

// MARK: - Glass Button Style

struct AuraGlassButtonStyle: ButtonStyle {
    @State private var isHovering = false
    
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .background {
                ZStack {
                    VisualEffectBlur(auraMaterial: .thin, blendingMode: .behindWindow)
                    
                    if isHovering {
                        Color.white.opacity(0.08)
                    }
                    if configuration.isPressed {
                        Color.black.opacity(0.1)
                    }
                }
            }
            .clipShape(Capsule())
            .overlay {
                Capsule()
                    .strokeBorder(AuraTheme.Colors.glassBorder, lineWidth: 0.5)
            }
            .scaleEffect(configuration.isPressed ? 0.96 : (isHovering ? 1.02 : 1.0))
            .animation(.spring(response: 0.2, dampingFraction: 0.7), value: configuration.isPressed)
            .animation(.spring(response: 0.3, dampingFraction: 0.7), value: isHovering)
            .onHover { hovering in
                isHovering = hovering
            }
    }
}

extension ButtonStyle where Self == AuraGlassButtonStyle {
    static var auraGlass: AuraGlassButtonStyle { AuraGlassButtonStyle() }
}

// MARK: - VisualEffectBlur Helper

struct VisualEffectBlur: NSViewRepresentable {
    var material: NSVisualEffectView.Material
    var blendingMode: NSVisualEffectView.BlendingMode
    
    init(material: NSVisualEffectView.Material, blendingMode: NSVisualEffectView.BlendingMode = .behindWindow) {
        self.material = material
        self.blendingMode = blendingMode
    }
    
    init(auraMaterial: AuraMaterial, blendingMode: NSVisualEffectView.BlendingMode = .behindWindow) {
        self.material = auraMaterial.nsValue
        self.blendingMode = blendingMode
    }
    
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = .active
        return view
    }
    
    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {
        nsView.material = material
        nsView.blendingMode = blendingMode
    }
}
