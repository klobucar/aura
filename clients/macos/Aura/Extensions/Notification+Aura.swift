//
//  Notification+Aura.swift
//  Aura
//
//  Notification extensions for Aura app events
//

import Foundation

nonisolated extension Notification.Name {
    /// Posted when the set of active speakers changes
    /// Object: Set<UInt32> of session IDs currently speaking
    static let activeSpeakersChanged = Notification.Name("activeSpeakersChanged")
    static let audioSettingsChanged = Notification.Name("audioSettingsChanged")
    static let connectionRestored = Notification.Name("connectionRestored")
    /// Posted when another user's profile (bio / avatar / display name)
    /// was updated. Object: UInt32 session id of the affected profile.
    static let profileUpdated = Notification.Name("profileUpdated")
    /// Posted when the user selects or hot-swaps the per-app input audio device.
    /// Object: AudioDeviceID (UInt32).
    static let audioInputDeviceChanged = Notification.Name("audioInputDeviceChanged")
    /// Posted when the user selects or hot-swaps the per-app output audio device.
    /// Object: AudioDeviceID (UInt32).
    static let audioOutputDeviceChanged = Notification.Name("audioOutputDeviceChanged")
    /// Posted when AVAudioEngine's output hardware configuration changes.
    static let audioOutputNeedsRestart = Notification.Name("audioOutputNeedsRestart")
}
