import SwiftUI
import AppKit

struct AuraTheme {
    // MARK: - Dynamic Palette
    
    private static var currentTheme: AuraThemeType {
        AppSettings.shared.theme
    }
    
    // MARK: - Colors
    
    struct Colors {
        static var background: Color {
            Color(nsColor: NSColor(name: nil) { appearance in
                let dark = appearance.bestMatch(from: [NSAppearance.Name.darkAqua, NSAppearance.Name.aqua]) == NSAppearance.Name.darkAqua
                switch (currentTheme, dark) {
                case (.zenith, true): return NSColor(red: 0.04, green: 0.04, blue: 0.05, alpha: 1.0)
                case (.zenith, false): return NSColor(red: 0.96, green: 0.96, blue: 0.98, alpha: 1.0)
                case (.frost, true): return NSColor(red: 0.05, green: 0.07, blue: 0.1, alpha: 1.0)
                case (.frost, false): return NSColor(red: 0.94, green: 0.97, blue: 1.0, alpha: 1.0)
                case (.bloom, true): return NSColor(red: 0.12, green: 0.08, blue: 0.15, alpha: 1.0)
                case (.bloom, false): return NSColor(red: 0.99, green: 0.96, blue: 0.98, alpha: 1.0)
                }
            })
        }
        
        static var backgroundGradient: LinearGradient {
            LinearGradient(
                colors: [
                    background,
                    Color(nsColor: NSColor(name: nil) { appearance in
                        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                        return dark ? NSColor(white: 0.02, alpha: 1.0) : NSColor(red: 0.9, green: 0.92, blue: 0.98, alpha: 1.0)
                    })
                ],
                startPoint: .top,
                endPoint: .bottom
            )
        }
        
        static var sidebarBackground: Color {
            Color(nsColor: .controlBackgroundColor).opacity(0.3)
        }
        
        // Brand Colors
        static var primary: Color {
            Color(nsColor: NSColor(name: nil) { appearance in
                let dark = appearance.bestMatch(from: [NSAppearance.Name.darkAqua, NSAppearance.Name.aqua]) == NSAppearance.Name.darkAqua
                switch (currentTheme, dark) {
                case (.zenith, true): return NSColor(red: 0.2, green: 0.63, blue: 1.0, alpha: 1.0)
                case (.zenith, false): return NSColor(red: 0.1, green: 0.45, blue: 0.9, alpha: 1.0)
                case (.frost, true): return NSColor(red: 0.0, green: 0.82, blue: 0.83, alpha: 1.0)
                case (.frost, false): return NSColor(red: 0.0, green: 0.6, blue: 0.7, alpha: 1.0)
                case (.bloom, true): return NSColor(red: 0.28, green: 0.2, blue: 0.83, alpha: 1.0)
                case (.bloom, false): return NSColor(red: 0.4, green: 0.1, blue: 0.7, alpha: 1.0)
                }
            })
        }
        
        static var secondary: Color {
            Color(nsColor: NSColor(name: nil) { appearance in
                let dark = appearance.bestMatch(from: [NSAppearance.Name.darkAqua, NSAppearance.Name.aqua]) == NSAppearance.Name.darkAqua
                switch (currentTheme, dark) {
                case (.zenith, true): return NSColor(red: 0.55, green: 0.33, blue: 1.0, alpha: 1.0)
                case (.zenith, false): return NSColor(red: 0.4, green: 0.2, blue: 0.8, alpha: 1.0)
                case (.frost, true): return NSColor(red: 0.18, green: 0.52, blue: 0.87, alpha: 1.0)
                case (.frost, false): return NSColor(red: 0.1, green: 0.4, blue: 0.8, alpha: 1.0)
                case (.bloom, true): return NSColor(red: 1.0, green: 0.52, blue: 0.63, alpha: 1.0)
                case (.bloom, false): return NSColor(red: 0.9, green: 0.3, blue: 0.5, alpha: 1.0)
                }
            })
        }
        
