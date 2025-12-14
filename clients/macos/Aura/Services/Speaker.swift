import AVFoundation
import SwiftUI
import Combine

class Speaker: NSObject, ObservableObject {
    static let shared = Speaker()
    
    private let synthesizer = AVSpeechSynthesizer()
    
    @Published var availableVoices: [AVSpeechSynthesisVoice] = []
    @Published var selectedVoiceId: String? {
        didSet {
            UserDefaults.standard.set(selectedVoiceId, forKey: "AuraSelectedVoice")
        }
    }
    
    override init() {
        super.init()
        self.availableVoices = AVSpeechSynthesisVoice.speechVoices()
        if let stored = UserDefaults.standard.string(forKey: "AuraSelectedVoice") {
            self.selectedVoiceId = stored
        } else {
            // Fallback to default voice for current language
            let currentLang = AVSpeechSynthesisVoice.currentLanguageCode()
            self.selectedVoiceId = AVSpeechSynthesisVoice(language: currentLang)?.identifier
        }
    }
    
    func speak(text: String, author: String) {
        // Logic: If message.author != me, queue the utterance.
        // Assuming "Me" is the local user identifier for now.
        guard author != "Me" else { return }
        
        let utterance = AVSpeechUtterance(string: text)
        
        if let voiceId = selectedVoiceId,
           let voice = AVSpeechSynthesisVoice(identifier: voiceId) {
            utterance.voice = voice
        }
        
        synthesizer.speak(utterance)
    }
}
