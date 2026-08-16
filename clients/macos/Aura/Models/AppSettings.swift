import Foundation
import Combine

enum AuraThemeType: String, CaseIterable, Codable {
    case zenith = "zenith"
    case frost = "frost"
    case bloom = "bloom"
    
    var displayName: String {
        switch self {
        case .zenith: return "Aura Zenith"
        case .frost: return "Chromatic Frost"
        case .bloom: return "Elysian Bloom"
        }
    }
}

/// Which column of the channel window gets the width.
///
/// Chat-first is the everyday arrangement: people read and type, voice is
/// ambient in a narrow rail. Voice-focus is for when voice *is* the event.
/// Persisted globally per user, not per channel.
enum AuraLayoutMode: String, CaseIterable, Codable {
    case chatFirst = "chat_first"
    case voiceFocus = "voice_focus"

    var displayName: String {
        switch self {
        case .chatFirst: return "Chat-first"
        case .voiceFocus: return "Voice-focus"
        }
    }

    /// SF Symbol standing for this mode in the channel header toggle.
    var iconName: String {
        switch self {
        case .chatFirst: return "rectangle.split.3x1"
        case .voiceFocus: return "mic.circle"
        }
    }

    var other: AuraLayoutMode {
        self == .chatFirst ? .voiceFocus : .chatFirst
    }
}

class AppSettings: ObservableObject {
    @Published var theme: AuraThemeType = .zenith
    @Published var layoutMode: AuraLayoutMode = .chatFirst
    @Published var trustedFingerprints: [String: String] = [:]

    private let defaults = UserDefaults.standard
    private let themeKey = "AuraThemeSelection"
    private let layoutModeKey = "AuraLayoutMode"
    private let fingerprintsKey = "AuraTrustedFingerprints"

    static let shared = AppSettings()
    
    private init() {
        loadSettings()
    }
    
    func loadSettings() {
        if let themeString = defaults.string(forKey: themeKey),
           let savedTheme = AuraThemeType(rawValue: themeString) {
            theme = savedTheme
        }
        
        if let modeString = defaults.string(forKey: layoutModeKey),
           let savedMode = AuraLayoutMode(rawValue: modeString) {
            layoutMode = savedMode
        }

        if let savedFingerprints = defaults.dictionary(forKey: fingerprintsKey) as? [String: String] {
            trustedFingerprints = savedFingerprints
        }
    }

    func saveSettings() {
        defaults.set(theme.rawValue, forKey: themeKey)
        defaults.set(layoutMode.rawValue, forKey: layoutModeKey)
        defaults.set(trustedFingerprints, forKey: fingerprintsKey)
    }

    /// Flips the channel window between Chat-first and Voice-focus and
    /// persists the choice immediately — the setting outlives the session.
    func toggleLayoutMode() {
        layoutMode = layoutMode.other
        saveSettings()
    }

    func trustFingerprint(host: String, fingerprint: String) {
        trustedFingerprints[host.lowercased()] = fingerprint
        saveSettings()
    }
    
    func isFingerprintTrusted(host: String, fingerprint: String) -> Bool {
        return trustedFingerprints[host.lowercased()] == fingerprint
    }
}
