import XCTest
import CryptoKit
@testable import Aura

@MainActor
final class SecureEnclaveTests: XCTestCase {
    
    // MARK: - Secure Enclave Key Wrapping Tests
    
    func testSecureEnclaveKeyGeneration() {
        let profileId = UUID()
        
        // Note: This test may fail on systems without Secure Enclave
        // (e.g., Intel Macs, simulators without biometric support)
        guard let enclaveKey = UserIdentity.getOrCreateSecureEnclaveKey(profileId: profileId) else {
            XCTSkip("Secure Enclave not available on this system")
            return
        }
        
        XCTAssertNotNil(enclaveKey)
        
        // Verify we can retrieve the same key
        let retrievedKey = UserIdentity.getOrCreateSecureEnclaveKey(profileId: profileId)
        XCTAssertNotNil(retrievedKey)
    }
    
    func testKeyWrappingWithBiometric() {
        let profileId = UUID()
        let testKeyData = Data(repeating: 0x42, count: 32) // 32-byte test key
        
        // Attempt to wrap key
        guard let wrappedData = UserIdentity.wrapKeyWithBiometric(keyData: testKeyData, profileId: profileId) else {
            XCTSkip("Secure Enclave not available or biometric auth failed")
            return
        }
        
        XCTAssertFalse(wrappedData.isEmpty)
        XCTAssertNotEqual(wrappedData, testKeyData) // Should be encrypted
    }
    
    func testKeyUnwrappingWithBiometric() {
        let profileId = UUID()
        let testKeyData = Data(repeating: 0x42, count: 32)
        
        // Wrap key
        guard let wrappedData = UserIdentity.wrapKeyWithBiometric(keyData: testKeyData, profileId: profileId) else {
            XCTSkip("Secure Enclave not available")
            return
        }
        
        // Unwrap key (may require biometric auth)
        guard let unwrappedData = UserIdentity.unwrapKeyWithBiometric(wrappedData: wrappedData, profileId: profileId) else {
            XCTSkip("Biometric authentication failed or cancelled")
            return
        }
        
        XCTAssertEqual(unwrappedData, testKeyData)
    }
    
    func testWrapUnwrapRoundTrip() {
        let profileId = UUID()

        // Generate a real Ed25519 key directly. We deliberately don't go
        // through UserIdentity here so tests don't need access to its
        // private signingKey storage.
        let key = Curve25519.Signing.PrivateKey()
        let keyData = key.rawRepresentation
        
        // Wrap
        guard let wrappedData = UserIdentity.wrapKeyWithBiometric(keyData: keyData, profileId: profileId) else {
            XCTSkip("Secure Enclave not available")
            return
        }
        
        // Unwrap
        guard let unwrappedData = UserIdentity.unwrapKeyWithBiometric(wrappedData: wrappedData, profileId: profileId) else {
            XCTSkip("Biometric authentication failed")
            return
        }
        
        // Verify key still works
        do {
            let recoveredKey = try Curve25519.Signing.PrivateKey(rawRepresentation: unwrappedData)
            let testData = "test".data(using: .utf8)!
            let signature = try recoveredKey.signature(for: testData)
            XCTAssertEqual(signature.count, 64)
        } catch {
            XCTFail("Failed to use recovered key: \\(error)")
        }
    }
    
    func testInvalidWrappedDataHandling() {
        let profileId = UUID()
        let invalidData = Data(repeating: 0xFF, count: 100)
        
        // Should return nil for invalid data
        let result = UserIdentity.unwrapKeyWithBiometric(wrappedData: invalidData, profileId: profileId)
        XCTAssertNil(result)
    }
    
    func testDifferentProfileIdsCantUnwrap() {
        let profileId1 = UUID()
        let profileId2 = UUID()
        let testKeyData = Data(repeating: 0x42, count: 32)
        
        // Wrap with profile 1
        guard let wrappedData = UserIdentity.wrapKeyWithBiometric(keyData: testKeyData, profileId: profileId1) else {
            XCTSkip("Secure Enclave not available")
            return
        }
        
        // Try to unwrap with profile 2 (should fail)
        let result = UserIdentity.unwrapKeyWithBiometric(wrappedData: wrappedData, profileId: profileId2)
        XCTAssertNil(result)
    }
}