        static var accent: Color {
            Color(nsColor: NSColor(name: nil) { appearance in
                let dark = appearance.bestMatch(from: [NSAppearance.Name.darkAqua, NSAppearance.Name.aqua]) == NSAppearance.Name.darkAqua
                switch (currentTheme, dark) {
                case (.zenith, true): return NSColor(red: 0.0, green: 1.0, blue: 0.76, alpha: 1.0)
                case (.zenith, false): return NSColor(red: 0.0, green: 0.7, blue: 0.5, alpha: 1.0)
                case (.frost, true): return NSColor(red: 1.0, green: 0.62, blue: 0.26, alpha: 1.0)
                case (.frost, false): return NSColor(red: 0.9, green: 0.5, blue: 0.1, alpha: 1.0)
                case (.bloom, true): return NSColor(red: 0.33, green: 0.94, blue: 0.77, alpha: 1.0)
                case (.bloom, false): return NSColor(red: 0.1, green: 0.6, blue: 0.4, alpha: 1.0)
                }
            })
        }
        
        static var lushIndigo: Color {
            Color(nsColor: NSColor(name: nil) { appearance in
                let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                switch (currentTheme, dark) {
                case (.zenith, true): return NSColor(red: 0.1, green: 0.3, blue: 1.0, alpha: 1.0)
                case (.zenith, false): return NSColor(red: 0.2, green: 0.4, blue: 0.9, alpha: 1.0)
                case (.frost, true): return NSColor(red: 0.0, green: 0.6, blue: 0.8, alpha: 1.0)
                case (.frost, false): return NSColor(red: 0.1, green: 0.5, blue: 0.7, alpha: 1.0)
                case (.bloom, true): return NSColor(red: 0.4, green: 0.2, blue: 0.9, alpha: 1.0)
                case (.bloom, false): return NSColor(red: 0.5, green: 0.3, blue: 0.8, alpha: 1.0)
                }
            })
        }
        
        static var lushMint: Color {
            accent.opacity(0.8)
        }

        // MARK: Semantic Text Ramp
        //
        // The design handoff specifies one neutral (dark, light) pair for the
        // text ramp — those are the zenith values below. frost and bloom nudge
        // the same luminance toward their own background hue (frost is blue-
        // black, bloom is violet-black) so text reads as part of the surface
        // instead of a grey sticker on top of it.

        /// Primary body/label color.
        static var text: Color {
            themed(zenith: (0xE8EAEE, 0x12141A),
                   frost: (0xE4EAF2, 0x101620),
                   bloom: (0xEEE8F2, 0x18121E))
        }

        /// One step down from `text` — secondary labels that are still content.
        static var textMid: Color {
            themed(zenith: (0xDDE2EA, 0x2A2F38),
                   frost: (0xD8E1EC, 0x252D38),
                   bloom: (0xE4DAEA, 0x302838))
        }

        /// Metadata, mono section labels, inactive icons.
        static var textDim: Color {
            themed(zenith: (0xB3BAC6, 0x5A616D),
                   frost: (0xACB7C6, 0x545E6D),
                   bloom: (0xBBB0C6, 0x615A6D))
        }

        /// The faintest legible step — hints and disabled text.
        /// The handoff gives a single value; it holds up on all six surfaces.
        static var textFaint: Color {
            themed(zenith: (0x8B929E, 0x8B929E))
        }

        // MARK: Semantic Status Colors
        //
        // The handoff tabulates dark values only. Light variants are darkened
        // to keep contrast on the light backgrounds. frost shifts `caution`
        // yellower because frost's `accent` is already orange — an amber
        // overlap segment on an orange lane would be unreadable.

        /// Reply-context sender, inline links.
        static var link: Color {
            themed(zenith: (0x5AB4FF, 0x1A6FCC))
        }

        /// Muted state.
        static var warn: Color {
            themed(zenith: (0xFF9D78, 0xC2542A))
        }

        /// Key changed / unverified, and the "talked over" overlap segment.
        static var caution: Color {
            themed(zenith: (0xFFBD2E, 0xB37800),
                   frost: (0xFFE04D, 0x9E7A00))
        }

        /// Higher-contrast text on top of a `cautionFill` surface.
        static var cautionText: Color {
            themed(zenith: (0xFFD489, 0x8A5C00),
                   frost: (0xFFEBA3, 0x7A5E00))
        }

        /// Background wash for caution rows and chips.
        static var cautionFill: Color { caution.opacity(0.12) }

        /// Stroke for caution rows and chips.
        static var cautionBorder: Color { caution.opacity(0.30) }

        /// Destructive actions and blocked peers.
        static var danger: Color {
            themed(zenith: (0xFF5F56, 0xCC2E26))
        }

        static var dangerFill: Color { danger.opacity(0.16) }

