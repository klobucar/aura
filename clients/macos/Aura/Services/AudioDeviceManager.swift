import Foundation
import Combine
import CoreAudio
import AVFoundation

class AudioDeviceManager: ObservableObject {
    @Published var availableOutputDevices: [AudioDevice] = []
    @Published var availableInputDevices: [AudioDevice] = []
    @Published var selectedOutputDeviceID: AudioDeviceID?
    @Published var selectedInputDeviceID: AudioDeviceID?
    
    struct AudioDevice: Identifiable, Hashable {
        let id: AudioDeviceID
        let name: String
        let uid: String
    }
    
    init() {
        loadDevices()
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
        
        // Get default output device
        propertyAddress.mSelector = kAudioHardwarePropertyDefaultOutputDevice
        var defaultOutputID = AudioDeviceID()
        var size = UInt32(MemoryLayout<AudioDeviceID>.size)
        
        if AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject),
            &propertyAddress,
            0,
            nil,
            &size,
            &defaultOutputID
        ) == noErr {
            selectedOutputDeviceID = defaultOutputID
        }
        
        // Get default input device
        propertyAddress.mSelector = kAudioHardwarePropertyDefaultInputDevice
        var defaultInputID = AudioDeviceID()
        
        if AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject),
            &propertyAddress,
            0,
            nil,
            &size,
            &defaultInputID
        ) == noErr {
            selectedInputDeviceID = defaultInputID
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
    
    func setOutputDevice(_ deviceID: AudioDeviceID) {
        var propertyAddress = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        
        var device = deviceID
        let size = UInt32(MemoryLayout<AudioDeviceID>.size)
        
        if AudioObjectSetPropertyData(
            AudioObjectID(kAudioObjectSystemObject),
            &propertyAddress,
            0,
            nil,
            size,
            &device
        ) == noErr {
            selectedOutputDeviceID = deviceID
            print("[AudioDeviceManager] Set output device to \(deviceID)")
        }
    }
    
    func setInputDevice(_ deviceID: AudioDeviceID) {
        var propertyAddress = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        
        var device = deviceID
        let size = UInt32(MemoryLayout<AudioDeviceID>.size)
        
        if AudioObjectSetPropertyData(
            AudioObjectID(kAudioObjectSystemObject),
            &propertyAddress,
            0,
            nil,
            size,
            &device
        ) == noErr {
            selectedInputDeviceID = deviceID
            print("[AudioDeviceManager] Set input device to \(deviceID)")
        }
    }
}
