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
