import Foundation
import Combine
import Carbon
import AppKit

class HotkeyManager: ObservableObject {
    static let shared = HotkeyManager()
    
    @Published private(set) var isPTTActive = false
    @Published private(set) var hasAccessibilityPermission = false
    
    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var currentHotkey: AudioSettings.Hotkey?
    
    private init() {
        checkAccessibilityPermission()
    }
    
    // MARK: - Accessibility Permission
    
    func checkAccessibilityPermission() {
        hasAccessibilityPermission = AXIsProcessTrusted()
    }
    
    func requestAccessibilityPermission() {
        let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
        AXIsProcessTrustedWithOptions(options)
        
        // Check again after a delay
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { [weak self] in
            self?.checkAccessibilityPermission()
        }
    }
    
    // MARK: - Hotkey Registration
    
    func registerHotkey(_ hotkey: AudioSettings.Hotkey) {
        // Unregister existing hotkey
        unregisterHotkey()
        
        guard hasAccessibilityPermission else {
            print("[HotkeyManager] No accessibility permission")
            return
        }
        
        currentHotkey = hotkey
        
        // Create event tap for global key monitoring
        let eventMask = (1 << CGEventType.keyDown.rawValue) | (1 << CGEventType.keyUp.rawValue) | (1 << CGEventType.flagsChanged.rawValue)
        
        guard let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: CGEventMask(eventMask),
            callback: { (proxy, type, event, refcon) -> Unmanaged<CGEvent>? in
                let manager = Unmanaged<HotkeyManager>.fromOpaque(refcon!).takeUnretainedValue()
                return manager.handleEvent(proxy: proxy, type: type, event: event)
            },
            userInfo: Unmanaged.passUnretained(self).toOpaque()
        ) else {
            print("[HotkeyManager] Failed to create event tap")
            return
        }
        
        eventTap = tap
        runLoopSource = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetCurrent(), runLoopSource, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        
        print("[HotkeyManager] Registered hotkey: \(hotkey.displayString)")
    }
    
    func unregisterHotkey() {
        if let tap = eventTap {
            CGEvent.tapEnable(tap: tap, enable: false)
            if let source = runLoopSource {
                CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, .commonModes)
            }
            eventTap = nil
            runLoopSource = nil
        }
        
        currentHotkey = nil
        isPTTActive = false
    }
    
    // MARK: - Event Handling
    
    private func handleEvent(proxy: CGEventTapProxy, type: CGEventType, event: CGEvent) -> Unmanaged<CGEvent>? {
        guard let hotkey = currentHotkey else {
            return Unmanaged.passRetained(event)
        }
        
        switch type {
        case .keyDown, .keyUp:
            let keyCode = UInt16(event.getIntegerValueField(.keyboardEventKeycode))
            let flags = event.flags
            
            // Check if this matches our hotkey
            if keyCode == hotkey.keyCode && modifiersMatch(flags, hotkey.modifiers) {
                DispatchQueue.main.async { [weak self] in
                    self?.isPTTActive = (type == .keyDown)
                }
                
                // Consume the event (don't pass it through)
                return nil
            }
            
        case .flagsChanged:
            // Handle modifier-only hotkeys or check if modifiers were released
            let flags = event.flags
            if !modifiersMatch(flags, hotkey.modifiers) && isPTTActive {
                DispatchQueue.main.async { [weak self] in
                    self?.isPTTActive = false
                }
            }
            
        default:
            break
        }
        
        return Unmanaged.passRetained(event)
    }
    
    private func modifiersMatch(_ eventFlags: CGEventFlags, _ hotkeyModifiers: UInt32) -> Bool {
        let relevantMask: UInt32 = UInt32(CGEventFlags.maskCommand.rawValue) |
                                    UInt32(CGEventFlags.maskShift.rawValue) |
                                    UInt32(CGEventFlags.maskAlternate.rawValue) |
                                    UInt32(CGEventFlags.maskControl.rawValue)
        
        let eventMods = UInt32(eventFlags.rawValue) & relevantMask
        return eventMods == (hotkeyModifiers & relevantMask)
    }
    
    // MARK: - Validation
    
    func validateHotkey(_ hotkey: AudioSettings.Hotkey) -> Bool {
        // Ensure at least one modifier is pressed
        let hasModifier = hotkey.modifiers & (
            UInt32(CGEventFlags.maskCommand.rawValue) |
            UInt32(CGEventFlags.maskShift.rawValue) |
            UInt32(CGEventFlags.maskAlternate.rawValue) |
            UInt32(CGEventFlags.maskControl.rawValue)
        ) != 0
        
        return hasModifier
    }
}
