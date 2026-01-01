import XCTest
@testable import Aura

@MainActor
final class FuzzTests: XCTestCase {
    
    // MARK: - Server Profile Fuzzing
    
    func testServerProfileWithRandomData() {
        for iteration in 0..<100 {
            let randomName = randomString(length: Int.random(in: 0...500))
            let randomHost = randomString(length: Int.random(in: 0...255))
            let randomPort = UInt16.random(in: 0...65535)
            let randomPassword = Bool.random() ? randomString(length: Int.random(in: 0...100)) : nil
            
            let server = ServerProfile(
                name: randomName,
                host: randomHost,
                port: randomPort,
                password: randomPassword,
                isFavorite: Bool.random()
            )
            
            let manager = ServerManager()
            
            // Should not crash
            manager.addServer(server)
            XCTAssertEqual(manager.servers.count, 1)
            
            // Test encoding/decoding
            do {
                let encoded = try JSONEncoder().encode(server)
                let decoded = try JSONDecoder().decode(ServerProfile.self, from: encoded)
                XCTAssertEqual(decoded.name, randomName)
                XCTAssertEqual(decoded.host, randomHost)
                XCTAssertEqual(decoded.port, randomPort)
            } catch {
                XCTFail("Encoding/decoding failed on iteration \(iteration): \(error)")
            }
        }
    }
    
    // MARK: - Profile Import Fuzzing
    
    func testProfileImportWithRandomJSON() {
        for _ in 0..<100 {
            let randomJSON = generateRandomJSON()
            
            // Should handle gracefully without crashing
            let result = UserIdentity.importProfile(from: randomJSON)
            
            // Most random data should fail to import
            // We're just checking it doesn't crash
        }
    }
    
    func testProfileImportWithMalformedJSON() {
        let malformedInputs: [Data] = [
            Data(),  // Empty
            "not json".data(using: .utf8)!,  // Plain text
            "{".data(using: .utf8)!,  // Incomplete JSON
            "{}".data(using: .utf8)!,  // Empty object
            "{\"version\":\"not a number\"}".data(using: .utf8)!,  // Wrong type
            randomData(length: 10000),  // Random bytes
        ]
        
        for input in malformedInputs {
            let result = UserIdentity.importProfile(from: input)
            XCTAssertNil(result, "Should reject malformed input")
        }
    }
    
    // MARK: - Keychain Fuzzing
    
    func testKeychainWithRandomIdentities() {
        var createdIds: [UUID] = []
        
        for _ in 0..<50 {
            let identity = UserIdentity()
            identity.id = UUID()
            identity.displayName = randomString(length: Int.random(in: 0...100))
            identity.generateKeypair()
            
            // Save to keychain
            identity.saveToKeychain(requiresBiometric: false)
            createdIds.append(identity.id!)
            
            // Try to load back
            let loaded = UserIdentity.loadFromKeychain(id: identity.id!, requiresBiometric: false)
            XCTAssertNotNil(loaded)
            XCTAssertEqual(loaded?.publicKeyHex, identity.publicKeyHex)
        }
        
        // Cleanup
        for id in createdIds {
            UserIdentity.deleteFromKeychain(id: id)
        }
    }
    
    // MARK: - Signature Fuzzing
    
    func testSigningWithRandomData() {
        let identity = UserIdentity()
        identity.generateKeypair()
        
        for _ in 0..<100 {
            let randomDataLength = Int.random(in: 0...10000)
            let testData = randomData(length: randomDataLength)
            
            // Should handle any data size
            let signature = identity.sign(testData)
            XCTAssertNotNil(signature)
            XCTAssertEqual(signature?.count, 64)
        }
    }
    
    // MARK: - Concurrent Access Fuzzing
    
    func testConcurrentServerOperations() async {
        let manager = ServerManager()
        
        await withTaskGroup(of: Void.self) { group in
            // Concurrent adds
            for i in 0..<20 {
                group.addTask { @MainActor in
                    let server = ServerProfile(
                        name: "Server \(i)",
                        host: "127.0.0.\(i % 255)",
                        port: UInt16.random(in: 1024...65535)
                    )
                    manager.addServer(server)
                }
            }
            
            // Concurrent deletes
            for _ in 0..<10 {
                group.addTask { @MainActor in
                    if let server = manager.servers.randomElement() {
                        manager.deleteServer(id: server.id)
                    }
                }
            }
            
            // Concurrent updates
            for _ in 0..<10 {
                group.addTask { @MainActor in
                    if var server = manager.servers.randomElement() {
                        server.name = self.randomString(length: 20)
                        manager.updateServer(server)
                    }
                }
            }
        }
        
        // Should not crash, final count is non-deterministic
        XCTAssertTrue(manager.servers.count >= 0)
    }
    
    // MARK: - Property-Based Testing
    
    func testServerProfileInvariants() {
        for _ in 0..<100 {
            let server = ServerProfile(
                name: randomString(length: Int.random(in: 0...100)),
                host: randomString(length: Int.random(in: 0...255)),
                port: UInt16.random(in: 0...65535)
            )
            
            // Invariant: ID should be unique
            let server2 = ServerProfile(
                name: server.name,
                host: server.host,
                port: server.port
            )
            XCTAssertNotEqual(server.id, server2.id)
            
            // Invariant: Encoding/decoding preserves data
            do {
                let encoded = try JSONEncoder().encode(server)
                let decoded = try JSONDecoder().decode(ServerProfile.self, from: encoded)
                XCTAssertEqual(decoded.id, server.id)
                XCTAssertEqual(decoded.name, server.name)
                XCTAssertEqual(decoded.host, server.host)
                XCTAssertEqual(decoded.port, server.port)
            } catch {
                XCTFail("Invariant violated: \(error)")
            }
        }
    }
    
    // MARK: - Helper Methods
    
    private func randomString(length: Int) -> String {
        let letters = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 !@#$%^&*()_+-=[]{}|;:',.<>?/~`"
        return String((0..<length).map { _ in letters.randomElement()! })
    }
    
    private func randomData(length: Int) -> Data {
        var data = Data(count: length)
        data.withUnsafeMutableBytes { ptr in
            if let baseAddress = ptr.baseAddress {
                arc4random_buf(baseAddress, length)
            }
        }
        return data
    }
    
    private func generateRandomJSON() -> Data {
        let randomStructures: [String] = [
            "{}",
            "{\"version\":1}",
            "{\"version\":1,\"id\":\"\(UUID().uuidString)\"}",
            "{\"version\":\(Int.random(in: -100...100))}",
            "{\"displayName\":\"\(randomString(length: 50))\"}",
            "{\"publicKey\":\"\(randomString(length: 64))\"}",
            "{\"privateKey\":\"\(randomString(length: 64))\"}",
            "[\(Int.random(in: 0...1000))]",
            "\"\(randomString(length: 100))\"",
            "null",
            "true",
            "false",
        ]
        
        return randomStructures.randomElement()!.data(using: .utf8)!
    }
}
