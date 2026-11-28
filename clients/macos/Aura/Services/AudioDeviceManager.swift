import Foundation
import Combine
import CoreAudio
import AVFoundation
import os

class AudioDeviceManager: ObservableObject {
    static let shared = AudioDeviceManager()

    private static let outputUIDKey = "AuraSelectedOutputDeviceUID"
    private static let inputUIDKey = "AuraSelectedInputDeviceUID"

    @Published var availableOutputDevices: [AudioDevice] = []
    @Published var availableInputDevices: [AudioDevice] = []
    /// `nil` represents System Default (no explicit CoreAudio device binding).
    @Published var selectedOutputDeviceID: AudioDeviceID?
    /// `nil` represents System Default (no explicit CoreAudio device binding).
    @Published var selectedInputDeviceID: AudioDeviceID?

    private var lastSystemDefaultOutputID: AudioDeviceID?
    private var lastSystemDefaultInputID: AudioDeviceID?

    struct AudioDevice: Identifiable, Hashable {
        let id: AudioDeviceID
        let name: String
        let uid: String
    }

    /// One instance for the app's lifetime (`shared`), so the CoreAudio
    /// listeners installed here are never removed.
    private init() {
        loadDevices()
        installCoreAudioListeners()
    }

    private func installCoreAudioListeners() {
        let systemObject = AudioObjectID(kAudioObjectSystemObject)
        let queue = DispatchQueue.main

        var devicesAddr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDevices,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let devicesBlock: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.handleHardwareChange()
        }
        _ = AudioObjectAddPropertyListenerBlock(systemObject, &devicesAddr, queue, devicesBlock)

