import XCTest
@testable import Aura

@MainActor
final class EdgeCaseTests: XCTestCase {
    
    var serverManager: ServerManager!
    var profileManager: ProfileManager!
    
    override func setUp() async throws {
        UserDefaults.standard.removeObject(forKey: "TestAuraServerProfiles")
        UserDefaults.standard.removeObject(forKey: "TestAuraUserProfiles")

        serverManager = ServerManager(storageKey: "TestAuraServerProfiles")
        profileManager = ProfileManager(storageKey: "TestAuraUserProfiles")
    }
    
    override func tearDown() async throws {
        for profile in profileManager.profiles {
            UserIdentity.deleteFromKeychain(id: profile.id)
        }
        
        UserDefaults.standard.removeObject(forKey: "TestAuraServerProfiles")
        UserDefaults.standard.removeObject(forKey: "TestAuraUserProfiles")
        
        serverManager = nil
        profileManager = nil
    }
    
    // MARK: - ServerManager Edge Cases
    
    func testUpdateNonExistentServer() {
        let server = ServerProfile(name: "Ghost Server", host: "127.0.0.1", port: 8443)
        
        // Try to update server that doesn't exist
        serverManager.updateServer(server)
        
        // Should not add it
        XCTAssertEqual(serverManager.servers.count, 0)
    }
    
    func testMarkNonExistentServerAsUsed() {
        let ghostId = UUID()
        
        // Should not crash
        serverManager.markAsUsed(id: ghostId)
        
        XCTAssertEqual(serverManager.servers.count, 0)
    }
    
    func testEmptyServerName() {
        let server = ServerProfile(name: "", host: "127.0.0.1", port: 8443)
        
        serverManager.addServer(server)
        
        XCTAssertEqual(serverManager.servers.count, 1)
        XCTAssertEqual(serverManager.servers.first?.name, "")
    }
    
    func testServerWithInvalidPort() {
        let server = ServerProfile(name: "Test", host: "127.0.0.1", port: 0)
        
        serverManager.addServer(server)
        
        XCTAssertEqual(serverManager.servers.first?.port, 0)
    }
    
    func testServerWithMaxPort() {
        let server = ServerProfile(name: "Test", host: "127.0.0.1", port: 65535)
        
        serverManager.addServer(server)
        
        XCTAssertEqual(serverManager.servers.first?.port, 65535)
    }
    
    func testRecentServersWithNoLastUsed() {
        let server = ServerProfile(name: "Never Used", host: "127.0.0.1", port: 8443)
        serverManager.addServer(server)
        
        let recent = serverManager.recentServers
        
        XCTAssertEqual(recent.count, 0) // Should not appear in recent
    }
    
    func testMultipleDeletesOfSameServer() {
        let server = ServerProfile(name: "Test", host: "127.0.0.1", port: 8443)
        serverManager.addServer(server)
        
        serverManager.deleteServer(id: server.id)
        XCTAssertEqual(serverManager.servers.count, 0)
        
        // Delete again (should not crash)
        serverManager.deleteServer(id: server.id)
        XCTAssertEqual(serverManager.servers.count, 0)
    }
    
    // MARK: - ProfileManager Edge Cases
    
    func testUpdateNonExistentProfile() {
        let profile = UserProfileModel(
            id: UUID(),
            displayName: "Ghost",
            publicKeyHex: "abc123"
        )
        
        profileManager.updateProfile(profile)
        
        XCTAssertEqual(profileManager.profiles.count, 0)
    }
    
    func testMarkNonExistentProfileAsUsed() {
        let ghostId = UUID()
        
        profileManager.markAsUsed(id: ghostId)
        
        XCTAssertEqual(profileManager.profiles.count, 0)
    }
    
    func testLinkNonExistentProfileToServer() {
        let ghostProfileId = UUID()
        let serverId = UUID()
        
        profileManager.linkToServer(profileId: ghostProfileId, serverId: serverId)
        
        XCTAssertEqual(profileManager.profiles.count, 0)
    }
    
    func testEmptyDisplayName() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = ""
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "", identity: identity)
        
        XCTAssertEqual(profileManager.profiles.count, 1)
        XCTAssertEqual(profileManager.profiles.first?.displayName, "")
    }
    
    func testProfileWithVeryLongDisplayName() {
        let longName = String(repeating: "A", count: 1000)
        
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = longName
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: longName, identity: identity)
        
        XCTAssertEqual(profileManager.profiles.first?.displayName, longName)
    }
    
    func testRecentProfilesWithNoLastUsed() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Never Used"
        identity.generateKeypair()
        
        profileManager.createProfile(displayName: "Never Used", identity: identity)
        
        let recent = profileManager.recentProfiles
        
        XCTAssertEqual(recent.count, 0)
    }
    
    // MARK: - UserIdentity Edge Cases
    
    func testSignWithoutKey() {
        let identity = UserIdentity()
        // Don't generate key
        
        let testData = "test".data(using: .utf8)!
        let signature = identity.sign(testData)
        
        XCTAssertNil(signature)
    }
    
    func testSignEmptyData() {
        let identity = UserIdentity()
        identity.generateKeypair()
        
        let emptyData = Data()
        let signature = identity.sign(emptyData)
        
        XCTAssertNotNil(signature)
        XCTAssertEqual(signature?.count, 64)
    }
    
    func testSignLargeData() {
        let identity = UserIdentity()
        identity.generateKeypair()
        
        let largeData = Data(repeating: 0x42, count: 1_000_000) // 1MB
        let signature = identity.sign(largeData)
        
        XCTAssertNotNil(signature)
        XCTAssertEqual(signature?.count, 64)
    }
    
    func testExportWithoutKey() {
        let identity = UserIdentity()
        identity.id = UUID()
        identity.displayName = "Test"
        // Don't generate key
        
        let result = identity.exportProfile()
        
        XCTAssertNil(result)
    }
    
    func testImportWithCorruptedPrivateKey() {
        let corruptedJSON = """
        {
            "version": 1,
            "id": "\(UUID().uuidString)",
            "displayName": "Test",
            "publicKey": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "privateKey": "not-valid-base64!!!"
        }
        """.data(using: .utf8)!
        
        let result = UserIdentity.importProfile(from: corruptedJSON)
        
        XCTAssertNil(result)
    }
    
    func testImportWithWrongKeyLength() {
        let wrongLengthJSON = """
        {
            "version": 1,
            "id": "\(UUID().uuidString)",
            "displayName": "Test",
            "publicKey": "0123456789abcdef",
            "privateKey": "YWJjZA=="
        }
        """.data(using: .utf8)!
        
        let result = UserIdentity.importProfile(from: wrongLengthJSON)
        
        XCTAssertNil(result)
    }
    
    // MARK: - Concurrent Access Tests
    
    func testConcurrentServerAdds() async {
        await withTaskGroup(of: Void.self) { group in
            for i in 1...10 {
                group.addTask { @MainActor in
                    let server = ServerProfile(name: "Server \(i)", host: "127.0.0.\(i)", port: 8443)
                    self.serverManager.addServer(server)
                }
            }
        }
        
        XCTAssertEqual(serverManager.servers.count, 10)
    }
    
    func testConcurrentProfileCreation() async {
        await withTaskGroup(of: Void.self) { group in
            for i in 1...10 {
                group.addTask { @MainActor in
                    let identity = UserIdentity()
                    identity.id = UUID()
                    identity.displayName = "User \(i)"
                    identity.generateKeypair()
                    self.profileManager.createProfile(displayName: "User \(i)", identity: identity)
                }
            }
        }
        
        XCTAssertEqual(profileManager.profiles.count, 10)
    }
}
