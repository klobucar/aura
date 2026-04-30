import Foundation
import Combine

/// Manages user profiles with keychain coordination
@MainActor
public class ProfileManager: ObservableObject {
    
    @Published public var profiles: [UserProfileModel] = []

    /// UserDefaults key. Production code uses the default; tests pass a custom
    /// key so they can isolate state per-test without touching real prefs.
    private let storageKey: String

    public init(storageKey: String = "AuraUserProfiles") {
        self.storageKey = storageKey
        loadProfiles()
    }
    
    // MARK: - CRUD Operations
    
    public func createProfile(displayName: String, identity: UserIdentity) {
        let profile = UserProfileModel(
            id: identity.id ?? UUID(),
            displayName: displayName,
            publicKeyHex: identity.publicKeyHex,
            createdAt: Date()
        )
        
        profiles.append(profile)
        
        // Save identity to keychain
        identity.saveToKeychain()
        
        saveProfiles()
    }
    
    public func updateProfile(_ profile: UserProfileModel) {
        if let index = profiles.firstIndex(where: { $0.id == profile.id }) {
            profiles[index] = profile
            saveProfiles()
        }
    }
    
    public func deleteProfile(id: UUID) {
        profiles.removeAll { $0.id == id }
        
        // Delete from keychain
        UserIdentity.deleteFromKeychain(id: id)
        
        saveProfiles()
    }
    
    public func markAsUsed(id: UUID) {
        if let index = profiles.firstIndex(where: { $0.id == id }) {
            profiles[index].lastUsed = Date()
            saveProfiles()
        }
    }
    
    public func linkToServer(profileId: UUID, serverId: UUID) {
        if let index = profiles.firstIndex(where: { $0.id == profileId }) {
            if !profiles[index].linkedServerIds.contains(serverId) {
                profiles[index].linkedServerIds.append(serverId)
                saveProfiles()
            }
        }
    }
    
    // MARK: - Computed Properties
    
    public var recentProfiles: [UserProfileModel] {
        profiles
            .filter { $0.lastUsed != nil }
            .sorted { ($0.lastUsed ?? .distantPast) > ($1.lastUsed ?? .distantPast) }
            .prefix(5)
            .map { $0 }
    }
    
    // MARK: - Persistence
    
    private func loadProfiles() {
        guard let data = UserDefaults.standard.data(forKey: storageKey) else {
            print("[ProfileManager] No saved profiles found")
            return
        }
        
        do {
            profiles = try JSONDecoder().decode([UserProfileModel].self, from: data)
            print("[ProfileManager] Loaded \\(profiles.count) profiles")
        } catch {
            print("[ProfileManager] Failed to load profiles: \\(error)")
        }
    }
    
    private func saveProfiles() {
        do {
            let data = try JSONEncoder().encode(profiles)
            UserDefaults.standard.set(data, forKey: storageKey)
            print("[ProfileManager] Saved \\(profiles.count) profiles")
        } catch {
            print("[ProfileManager] Failed to save profiles: \\(error)")
        }
    }
}
