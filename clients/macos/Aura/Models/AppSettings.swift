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

class AppSettings: ObservableObject {
    @Published var theme: AuraThemeType = .zenith
    @Published var trustedFingerprints: [String: String] = [:]
    
    private let defaults = UserDefaults.standard
    private let themeKey = "AuraThemeSelection"
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
        
        if let savedFingerprints = defaults.dictionary(forKey: fingerprintsKey) as? [String: String] {
            trustedFingerprints = savedFingerprints
        }
    }
    
    func saveSettings() {
        defaults.set(theme.rawValue, forKey: themeKey)
        defaults.set(trustedFingerprints, forKey: fingerprintsKey)
    }
    
    func trustFingerprint(host: String, fingerprint: String) {
        trustedFingerprints[host.lowercased()] = fingerprint
        saveSettings()
    }
    
    func isFingerprintTrusted(host: String, fingerprint: String) -> Bool {
        return trustedFingerprints[host.lowercased()] == fingerprint
    }
}
