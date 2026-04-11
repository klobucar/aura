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
        
        // Monitor for next key press
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { event in
            // Capture the keypress
            if event.type == .keyDown {
                let keyCode = UInt16(event.keyCode)
                let modifiers = UInt32(event.modifierFlags.rawValue)
                
                let newHotkey = AudioSettings.Hotkey(keyCode: keyCode, modifiers: modifiers)
                
                // Validate that it has at least one modifier
                if HotkeyManager.shared.validateHotkey(newHotkey) {
                    hotkey = newHotkey
                } else {
                    // Show alert that modifier is required
                    NSSound.beep()
                }
                
                stopRecording()
                return nil // Consume event
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
