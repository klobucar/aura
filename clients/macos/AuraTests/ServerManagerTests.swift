import XCTest
@testable import Aura

@MainActor
final class ServerManagerTests: XCTestCase {
    
    var serverManager: ServerManager!
    let testStorageKey = "TestAuraServerProfiles"
    
    override func setUp() async throws {
        // Use a test-specific storage key
        serverManager = ServerManager()
        UserDefaults.standard.removeObject(forKey: testStorageKey)
    }
    
    override func tearDown() async throws {
        UserDefaults.standard.removeObject(forKey: testStorageKey)
        serverManager = nil
    }
    
    // MARK: - CRUD Tests
    
    func testAddServer() {
        let server = ServerProfile(
            name: "Test Server",
            host: "127.0.0.1",
            port: 8443
        )
        
        serverManager.addServer(server)
        
        XCTAssertEqual(serverManager.servers.count, 1)
        XCTAssertEqual(serverManager.servers.first?.name, "Test Server")
        XCTAssertEqual(serverManager.servers.first?.host, "127.0.0.1")
    }
    
    func testUpdateServer() {
        var server = ServerProfile(
            name: "Original Name",
            host: "127.0.0.1",
            port: 8443
        )
        
        serverManager.addServer(server)
        
        server.name = "Updated Name"
        server.host = "192.168.1.1"
        serverManager.updateServer(server)
        
        XCTAssertEqual(serverManager.servers.count, 1)
        XCTAssertEqual(serverManager.servers.first?.name, "Updated Name")
        XCTAssertEqual(serverManager.servers.first?.host, "192.168.1.1")
    }
    
    func testDeleteServer() {
        let server = ServerProfile(
            name: "Test Server",
            host: "127.0.0.1",
            port: 8443
        )
        
        serverManager.addServer(server)
        XCTAssertEqual(serverManager.servers.count, 1)
        
        serverManager.deleteServer(id: server.id)
        XCTAssertEqual(serverManager.servers.count, 0)
    }
    
    // MARK: - Recent Servers Tests
    
    func testRecentServers() {
        let server1 = ServerProfile(name: "Server 1", host: "127.0.0.1", port: 8443)
        let server2 = ServerProfile(name: "Server 2", host: "127.0.0.2", port: 8443)
        let server3 = ServerProfile(name: "Server 3", host: "127.0.0.3", port: 8443)
        
        serverManager.addServer(server1)
        serverManager.addServer(server2)
        serverManager.addServer(server3)
        
        // Mark servers as used in specific order
        serverManager.markAsUsed(id: server1.id)
        Thread.sleep(forTimeInterval: 0.01) // Ensure different timestamps
        serverManager.markAsUsed(id: server3.id)
        Thread.sleep(forTimeInterval: 0.01)
        serverManager.markAsUsed(id: server2.id)
        
        let recent = serverManager.recentServers
        
        XCTAssertEqual(recent.count, 3)
        XCTAssertEqual(recent[0].name, "Server 2") // Most recent
        XCTAssertEqual(recent[1].name, "Server 3")
        XCTAssertEqual(recent[2].name, "Server 1") // Least recent
    }
    
    func testRecentServersLimit() {
        // Add 10 servers
        for i in 1...10 {
            let server = ServerProfile(name: "Server \(i)", host: "127.0.0.\(i)", port: 8443)
            serverManager.addServer(server)
            serverManager.markAsUsed(id: server.id)
            Thread.sleep(forTimeInterval: 0.01)
        }
        
        let recent = serverManager.recentServers
        XCTAssertEqual(recent.count, 5) // Should be limited to 5
    }
    
    // MARK: - Favorite Servers Tests
    
    func testFavoriteServers() {
        let server1 = ServerProfile(name: "Server 1", host: "127.0.0.1", port: 8443, isFavorite: true)
        let server2 = ServerProfile(name: "Server 2", host: "127.0.0.2", port: 8443, isFavorite: false)
        let server3 = ServerProfile(name: "Server 3", host: "127.0.0.3", port: 8443, isFavorite: true)
        
        serverManager.addServer(server1)
        serverManager.addServer(server2)
        serverManager.addServer(server3)
        
        let favorites = serverManager.favoriteServers
        
        XCTAssertEqual(favorites.count, 2)
        XCTAssertTrue(favorites.contains(where: { $0.name == "Server 1" }))
        XCTAssertTrue(favorites.contains(where: { $0.name == "Server 3" }))
    }
    
    // MARK: - Persistence Tests
    
    func testPersistence() {
        let server = ServerProfile(
            name: "Persistent Server",
            host: "127.0.0.1",
            port: 8443,
            password: "secret"
        )
        
        serverManager.addServer(server)
        
        // Create new manager instance (simulates app restart)
        let newManager = ServerManager()
        
        XCTAssertEqual(newManager.servers.count, 1)
        XCTAssertEqual(newManager.servers.first?.name, "Persistent Server")
        XCTAssertEqual(newManager.servers.first?.password, "secret")
    }
}
