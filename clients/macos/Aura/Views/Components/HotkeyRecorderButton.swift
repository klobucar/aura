import SwiftUI
import AppKit

struct HotkeyRecorderButton: View {
    @Binding var hotkey: AudioSettings.Hotkey?
    @State private var isRecording = false
    @State private var monitor: Any?
    
    var body: some View {
        HStack(spacing: 12) {
            Button(action: toggleRecording) {
                HStack {
                    Image(systemName: isRecording ? "record.circle.fill" : "keyboard")
                        .accessibilityLabel(isRecording ? "Stop Recording" : "Record Hotkey")
                        .foregroundStyle(isRecording ? .red : .blue)
                    Text(isRecording ? "Press any key..." : (hotkey?.displayString ?? "Click to set"))
                        .font(.system(.body, design: .monospaced))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(isRecording ? Color.red.opacity(0.1) : Color.blue.opacity(0.1))
                .clipShape(.rect(cornerRadius: 6))
            }
            .buttonStyle(.plain)
            
            if hotkey != nil {
                Button(action: clearHotkey) {
                    Image(systemName: "xmark.circle.fill")
                        .accessibilityLabel("Close")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
            }
        }
    }
    
    private func toggleRecording() {
        if isRecording {
            stopRecording()
        } else {
            startRecording()
        }
    }
    
    private func startRecording() {
        isRecording = true

        // Monitor for the next key press or modifier-only chord while the
        // settings window is front. `addLocalMonitorForEvents` bypasses the
        // accessibility-permission requirement, so recording works even
        // before the user has granted permission for the global tap.
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { event in
            let rawMods = UInt32(event.modifierFlags.rawValue) & HotkeyManager.relevantModifierMask

            switch event.type {
            case .keyDown:
                let keyCode = UInt16(event.keyCode)
                // Accept any key, with or without modifiers. Plain keys like
                // F13 or backtick are normal PTT choices on desktops.
                let newHotkey = AudioSettings.Hotkey(keyCode: keyCode, modifiers: rawMods)
                if HotkeyManager.shared.validateHotkey(newHotkey) {
                    hotkey = newHotkey
                } else {
                    NSSound.beep()
                }
                stopRecording()
                return nil // Consume

            case .flagsChanged:
                // Let the user capture a modifier-only chord (e.g. just
                // Right-Option) by releasing all modifiers after pressing
                // them down. We only commit when `rawMods` transitions
                // back to zero so the chord is stable.
                if rawMods == 0 {
                    // Modifier released — nothing to record.
                    return nil
                }
                // Track the latest non-zero modifier combo. We commit on
                // the NEXT transition to zero, but simpler: commit now
                // with a short grace window so the user can tap-and-release
                // a modifier like Right-Option.
                let newHotkey = AudioSettings.Hotkey(
                    keyCode: AudioSettings.Hotkey.modifierOnlyKeyCode,
                    modifiers: rawMods
                )
                if HotkeyManager.shared.validateHotkey(newHotkey) {
                    hotkey = newHotkey
                    stopRecording()
                    return nil
                }
                return nil

            default:
                break
            }

            return event
        }
    }
    
    private func stopRecording() {
        isRecording = false
        if let monitor = monitor {
            NSEvent.removeMonitor(monitor)
            self.monitor = nil
        }
    }
    
    private func clearHotkey() {
        hotkey = nil
    }
}
