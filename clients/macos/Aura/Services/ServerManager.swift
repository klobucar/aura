import Foundation
import Combine

/// Manages saved server profiles with persistence
@MainActor
public class ServerManager: ObservableObject {
    
    @Published public var servers: [ServerProfile] = []

    /// UserDefaults key. Production code uses the default; tests pass a custom
    /// key so they can isolate state per-test without touching real prefs.
    private let storageKey: String

    public init(storageKey: String = "AuraServerProfiles") {
        self.storageKey = storageKey
        loadServers()
    }
    
    // MARK: - CRUD Operations
    
    public func addServer(_ server: ServerProfile) {
        servers.append(server)
        saveServers()
    }
    
    public func updateServer(_ server: ServerProfile) {
        if let index = servers.firstIndex(where: { $0.id == server.id }) {
            servers[index] = server
            saveServers()
        }
    }
    
    public func deleteServer(id: UUID) {
        servers.removeAll { $0.id == id }
        saveServers()
    }
    
    public func markAsUsed(id: UUID) {
        if let index = servers.firstIndex(where: { $0.id == id }) {
            servers[index].lastUsed = Date()
            saveServers()
        }
    }
    
    // MARK: - Computed Properties
    
    public var recentServers: [ServerProfile] {
        servers
            .filter { $0.lastUsed != nil }
            .sorted { ($0.lastUsed ?? .distantPast) > ($1.lastUsed ?? .distantPast) }
            .prefix(5)
            .map { $0 }
    }
    
    public var favoriteServers: [ServerProfile] {
        servers.filter { $0.isFavorite }
    }
    
    // MARK: - Persistence
    
    private func loadServers() {
        guard let data = UserDefaults.standard.data(forKey: storageKey) else {
            print("[ServerManager] No saved servers found")
            return
        }
        
        do {
            servers = try JSONDecoder().decode([ServerProfile].self, from: data)
            print("[ServerManager] Loaded \\(servers.count) servers")
        } catch {
            print("[ServerManager] Failed to load servers: \\(error)")
        }
    }
    
    private func saveServers() {
        do {
            let data = try JSONEncoder().encode(servers)
            UserDefaults.standard.set(data, forKey: storageKey)
            print("[ServerManager] Saved \\(servers.count) servers")
        } catch {
            print("[ServerManager] Failed to save servers: \\(error)")
        }
    }
}
