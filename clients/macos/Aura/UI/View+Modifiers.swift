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
