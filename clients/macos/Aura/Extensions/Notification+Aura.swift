//
//  Notification+Aura.swift
//  Aura
//
//  Notification extensions for Aura app events
//

import Foundation

extension Notification.Name {
    /// Posted when the set of active speakers changes
    /// Object: Set<UInt32> of session IDs currently speaking
    static let activeSpeakersChanged = Notification.Name("activeSpeakersChanged")
    static let audioSettingsChanged = Notification.Name("audioSettingsChanged")
    static let connectionRestored = Notification.Name("connectionRestored")
}