        static var dangerBorder: Color { danger.opacity(0.38) }

        /// Healthy latency.
        static var good: Color {
            themed(zenith: (0x27C93F, 0x1A8C2E))
        }

        // Materials
        static var glassBorder: Color {
            Color(nsColor: NSColor(name: nil) { appearance in
                return appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? 
                    NSColor(white: 1.0, alpha: 0.18) : 
                    NSColor(white: 1.0, alpha: 0.4)
            })
        }
        
        static var glassHighlight: Color {
            Color(nsColor: NSColor(name: nil) { appearance in
                return appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? 
                    NSColor(white: 1.0, alpha: 0.16) : 
                    NSColor(white: 1.0, alpha: 0.45)
            })
        }
        
        static var rimLight: Color {
            Color.white.opacity(0.3)
        }
        
        /// Subtle overlay tint for Liquid Glass surfaces
        static var liquidOverlay: Color {
            Color.white.opacity(0.12)
        }
        
        static var liquidFrosted: Color {
            Color(nsColor: NSColor(name: nil) { appearance in
                return appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? 
                    NSColor(white: 1.0, alpha: 0.05) : 
                    NSColor(red: 0.9, green: 0.95, blue: 1.0, alpha: 0.25)
            })
        }
        
        static var ultraFrosted: Color {
            Color.white.opacity(0.02)
        }
        
        static var auraSecondaryGlow: Color {
            Color(nsColor: .systemPurple).opacity(0.3)
        }
        
        static var auraTertiaryGlow: Color {
            Color(nsColor: .systemMint).opacity(0.2)
        }

        // MARK: - Semantic Palette Helpers

        /// Builds an appearance- and theme-aware color from one `(dark, light)`
        /// hex pair per theme. `frost` / `bloom` fall back to `zenith` — most
        /// semantic tokens are shared, and passing only `zenith` documents
        /// "the handoff gives one value for every theme".
        private static func themed(
            zenith: (dark: UInt32, light: UInt32),
            frost: (dark: UInt32, light: UInt32)? = nil,
            bloom: (dark: UInt32, light: UInt32)? = nil
        ) -> Color {
            Color(nsColor: NSColor(name: nil) { appearance in
                let isDark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                let pair: (dark: UInt32, light: UInt32)
                switch currentTheme {
                case .zenith: pair = zenith
                case .frost: pair = frost ?? zenith
                case .bloom: pair = bloom ?? zenith
                }
                return nsColor(hex: isDark ? pair.dark : pair.light)
            })
        }

        /// 24-bit `0xRRGGBB` → opaque `NSColor`.
        private static func nsColor(hex: UInt32) -> NSColor {
            NSColor(
                red: CGFloat((hex >> 16) & 0xFF) / 255.0,
                green: CGFloat((hex >> 8) & 0xFF) / 255.0,
                blue: CGFloat(hex & 0xFF) / 255.0,
                alpha: 1.0
            )
        }
    }
    
    // MARK: - Gradients
    
