import XCTest
import CryptoKit
@testable import Aura

@MainActor
final class UserIdentityTests: XCTestCase {
    
    var identity: UserIdentity!
    
    override func setUp() async throws {
        identity = UserIdentity()
        
        // Clean up test keychain entries
        if let id = identity.id {
            UserIdentity.deleteFromKeychain(id: id)
        }
    }
    
    override func tearDown() async throws {
        if let id = identity.id {
            UserIdentity.deleteFromKeychain(id: id)
        }
        identity = nil
    }
    
    // MARK: - Key Generation Tests
    
    func testGenerateKeypair() {
        identity.generateKeypair()
        
        XCTAssertNotNil(identity.publicKey)
        XCTAssertFalse(identity.publicKeyHex.isEmpty)
        XCTAssertEqual(identity.publicKey?.count, 32) // Ed25519 public key is 32 bytes
    }
    
    func testPublicKeyHexFormat() {
        identity.generateKeypair()
        
        let hexString = identity.publicKeyHex
        
        // Should be 64 hex characters (32 bytes * 2)
        XCTAssertEqual(hexString.count, 64)
        
        // Should only contain hex characters
        let hexCharacterSet = CharacterSet(charactersIn: "0123456789abcdef")
        XCTAssertTrue(hexString.lowercased().unicodeScalars.allSatisfy { hexCharacterSet.contains($0) })
    }
    
    // MARK: - Signing Tests
    
    func testSignData() {
        identity.generateKeypair()
        
        let testData = "Hello, Aura!".data(using: .utf8)!
        let signature = identity.sign(testData)
        
        XCTAssertNotNil(signature)
        XCTAssertEqual(signature?.count, 64) // Ed25519 signature is 64 bytes
    }
    
    func testSignatureVerification() throws {
        identity.generateKeypair()
        
        let testData = "Test message".data(using: .utf8)!
        guard let signature = identity.sign(testData) else {
            XCTFail("Failed to sign data")
            return
        }
        
        // Verify signature using CryptoKit
        guard let publicKeyData = identity.publicKey else {
            XCTFail("No public key")
            return
        }
        
        let publicKey = try Curve25519.Signing.PublicKey(rawRepresentation: publicKeyData)
        XCTAssertTrue(publicKey.isValidSignature(signature, for: testData))
    }
    
    func testDifferentDataProducesDifferentSignatures() {
        identity.generateKeypair()
        
        let data1 = "Message 1".data(using: .utf8)!
        let data2 = "Message 2".data(using: .utf8)!
        
        let signature1 = identity.sign(data1)
        let signature2 = identity.sign(data2)
        
        XCTAssertNotEqual(signature1, signature2)
    }
    
    // MARK: - Keychain Tests (Non-Biometric)
    
    func testKeychainSaveAndLoad() {
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()
        
        let originalPublicKey = identity.publicKeyHex
        
        // Save to keychain
        identity.saveToKeychain(requiresBiometric: false)
        
        // Load from keychain
        guard let loadedIdentity = UserIdentity.loadFromKeychain(id: identity.id!, requiresBiometric: false) else {
            XCTFail("Failed to load identity from keychain")
            return
        }
        
        XCTAssertEqual(loadedIdentity.publicKeyHex, originalPublicKey)
        XCTAssertEqual(loadedIdentity.id, identity.id)
    }
    
    func testKeychainDelete() {
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()
        
        // Save to keychain
        identity.saveToKeychain(requiresBiometric: false)
        
        // Verify it exists
        XCTAssertNotNil(UserIdentity.loadFromKeychain(id: identity.id!, requiresBiometric: false))
        
        // Delete from keychain
        UserIdentity.deleteFromKeychain(id: identity.id!)
        
        // Verify it's gone
        XCTAssertNil(UserIdentity.loadFromKeychain(id: identity.id!, requiresBiometric: false))
    }
    
    // MARK: - Import/Export Tests
    
