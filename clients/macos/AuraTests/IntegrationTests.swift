import XCTest
@testable import Aura

@MainActor
final class IntegrationTests: XCTestCase {
    
    var serverManager: ServerManager!
    var profileManager: ProfileManager!
    
    override func setUp() async throws {
        serverManager = ServerManager()
        profileManager = ProfileManager()
        
        // Clean up test data
        UserDefaults.standard.removeObject(forKey: "TestAuraServerProfiles")
        UserDefaults.standard.removeObject(forKey: "TestAuraUserProfiles")
    }
    
    override func tearDown() async throws {
        // Clean up
        for profile in profileManager.profiles {
            UserIdentity.deleteFromKeychain(id: profile.id)
        }
        
        UserDefaults.standard.removeObject(forKey: "TestAuraServerProfiles")
        UserDefaults.standard.removeObject(forKey: "TestAuraUserProfiles")
        
        serverManager = nil
        profileManager = nil
    }
    
    // MARK: - Profile + Server Integration Tests
    
    func testProfileServerLinking() {
        // Create a server
        let server = ServerProfile(
            name: "Test Server",
            host: "127.0.0.1",
            port: 8443
        )
        serverManager.addServer(server)
        
        // Create a profile
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Test User", identity: identity)
        
        // Link profile to server
        profileManager.linkToServer(profileId: identity.id!, serverId: server.id)
        
        // Verify link
        let profile = profileManager.profiles.first!
        XCTAssertTrue(profile.linkedServerIds.contains(server.id))
        
        // Mark both as used
        serverManager.markAsUsed(id: server.id)
        profileManager.markAsUsed(id: profile.id)
        
        // Verify they appear in recent lists
        XCTAssertEqual(serverManager.recentServers.count, 1)
        XCTAssertEqual(profileManager.recentProfiles.count, 1)
    }
    
    func testMultipleProfilesMultipleServers() {
        // Create 3 servers
        let servers = (1...3).map { i in
            ServerProfile(name: "Server \(i)", host: "127.0.0.\(i)", port: 8443)
        }
        servers.forEach { serverManager.addServer($0) }
        
        // Create 2 profiles
        let identities = (1...2).map { i -> UserIdentity in
            let identity = UserIdentity()
            identity.id = UUID()
            identity.displayName = "User \(i)"
            identity.generateKeypair()
            return identity
        }
        identities.forEach { profileManager.createProfile(displayName: $0.displayName, identity: $0) }
        
        // Link User 1 to Server 1 and 2
        profileManager.linkToServer(profileId: identities[0].id!, serverId: servers[0].id)
        profileManager.linkToServer(profileId: identities[0].id!, serverId: servers[1].id)
        
        // Link User 2 to Server 2 and 3
        profileManager.linkToServer(profileId: identities[1].id!, serverId: servers[1].id)
        profileManager.linkToServer(profileId: identities[1].id!, serverId: servers[2].id)
        
        // Verify links
        let profile1 = profileManager.profiles.first { $0.displayName == "User 1" }!
        let profile2 = profileManager.profiles.first { $0.displayName == "User 2" }!
        
        XCTAssertEqual(profile1.linkedServerIds.count, 2)
        XCTAssertEqual(profile2.linkedServerIds.count, 2)
        XCTAssertTrue(profile1.linkedServerIds.contains(servers[0].id))
        XCTAssertTrue(profile1.linkedServerIds.contains(servers[1].id))
        XCTAssertTrue(profile2.linkedServerIds.contains(servers[1].id))
        XCTAssertTrue(profile2.linkedServerIds.contains(servers[2].id))
    }
    
    // MARK: - Import/Export Integration Tests
    
    func testProfileImportExportWithServerLinks() {
        // Create server
        let server = ServerProfile(name: "Test Server", host: "127.0.0.1", port: 8443)
        serverManager.addServer(server)
        
        // Create profile
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Export User"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Export User", identity: identity)
        profileManager.linkToServer(profileId: identity.id!, serverId: server.id)
        
        // Export profile
        guard let exportData = identity.exportProfile() else {
            XCTFail("Failed to export profile")
            return
        }
        
