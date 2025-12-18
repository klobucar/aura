import Foundation
import AVFoundation
import Combine
import SwiftUI

class TtsManager: ObservableObject {
    static let shared = TtsManager()
    
    private let synthesizer = AVSpeechSynthesizer()
    private var formatter: TtsFormatter?
    
    @Published var settings = TtsSettings(
        enabled: false,
        volume: 0.8,
        rate: 0.5,
        speakChat: true,
        speakJoinLeave: true
    )
    
    private init() {
        self.formatter = TtsFormatter()
    }
    
    func setFormatter(_ formatter: TtsFormatter) {
        self.formatter = formatter
    }
    
    func speak(_ text: String) {
        guard settings.enabled else { return }
        
        // Use Rust-side sanitization first
        let sanitized = formatter?.sanitize(text: text) ?? text
        
        let utterance = AVSpeechUtterance(string: sanitized)
        utterance.voice = AVSpeechSynthesisVoice(language: "en-US")
        utterance.rate = settings.rate
        utterance.volume = settings.volume
        
        synthesizer.speak(utterance)
    }
    
    func speakMessage(sender: String, content: String) {
        guard settings.enabled && settings.speakChat else { return }
        let textToSpeak = "\(sender) says: \(content)"
        speak(textToSpeak)
    }
    
    func speakJoin(name: String) {
        guard settings.enabled && settings.speakJoinLeave else { return }
        if let formatted = formatter?.formatJoin(name: name) {
            speak(formatted)
        } else {
            speak("\(name) joined the channel")
        }
    }
    
    func speakLeave(name: String) {
        guard settings.enabled && settings.speakJoinLeave else { return }
        if let formatted = formatter?.formatLeave(name: name) {
            speak(formatted)
        } else {
            speak("\(name) left the channel")
        }
    }
    
    func stop() {
        synthesizer.stopSpeaking(at: .immediate)
    }
}