        var defaultOutAddr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let defaultOutBlock: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.handleHardwareChange()
        }
        _ = AudioObjectAddPropertyListenerBlock(systemObject, &defaultOutAddr, queue, defaultOutBlock)

        var defaultInAddr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let defaultInBlock: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.handleHardwareChange()
        }
        _ = AudioObjectAddPropertyListenerBlock(systemObject, &defaultInAddr, queue, defaultInBlock)
    }

    private func handleHardwareChange() {
        let prevOutput = selectedOutputDeviceID
        let prevInput = selectedInputDeviceID
        let prevSysOut = lastSystemDefaultOutputID
        let prevSysIn = lastSystemDefaultInputID
        loadDevices()

        let outputChanged = selectedOutputDeviceID != prevOutput
            || (selectedOutputDeviceID == nil && lastSystemDefaultOutputID != prevSysOut)
        if outputChanged {
            print("[AudioDeviceManager] Hot-swap output device -> \(selectedOutputDeviceID.map(String.init) ?? "System Default")")
            NotificationCenter.default.post(name: .audioOutputDeviceChanged, object: selectedOutputDeviceID)
        }

        let inputChanged = selectedInputDeviceID != prevInput
            || (selectedInputDeviceID == nil && lastSystemDefaultInputID != prevSysIn)
        if inputChanged {
            print("[AudioDeviceManager] Hot-swap input device -> \(selectedInputDeviceID.map(String.init) ?? "System Default")")
            NotificationCenter.default.post(name: .audioInputDeviceChanged, object: selectedInputDeviceID)
        }
    }
    
    func loadDevices() {
        var propertySize = UInt32(0)
        var propertyAddress = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDevices,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        
        // Get number of devices
        guard AudioObjectGetPropertyDataSize(
            AudioObjectID(kAudioObjectSystemObject),
            &propertyAddress,
            0,
            nil,
            &propertySize
        ) == noErr else {
            return
        }
        
        let deviceCount = Int(propertySize) / MemoryLayout<AudioDeviceID>.size
        var devices = [AudioDeviceID](repeating: 0, count: deviceCount)
        
        // Get device IDs
        guard AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject),
            &propertyAddress,
            0,
            nil,
            &propertySize,
            &devices
        ) == noErr else {
            return
        }
        
        // Filter for output devices
        availableOutputDevices = devices.compactMap { deviceID in
            guard isOutputDevice(deviceID),
                  let name = getDeviceName(deviceID),
                  let uid = getDeviceUID(deviceID) else {
                return nil
            }
            return AudioDevice(id: deviceID, name: name, uid: uid)
        }
        
        // Filter for input devices
        availableInputDevices = devices.compactMap { deviceID in
            guard isInputDevice(deviceID),
                  let name = getDeviceName(deviceID),
                  let uid = getDeviceUID(deviceID) else {
                return nil
            }
            return AudioDevice(id: deviceID, name: name, uid: uid)
        }
        
        // Track current system default output/input IDs so we only notify on real default changes.
        var sysOutID = AudioDeviceID()
        var sysSize = UInt32(MemoryLayout<AudioDeviceID>.size)
        propertyAddress.mSelector = kAudioHardwarePropertyDefaultOutputDevice
        lastSystemDefaultOutputID = AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &propertyAddress, 0, nil, &sysSize, &sysOutID
        ) == noErr ? sysOutID : nil

        var sysInID = AudioDeviceID()
        propertyAddress.mSelector = kAudioHardwarePropertyDefaultInputDevice
        lastSystemDefaultInputID = AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &propertyAddress, 0, nil, &sysSize, &sysInID
        ) == noErr ? sysInID : nil

        // Resolve output selection: nil means System Default; otherwise match persisted UID if present.
        if let savedOutUID = UserDefaults.standard.string(forKey: Self.outputUIDKey),
           let matched = availableOutputDevices.first(where: { $0.uid == savedOutUID }) {
            selectedOutputDeviceID = matched.id
        } else {
            selectedOutputDeviceID = nil
        }

        // Resolve input selection: nil means System Default; otherwise match persisted UID if present.
        if let savedInUID = UserDefaults.standard.string(forKey: Self.inputUIDKey),
           let matched = availableInputDevices.first(where: { $0.uid == savedInUID }) {
            selectedInputDeviceID = matched.id
        } else {
            selectedInputDeviceID = nil
        }
    }
    
    private func isOutputDevice(_ deviceID: AudioDeviceID) -> Bool {
        var propertyAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreams,
            mScope: kAudioDevicePropertyScopeOutput,
            mElement: kAudioObjectPropertyElementMain
        )
        
        var propertySize = UInt32(0)
        guard AudioObjectGetPropertyDataSize(
            deviceID,
            &propertyAddress,
            0,
            nil,
            &propertySize
        ) == noErr else {
            return false
        }
        
        return propertySize > 0
    }
    
    private func isInputDevice(_ deviceID: AudioDeviceID) -> Bool {
        var propertyAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreams,
            mScope: kAudioDevicePropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        
        var propertySize = UInt32(0)
        guard AudioObjectGetPropertyDataSize(
            deviceID,
            &propertyAddress,
            0,
            nil,
            &propertySize
        ) == noErr else {
            return false
        }
        
        return propertySize > 0
    }
    
    private func getDeviceName(_ deviceID: AudioDeviceID) -> String? {
        var propertyAddress = AudioObjectPropertyAddress(
            mSelector: kAudioObjectPropertyName,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        
        var name: CFString = "" as CFString
        var propertySize = UInt32(MemoryLayout<CFString>.size)
        
        guard AudioObjectGetPropertyData(
            deviceID,
            &propertyAddress,
            0,
            nil,
            &propertySize,
            &name
        ) == noErr else {
            return nil
        }
        
        return name as String
    }
    
    private func getDeviceUID(_ deviceID: AudioDeviceID) -> String? {
        var propertyAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceUID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        
        var uid: CFString = "" as CFString
        var propertySize = UInt32(MemoryLayout<CFString>.size)
        
        guard AudioObjectGetPropertyData(
            deviceID,
            &propertyAddress,
            0,
            nil,
            &propertySize,
            &uid
        ) == noErr else {
            return nil
        }
        
        return uid as String
    }
    
    /// Selects the per-app output device without mutating system-wide defaults (`nil` = System Default).
    func setOutputDevice(_ deviceID: AudioDeviceID?) {
        selectedOutputDeviceID = deviceID
        if let deviceID,
           let uid = availableOutputDevices.first(where: { $0.id == deviceID })?.uid ?? getDeviceUID(deviceID) {
            UserDefaults.standard.set(uid, forKey: Self.outputUIDKey)
        } else {
            UserDefaults.standard.removeObject(forKey: Self.outputUIDKey)
        }
        NotificationCenter.default.post(name: .audioOutputDeviceChanged, object: deviceID)
        print("[AudioDeviceManager] Selected per-app output device: \(deviceID.map(String.init) ?? "System Default")")
    }

    /// Selects the per-app input device without mutating system-wide defaults (`nil` = System Default).
    func setInputDevice(_ deviceID: AudioDeviceID?) {
        selectedInputDeviceID = deviceID
        if let deviceID,
           let uid = availableInputDevices.first(where: { $0.id == deviceID })?.uid ?? getDeviceUID(deviceID) {
            UserDefaults.standard.set(uid, forKey: Self.inputUIDKey)
        } else {
            UserDefaults.standard.removeObject(forKey: Self.inputUIDKey)
        }
        NotificationCenter.default.post(name: .audioInputDeviceChanged, object: deviceID)
        print("[AudioDeviceManager] Selected per-app input device: \(deviceID.map(String.init) ?? "System Default")")
    }
}
