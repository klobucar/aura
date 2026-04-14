import Foundation
import Combine
import Carbon
import AppKit

class HotkeyManager: ObservableObject {
    static let shared = HotkeyManager()

    /// Only these bits of a CGEventFlags / NSEvent.ModifierFlags rawValue
    /// are considered when matching or persisting a PTT hotkey. Anything
    /// else (function-key bit, device type, caps lock state, numeric pad)
    /// gets masked out so stored hotkeys are comparable byte-for-byte.
    static let relevantModifierMask: UInt32 =
        UInt32(CGEventFlags.maskCommand.rawValue) |
        UInt32(CGEventFlags.maskShift.rawValue) |
        UInt32(CGEventFlags.maskAlternate.rawValue) |
        UInt32(CGEventFlags.maskControl.rawValue)

    @Published private(set) var isPTTActive = false
    @Published private(set) var hasAccessibilityPermission = false

    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var currentHotkey: AudioSettings.Hotkey?
    private var pendingHotkeyForGrant: AudioSettings.Hotkey?
    private var permissionPollTimer: Timer?

    private init() {
        checkAccessibilityPermission()
    }

    // MARK: - Accessibility Permission

    func checkAccessibilityPermission() {
        let granted = AXIsProcessTrusted()
        hasAccessibilityPermission = granted
        if granted, let pending = pendingHotkeyForGrant {
            pendingHotkeyForGrant = nil
            stopPermissionPoll()
            registerHotkey(pending)
        }
    }

    func requestAccessibilityPermission() {
        let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
        AXIsProcessTrustedWithOptions(options)
        startPermissionPoll()
    }

    /// Poll AXIsProcessTrusted once per second while we're waiting for the
    /// user to flip the switch in System Settings → Privacy & Security →
    /// Accessibility. Stops itself as soon as the answer turns true or
    /// when the PTT mode is disabled.
    private func startPermissionPoll() {
        stopPermissionPoll()
        permissionPollTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self] _ in
            self?.checkAccessibilityPermission()
        }
    }

    private func stopPermissionPoll() {
        permissionPollTimer?.invalidate()
        permissionPollTimer = nil
    }
    
    // MARK: - Hotkey Registration
    
    func registerHotkey(_ hotkey: AudioSettings.Hotkey) {
        // Unregister existing hotkey
        unregisterHotkey()

        // Always re-check before registering — a stale `hasAccessibilityPermission`
        // cached from app launch will happily return false even after the user
        // has just granted it in System Settings.
        checkAccessibilityPermission()

        guard hasAccessibilityPermission else {
            print("[HotkeyManager] No accessibility permission — queuing registration and prompting")
            pendingHotkeyForGrant = hotkey
            requestAccessibilityPermission()
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
        pendingHotkeyForGrant = nil
        stopPermissionPoll()
        isPTTActive = false
    }
    
    // MARK: - Event Handling
    
    private func handleEvent(proxy: CGEventTapProxy, type: CGEventType, event: CGEvent) -> Unmanaged<CGEvent>? {
        guard let hotkey = currentHotkey else {
            return Unmanaged.passRetained(event)
        }

        let storedMods = hotkey.modifiers & Self.relevantModifierMask
        let isModifierOnly = storedMods != 0 && hotkey.keyCode == AudioSettings.Hotkey.modifierOnlyKeyCode

        switch type {
        case .keyDown, .keyUp:
            if isModifierOnly { break } // modifier-only hotkey cares only about flagsChanged

            let keyCode = UInt16(event.getIntegerValueField(.keyboardEventKeycode))
            let flags = event.flags

            if keyCode == hotkey.keyCode && modifiersMatch(flags, storedMods) {
                DispatchQueue.main.async { [weak self] in
                    self?.isPTTActive = (type == .keyDown)
                }
                return nil // Consume
            }

        case .flagsChanged:
            let flags = event.flags
            let eventMods = UInt32(flags.rawValue) & Self.relevantModifierMask

            if isModifierOnly {
                // Activate when exactly the stored modifier set is pressed,
                // deactivate when it is no longer.
                let nowActive = eventMods == storedMods
                if nowActive != isPTTActive {
                    DispatchQueue.main.async { [weak self] in
                        self?.isPTTActive = nowActive
                    }
                }
            } else if storedMods != 0 && isPTTActive && eventMods != storedMods {
                // Modifier+key hotkey: if the user releases the modifier
                // before we see the keyUp, still deactivate cleanly.
                // For modifier-less hotkeys (storedMods == 0) this path is
                // a no-op so merely tapping Cmd does not kill PTT.
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
        let eventMods = UInt32(eventFlags.rawValue) & Self.relevantModifierMask
        return eventMods == (hotkeyModifiers & Self.relevantModifierMask)
    }

    // MARK: - Validation

    func validateHotkey(_ hotkey: AudioSettings.Hotkey) -> Bool {
        // Any bound key is allowed — plain keys (F13, backtick, …) are
        // common for PTT on desktops and used to be rejected outright.
        // Modifier-only bindings (e.g. right-Option) are represented by
        // a sentinel keyCode and at least one modifier bit.
        let mods = hotkey.modifiers & Self.relevantModifierMask
        if hotkey.keyCode == AudioSettings.Hotkey.modifierOnlyKeyCode {
            return mods != 0
        }
        return true
    }
}