    func testExportProfile() {
        identity.id = UUID()
        identity.displayName = "Export Test"
        identity.generateKeypair()
        
        guard let exportData = identity.exportProfile() else {
            XCTFail("Failed to export profile")
            return
        }
        
        XCTAssertFalse(exportData.isEmpty)
        
        // Verify it's valid JSON
        let json = try? JSONSerialization.jsonObject(with: exportData) as? [String: Any]
        XCTAssertNotNil(json)
        XCTAssertEqual(json?["version"] as? Int, 1)
        XCTAssertEqual(json?["displayName"] as? String, "Export Test")
        XCTAssertNotNil(json?["publicKey"])
        XCTAssertNotNil(json?["privateKey"])
    }
    
    func testImportProfile() {
        identity.id = UUID()
        identity.displayName = "Import Test"
        identity.generateKeypair()
        
        let originalPublicKey = identity.publicKeyHex
        
        guard let exportData = identity.exportProfile() else {
            XCTFail("Failed to export profile")
            return
        }
        
        guard let importedIdentity = UserIdentity.importProfile(from: exportData) else {
            XCTFail("Failed to import profile")
            return
        }
        
        XCTAssertEqual(importedIdentity.displayName, "Import Test")
        XCTAssertEqual(importedIdentity.publicKeyHex, originalPublicKey)
        XCTAssertEqual(importedIdentity.id, identity.id)
    }
    
    func testImportExportRoundTrip() {
        identity.id = UUID()
        identity.displayName = "Round Trip Test"
        identity.generateKeypair()
        
        let testData = "Test signature".data(using: .utf8)!
        guard let originalSignature = identity.sign(testData) else {
            XCTFail("Failed to sign data")
            return
        }
        
        // Export
        guard let exportData = identity.exportProfile() else {
            XCTFail("Failed to export")
            return
        }
        
        // Import
        guard let importedIdentity = UserIdentity.importProfile(from: exportData) else {
            XCTFail("Failed to import")
            return
        }
        
        // Verify imported identity can produce same signature
        guard let importedSignature = importedIdentity.sign(testData) else {
            XCTFail("Failed to sign with imported identity")
            return
        }
        
        XCTAssertEqual(originalSignature, importedSignature)
    }
    
    func testImportInvalidJSON() {
        let invalidData = "not json".data(using: .utf8)!
        
        let result = UserIdentity.importProfile(from: invalidData)
        XCTAssertNil(result)
    }
    
    func testImportMissingFields() {
        let incompleteJSON = """
        {
            "version": 1,
            "displayName": "Test"
        }
        """.data(using: .utf8)!
        
        let result = UserIdentity.importProfile(from: incompleteJSON)
        XCTAssertNil(result)
    }
    
    func testImportWrongVersion() {
        let wrongVersionJSON = """
        {
            "version": 999,
            "id": "\(UUID().uuidString)",
            "displayName": "Test",
            "publicKey": "abc123",
            "privateKey": "def456"
        }
        """.data(using: .utf8)!
        
        let result = UserIdentity.importProfile(from: wrongVersionJSON)
        XCTAssertNil(result)
    }
    
    // MARK: - Display Name Tests
    
    func testSaveDisplayName() {
        identity.saveDisplayName("Test Name")
        
        XCTAssertEqual(identity.displayName, "Test Name")
        XCTAssertEqual(UserDefaults.standard.string(forKey: "AuraDisplayName"), "Test Name")
    }
    
    func testLoadOrGenerateWithSavedName() {
        UserDefaults.standard.set("Saved Name", forKey: "AuraDisplayName")
        
        identity.loadOrGenerate()
        
        XCTAssertEqual(identity.displayName, "Saved Name")
        XCTAssertNotNil(identity.publicKey)
    }
    
    func testLoadOrGenerateWithoutSavedName() {
        UserDefaults.standard.removeObject(forKey: "AuraDisplayName")
        
        identity.loadOrGenerate()
        
        XCTAssertFalse(identity.displayName.isEmpty)
        XCTAssertTrue(identity.displayName.hasPrefix("User"))
        XCTAssertNotNil(identity.publicKey)
    }
}
