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
    
    private let defaults = UserDefaults.standard
    private let themeKey = "AuraThemeSelection"
    
    static let shared = AppSettings()
    
    private init() {
        loadSettings()
    }
    
    func loadSettings() {
        if let themeString = defaults.string(forKey: themeKey),
           let savedTheme = AuraThemeType(rawValue: themeString) {
            theme = savedTheme
        }
    }
    
    func saveSettings() {
        defaults.set(theme.rawValue, forKey: themeKey)
    }
}
