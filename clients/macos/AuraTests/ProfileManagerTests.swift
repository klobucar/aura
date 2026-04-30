import XCTest
@testable import Aura

@MainActor
final class ProfileManagerTests: XCTestCase {
    
    var profileManager: ProfileManager!
    let testStorageKey = "TestAuraUserProfiles"
    
    override func setUp() async throws {
        UserDefaults.standard.removeObject(forKey: testStorageKey)

        // The keychain cleanup helper iterates `profileManager.profiles`, so
        // we have to construct the manager first. With an empty UserDefaults
        // it'll start with zero profiles.
        profileManager = ProfileManager(storageKey: testStorageKey)
        cleanupTestKeychain()
    }
    
    override func tearDown() async throws {
        UserDefaults.standard.removeObject(forKey: testStorageKey)
        cleanupTestKeychain()
        profileManager = nil
    }
    
    private func cleanupTestKeychain() {
        // Clean up test profiles from keychain
        for profile in profileManager.profiles {
            UserIdentity.deleteFromKeychain(id: profile.id)
        }
    }
    
    // MARK: - CRUD Tests
    
    func testCreateProfile() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Test User", identity: identity)
        
        XCTAssertEqual(profileManager.profiles.count, 1)
        XCTAssertEqual(profileManager.profiles.first?.displayName, "Test User")
        XCTAssertNotNil(profileManager.profiles.first?.publicKeyHex)
    }
    
    func testUpdateProfile() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Original Name"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Original Name", identity: identity)
        
        var profile = profileManager.profiles.first!
        profile.displayName = "Updated Name"
        profileManager.updateProfile(profile)
        
        XCTAssertEqual(profileManager.profiles.count, 1)
        XCTAssertEqual(profileManager.profiles.first?.displayName, "Updated Name")
    }
    
    func testDeleteProfile() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Test User", identity: identity)
        let profileId = profileManager.profiles.first!.id
        
        XCTAssertEqual(profileManager.profiles.count, 1)
        
        profileManager.deleteProfile(id: profileId)
        
        XCTAssertEqual(profileManager.profiles.count, 0)
    }
    
    // MARK: - Server Linking Tests
    
    func testLinkProfileToServer() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Test User", identity: identity)
        let profileId = profileManager.profiles.first!.id
        
        let serverId = UUID()
        profileManager.linkToServer(profileId: profileId, serverId: serverId)
        
        XCTAssertTrue(profileManager.profiles.first!.linkedServerIds.contains(serverId))
    }
    
    func testLinkProfileToMultipleServers() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Test User", identity: identity)
        let profileId = profileManager.profiles.first!.id
        
        let server1 = UUID()
        let server2 = UUID()
        
        profileManager.linkToServer(profileId: profileId, serverId: server1)
        profileManager.linkToServer(profileId: profileId, serverId: server2)
        
        XCTAssertEqual(profileManager.profiles.first!.linkedServerIds.count, 2)
        XCTAssertTrue(profileManager.profiles.first!.linkedServerIds.contains(server1))
        XCTAssertTrue(profileManager.profiles.first!.linkedServerIds.contains(server2))
    }
    
    func testNoDuplicateServerLinks() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Test User", identity: identity)
        let profileId = profileManager.profiles.first!.id
        
        let serverId = UUID()
        
        profileManager.linkToServer(profileId: profileId, serverId: serverId)
        profileManager.linkToServer(profileId: profileId, serverId: serverId)
        
        XCTAssertEqual(profileManager.profiles.first!.linkedServerIds.count, 1)
    }
    
    // MARK: - Recent Profiles Tests
    
    func testRecentProfiles() {
        let identity1 = UserIdentity()
        identity1.id = UUID()
        identity1.displayName = "User 1"
        identity1.generateKeypair()
        
        let identity2 = UserIdentity()
        identity2.id = UUID()
        identity2.displayName = "User 2"
        identity2.generateKeypair()
        
        let identity3 = UserIdentity()
        identity3.id = UUID()
        identity3.displayName = "User 3"
        identity3.generateKeypair()
        
        profileManager.createProfile(displayName: "User 1", identity: identity1)
        profileManager.createProfile(displayName: "User 2", identity: identity2)
        profileManager.createProfile(displayName: "User 3", identity: identity3)
        
        // Mark profiles as used in specific order
        profileManager.markAsUsed(id: identity1.id!)
        Thread.sleep(forTimeInterval: 0.01)
        profileManager.markAsUsed(id: identity3.id!)
        Thread.sleep(forTimeInterval: 0.01)
        profileManager.markAsUsed(id: identity2.id!)
        
        let recent = profileManager.recentProfiles
        
        XCTAssertEqual(recent.count, 3)
        XCTAssertEqual(recent[0].displayName, "User 2") // Most recent
        XCTAssertEqual(recent[1].displayName, "User 3")
        XCTAssertEqual(recent[2].displayName, "User 1") // Least recent
    }
    
    func testRecentProfilesLimit() {
        // Add 10 profiles
        for i in 1...10 {
            let identity = UserIdentity()
            identity.id = UUID()
            identity.displayName = "User \(i)"
            identity.generateKeypair()
            
            profileManager.createProfile(displayName: "User \(i)", identity: identity)
            profileManager.markAsUsed(id: identity.id!)
            Thread.sleep(forTimeInterval: 0.01)
        }
        
        let recent = profileManager.recentProfiles
        XCTAssertEqual(recent.count, 5) // Should be limited to 5
    }
    
    // MARK: - Persistence Tests
    
    func testPersistence() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Persistent User"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Persistent User", identity: identity)
        
        // Create new manager instance (simulates app restart)
        let newManager = ProfileManager(storageKey: testStorageKey)
        
        XCTAssertEqual(newManager.profiles.count, 1)
        XCTAssertEqual(newManager.profiles.first?.displayName, "Persistent User")
    }
    
    // MARK: - Biometric Flag Tests
    
    func testBiometricFlagPersistence() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Biometric User"
        identity.generateKeypair()
        
        var profile = UserProfileModel(
            id: identity.id!,
            displayName: "Biometric User",
            publicKeyHex: identity.publicKeyHex,
            requiresBiometric: true
        )
        
        profileManager.profiles.append(profile)
        
        // Create new manager instance
        let newManager = ProfileManager()
        
        XCTAssertTrue(newManager.profiles.first?.requiresBiometric ?? false)
    }
}
