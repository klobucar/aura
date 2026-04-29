import Foundation
import Combine
import CoreGraphics

class AudioSettings: ObservableObject {
    // MARK: - Transmission Mode
    
    enum TransmissionMode: String, Codable, CaseIterable {
        case pushToTalk = "ptt"
        case alwaysOn = "always_on"
        case voiceActivation = "vad"
        
        var displayName: String {
            switch self {
            case .pushToTalk: return "Push-to-Talk"
            case .alwaysOn: return "Always On"
            case .voiceActivation: return "Voice Activation"
            }
        }
    }
    
    // MARK: - Hotkey
    
    struct Hotkey: Codable, Equatable {
        let keyCode: UInt16
        let modifiers: UInt32 // masked CGEventFlags rawValue

        /// Sentinel used when the hotkey is modifier-only (e.g. Right-Option).
        /// 0xFFFF is outside the valid macOS virtual key-code range.
        static let modifierOnlyKeyCode: UInt16 = 0xFFFF

        var isModifierOnly: Bool { keyCode == Self.modifierOnlyKeyCode }

        var displayString: String {
            var parts: [String] = []

            if modifiers & UInt32(CGEventFlags.maskControl.rawValue) != 0 {
                parts.append("⌃")
            }
            if modifiers & UInt32(CGEventFlags.maskAlternate.rawValue) != 0 {
                parts.append("⌥")
            }
            if modifiers & UInt32(CGEventFlags.maskShift.rawValue) != 0 {
                parts.append("⇧")
            }
            if modifiers & UInt32(CGEventFlags.maskCommand.rawValue) != 0 {
                parts.append("⌘")
            }

            if isModifierOnly {
                if parts.isEmpty { parts.append("(unset)") }
            } else if let keyChar = keyCodeToString(keyCode) {
                parts.append(keyChar)
            } else {
                parts.append("Key \(keyCode)")
            }

            return parts.joined()
        }
        
        private func keyCodeToString(_ keyCode: UInt16) -> String? {
            // Common key mappings
            switch keyCode {
            case 0x31: return "Space"
            case 0x24: return "Return"
            case 0x30: return "Tab"
            case 0x33: return "Delete"
            case 0x35: return "Esc"
            case 0x7E: return "↑"
            case 0x7D: return "↓"
            case 0x7B: return "←"
            case 0x7C: return "→"
            // A-Z
            case 0x00: return "A"
            case 0x0B: return "B"
            case 0x08: return "C"
            case 0x02: return "D"
            case 0x0E: return "E"
            case 0x03: return "F"
            case 0x05: return "G"
            case 0x04: return "H"
            case 0x22: return "I"
            case 0x26: return "J"
            case 0x28: return "K"
            case 0x25: return "L"
            case 0x2E: return "M"
            case 0x2D: return "N"
            case 0x1F: return "O"
            case 0x23: return "P"
            case 0x0C: return "Q"
            case 0x0F: return "R"
            case 0x01: return "S"
            case 0x11: return "T"
            case 0x20: return "U"
            case 0x09: return "V"
            case 0x0D: return "W"
            case 0x07: return "X"
            case 0x10: return "Y"
            case 0x06: return "Z"
            default: return nil
            }
        }
    }
    
    // MARK: - Properties
    
    @Published var outputDeviceID: String?
    @Published var transmissionMode: TransmissionMode = .voiceActivation
    @Published var vadSensitivity: Float = 0.5 // 0.0 = very sensitive, 1.0 = loud speech only
    @Published var pttHotkey: Hotkey?

    /// VAD detection threshold in dB, derived from `vadSensitivity` slider.
    /// Linear map: 0.0 → -50 dB (sensitive), 1.0 → -20 dB (loud-only).
    var vadThresholdDb: Float {
        -50.0 + (vadSensitivity * 30.0)
    }

    
    // MARK: - Persistence
    
    private let defaults = UserDefaults.standard
    private let outputDeviceKey = "AuraOutputDevice"
    private let transmissionModeKey = "AuraTransmissionMode"
    private let vadSensitivityKey = "AuraVADSensitivity"
    private let pttHotkeyKey = "AuraPTTHotkey"
    
    init() {
        loadSettings()
    }
    
    func loadSettings() {
        outputDeviceID = defaults.string(forKey: outputDeviceKey)
        
        if let modeString = defaults.string(forKey: transmissionModeKey),
           let mode = TransmissionMode(rawValue: modeString) {
            transmissionMode = mode
        }
        
        vadSensitivity = defaults.float(forKey: vadSensitivityKey)
        if vadSensitivity == 0 {
            vadSensitivity = 0.5 // Default if never set
        }
        
        if let hotkeyData = defaults.data(forKey: pttHotkeyKey),
           let hotkey = try? JSONDecoder().decode(Hotkey.self, from: hotkeyData) {
            pttHotkey = hotkey
        }
    }
    
    func saveSettings() {
        defaults.set(outputDeviceID, forKey: outputDeviceKey)
        defaults.set(transmissionMode.rawValue, forKey: transmissionModeKey)
        defaults.set(vadSensitivity, forKey: vadSensitivityKey)
        
        if let hotkey = pttHotkey,
           let hotkeyData = try? JSONEncoder().encode(hotkey) {
            defaults.set(hotkeyData, forKey: pttHotkeyKey)
        } else {
            defaults.removeObject(forKey: pttHotkeyKey)
        }
    }
}