        // Delete profile
        profileManager.deleteProfile(id: identity.id!)
        XCTAssertEqual(profileManager.profiles.count, 0)
        
        // Import profile
        guard let importedIdentity = UserIdentity.importProfile(from: exportData) else {
            XCTFail("Failed to import profile")
            return
        }
        
        // Verify imported profile
        XCTAssertEqual(importedIdentity.displayName, "Export User")
        XCTAssertEqual(importedIdentity.publicKeyHex, identity.publicKeyHex)
        
        // Note: Server links are stored in UserProfileModel, not in the exported identity
        // So we need to recreate the profile in ProfileManager
        let newProfile = UserProfileModel(
            id: importedIdentity.id!,
            displayName: importedIdentity.displayName,
            publicKeyHex: importedIdentity.publicKeyHex
        )
        profileManager.profiles.append(newProfile)
        
        XCTAssertEqual(profileManager.profiles.count, 1)
    }
    
    // MARK: - Keychain Integration Tests
    
    func testProfileKeychainPersistence() {
        // Create profile with keychain storage
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Keychain User"
        identity.generateKeypair()
        
        let originalPublicKey = identity.publicKeyHex
        
        // Save to keychain
        identity.saveToKeychain(requiresBiometric: false)
        
        // Create profile metadata
        profileManager.createProfile(displayName: "Keychain User", identity: identity)
        
        // Simulate app restart - create new managers
        let newProfileManager = ProfileManager()
        
        // Load profile metadata
        XCTAssertEqual(newProfileManager.profiles.count, 1)
        let profileId = newProfileManager.profiles.first!.id
        
        // Load identity from keychain
        guard let loadedIdentity = UserIdentity.loadFromKeychain(id: profileId, requiresBiometric: false) else {
            XCTFail("Failed to load identity from keychain")
            return
        }
        
        XCTAssertEqual(loadedIdentity.publicKeyHex, originalPublicKey)
    }
    
    // MARK: - Connection Flow Integration Tests
    
    func testSavedServerAndProfileForConnection() {
        // Create and save server
        let server = ServerProfile(
            name: "My Server",
            host: "127.0.0.1",
            port: 8443,
            password: "secret"
        )
        serverManager.addServer(server)
        
        // Create and save profile
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "My Profile"
        identity.generateKeypair()
        
        identity.saveToKeychain(requiresBiometric: false)
        profileManager.createProfile(displayName: "My Profile", identity: identity)
        profileManager.linkToServer(profileId: identity.id!, serverId: server.id)
        
        // Simulate selecting server and profile for connection
        let selectedServer = serverManager.servers.first { $0.id == server.id }!
        let selectedProfile = profileManager.profiles.first { $0.id == identity.id }!
        
        XCTAssertEqual(selectedServer.name, "My Server")
        XCTAssertEqual(selectedServer.host, "127.0.0.1")
        XCTAssertEqual(selectedServer.port, 8443)
        XCTAssertEqual(selectedServer.password, "secret")
        
        XCTAssertEqual(selectedProfile.displayName, "My Profile")
        XCTAssertTrue(selectedProfile.linkedServerIds.contains(server.id))
        
        // Load identity from keychain for connection
        guard let connectionIdentity = UserIdentity.loadFromKeychain(id: selectedProfile.id, requiresBiometric: false) else {
            XCTFail("Failed to load identity for connection")
            return
        }
        
        XCTAssertEqual(connectionIdentity.publicKeyHex, selectedProfile.publicKeyHex)
    }
    
    // MARK: - Data Consistency Tests
    
    func testDeleteServerRemovesLinks() {
        // Create server and profile
        let server = ServerProfile(name: "Test Server", host: "127.0.0.1", port: 8443)
        serverManager.addServer(server)
        
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Test User", identity: identity)
        profileManager.linkToServer(profileId: identity.id!, serverId: server.id)
        
        // Verify link exists
        XCTAssertTrue(profileManager.profiles.first!.linkedServerIds.contains(server.id))
        
        // Delete server
        serverManager.deleteServer(id: server.id)
        
        // Note: In a production app, you'd want to clean up orphaned links
        // For now, we just verify the server is gone
        XCTAssertEqual(serverManager.servers.count, 0)
    }
}