    struct Gradients {
        static var primary: LinearGradient {
            LinearGradient(
                colors: [Colors.primary, Colors.secondary],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
        
        static var lushIndigo: LinearGradient {
            LinearGradient(
                colors: [Colors.lushIndigo, Colors.primary],
                startPoint: .top,
                endPoint: .bottom
            )
        }
        
        static var lushMint: LinearGradient {
            LinearGradient(
                colors: [Colors.lushMint, Colors.accent],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
        
        static let surface = LinearGradient(
            colors: [Color.white.opacity(0.08), Color.clear],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
    }
    
    // MARK: - Shadows
    
    struct Shadows {
        static func soft() -> some ViewModifier {
            ShadowModifier(color: .black.opacity(0.1), radius: 8, x: 0, y: 4)
        }
        
        static func deep() -> some ViewModifier {
            ShadowModifier(color: .black.opacity(0.2), radius: 20, x: 0, y: 12)
        }
        
        static func glass() -> some ViewModifier {
            ShadowModifier(color: .black.opacity(0.15), radius: 15, x: 0, y: 8)
        }
        
        static func glow(color: Color) -> some ViewModifier {
            ShadowModifier(color: color.opacity(0.5), radius: 12, x: 0, y: 0)
        }
    }
    
    // MARK: - Layout Constants
    
    struct Layout {
        static let cornerRadius: CGFloat = 12
        static let glassCornerRadius: CGFloat = 16
        static let liquidGlassCornerRadius: CGFloat = 20
        static let cardPadding: CGFloat = 12

        // MARK: Layout-mode column widths

        /// Roster sidebar — the same width in both layout modes.
        static let rosterWidth: CGFloat = 268
        /// Voice rail width in Chat-first mode.
        static let voiceRailWidth: CGFloat = 336
        /// Chat column width in Voice-focus mode.
        static let chatNarrowWidth: CGFloat = 320
        /// Unified toolbar height for the roster and channel headers.
        static let headerHeight: CGFloat = 52
    }

    // MARK: - Spacing Scale

    /// The handoff's spacing ramp, named by value so a spec line like
    /// "padding 13×14, gap 9" transcribes without a lookup table.
    struct Spacing {
        static let s2: CGFloat = 2
        static let s3: CGFloat = 3
        static let s5: CGFloat = 5
        static let s8: CGFloat = 8
        static let s9: CGFloat = 9
        static let s11: CGFloat = 11
        static let s12: CGFloat = 12
        static let s14: CGFloat = 14
        static let s16: CGFloat = 16
        static let s18: CGFloat = 18
        static let s20: CGFloat = 20
        static let s22: CGFloat = 22
    }

    // MARK: - Radii Scale

    /// Corner radii ramp. The 4/6/8 steps exist for parity with the Avalonia
    /// client's tighter geometry; macOS surfaces start at 10.
    struct Radii {
        static let r4: CGFloat = 4
        static let r6: CGFloat = 6
        static let r8: CGFloat = 8
        static let r10: CGFloat = 10
        static let r14: CGFloat = 14
        static let r16: CGFloat = 16
        static let r18: CGFloat = 18
        static let r20: CGFloat = 20
        /// Standard tile / panel radius on macOS.
        static let r22: CGFloat = 22
        /// Composer.
        static let r24: CGFloat = 24
        /// Command HUD and shell.
        static let r26: CGFloat = 26
    }

    // MARK: - Type Scale

    /// The mock's sub-pixel sizes (9.5, 12.5, 13.5, 14.5) are artifacts of a
    /// scaled HTML canvas. The handoff rounds them to the SwiftUI ramp
    /// 10/11/12/13/15/19, which is what these constants are.
    struct Typography {
        static let t10: CGFloat = 10
        static let t11: CGFloat = 11
        static let t12: CGFloat = 12
        static let t13: CGFloat = 13
        static let t15: CGFloat = 15
        static let t19: CGFloat = 19

        /// `0.14em` at the 10pt mono section-label size.
        static let monoLabelKerning: CGFloat = 1.4
        /// `−0.01em` on 15pt-and-up titles.
        static let titleKerning: CGFloat = -0.15

        /// `IBM Plex Mono` if the user happens to have it installed, SF Mono
        /// otherwise. Nothing is bundled — the handoff's "ship it or
        /// substitute" is answered with "substitute, gracefully".
        private static let installedMonoFamily: String? = {
            ["IBM Plex Mono", "IBMPlexMono"].first { NSFont(name: $0, size: 10) != nil }
        }()

        /// Mono face for numerics, fingerprints, keycaps and section labels.
        static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
            if let family = installedMonoFamily {
                return .custom(family, fixedSize: size).weight(weight)
            }
            return .system(size: size, weight: weight, design: .monospaced)
        }

        /// System UI face. `weight: .semibold` stands in for the handoff's 650 —
        /// SwiftUI only exposes named weights, and 650 sits between 600 and 700.
        static func ui(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
            .system(size: size, weight: weight)
        }
    }

    // MARK: - Animations

    struct Motion {
        /// Layout-mode column swap — widths only.
        static let modeSwap: Animation = .spring(response: 0.32, dampingFraction: 0.82)
        /// Content that appears/disappears across the mode swap.
        static let crossFade: Animation = .easeInOut(duration: 0.16)
        /// Speaking-ring pulse.
        static let speakingPulse: Animation = .easeInOut(duration: 1.4).repeatForever(autoreverses: true)
    }
}

// Internal helper for shadow modifier
private struct ShadowModifier: ViewModifier {
    let color: Color
    let radius: CGFloat
    let x: CGFloat
    let y: CGFloat
    
    func body(content: Content) -> some View {
        content.shadow(color: color, radius: radius, x: x, y: y)
    }
}
