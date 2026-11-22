import XCTest
@testable import Aura

@MainActor
final class SecureEnclaveTests: XCTestCase {

    // MARK: - Key Tags

    func testSecureEnclaveKeyTagIsPerProfile() {
        let first = UUID()
        let second = UUID()

        let tag = UserIdentity.secureEnclaveKeyTag(profileId: first)

        XCTAssertEqual(String(decoding: tag, as: UTF8.self), "com.aura.enclave.\(first.uuidString)")
        XCTAssertNotEqual(tag, UserIdentity.secureEnclaveKeyTag(profileId: second))
        XCTAssertNotEqual(tag, UserIdentity.legacySharedSecureEnclaveKeyTag)
    }

    func testLegacySharedTagIsTheOldLiteral() {
        // Pre-hybrid wrapped keys were encrypted to the key with this exact
        // tag, so migration has to keep finding it.
        XCTAssertEqual(
            String(decoding: UserIdentity.legacySharedSecureEnclaveKeyTag, as: UTF8.self),
            "com.aura.enclave.\\(profileId.uuidString)"
        )
    }

    // MARK: - Secure Enclave Key Wrapping Tests

    func testSecureEnclaveKeyGeneration() throws {
        let profileId = UUID()
        defer { UserIdentity.deleteSecureEnclaveKey(profileId: profileId) }

        // Note: This test may fail on systems without Secure Enclave
        // (e.g., Intel Macs, simulators without biometric support)
        guard let enclaveKey = UserIdentity.getOrCreateSecureEnclaveKey(profileId: profileId) else {
            throw XCTSkip("Secure Enclave not available on this system")
        }

        XCTAssertNotNil(enclaveKey)

        // Verify we can retrieve the same key
        let retrievedKey = UserIdentity.getOrCreateSecureEnclaveKey(profileId: profileId)
        XCTAssertNotNil(retrievedKey)
    }

    func testKeyWrappingWithBiometric() throws {
        let profileId = UUID()
        defer { UserIdentity.deleteSecureEnclaveKey(profileId: profileId) }
        let testKeyData = generateIdentitySecret()

        // Attempt to wrap key
        guard let wrappedData = UserIdentity.wrapKeyWithBiometric(keyData: testKeyData, profileId: profileId) else {
            throw XCTSkip("Secure Enclave not available or biometric auth failed")
        }

        XCTAssertFalse(wrappedData.isEmpty)
        XCTAssertNotEqual(wrappedData, testKeyData) // Should be encrypted
    }

    func testKeyUnwrappingWithBiometric() throws {
        let profileId = UUID()
        defer { UserIdentity.deleteSecureEnclaveKey(profileId: profileId) }
        let testKeyData = generateIdentitySecret()

        // Wrap key
        guard let wrappedData = UserIdentity.wrapKeyWithBiometric(keyData: testKeyData, profileId: profileId) else {
            throw XCTSkip("Secure Enclave not available")
        }

        // Unwrap key (may require biometric auth)
        guard let unwrappedData = UserIdentity.unwrapKeyWithBiometric(wrappedData: wrappedData, profileId: profileId) else {
            throw XCTSkip("Biometric authentication failed or cancelled")
        }

        XCTAssertEqual(unwrappedData, testKeyData)
    }

    func testWrapUnwrapRoundTrip() throws {
        let profileId = UUID()
        defer { UserIdentity.deleteSecureEnclaveKey(profileId: profileId) }
        let secret = generateIdentitySecret()

        // Wrap
        guard let wrappedData = UserIdentity.wrapKeyWithBiometric(keyData: secret, profileId: profileId) else {
            throw XCTSkip("Secure Enclave not available")
        }

        // Unwrap
        guard let unwrappedData = UserIdentity.unwrapKeyWithBiometric(wrappedData: wrappedData, profileId: profileId) else {
            throw XCTSkip("Biometric authentication failed")
        }

        // The recovered secret is the same identity
        XCTAssertEqual(try identityPublicKey(identitySecret: unwrappedData), try identityPublicKey(identitySecret: secret))
    }

    func testInvalidWrappedDataHandling() {
        let profileId = UUID()
        let invalidData = Data(repeating: 0xFF, count: 100)

        // Should return nil for invalid data
        let result = UserIdentity.unwrapKeyWithBiometric(wrappedData: invalidData, profileId: profileId)
        XCTAssertNil(result)
    }

    func testDifferentProfileIdsCantUnwrap() throws {
        let profileId1 = UUID()
        let profileId2 = UUID()
        defer {
            UserIdentity.deleteSecureEnclaveKey(profileId: profileId1)
            UserIdentity.deleteSecureEnclaveKey(profileId: profileId2)
        }
        let testKeyData = generateIdentitySecret()

        // Wrap with profile 1
        guard let wrappedData = UserIdentity.wrapKeyWithBiometric(keyData: testKeyData, profileId: profileId1) else {
            throw XCTSkip("Secure Enclave not available")
        }

        // Try to unwrap with profile 2 (should fail)
        let result = UserIdentity.unwrapKeyWithBiometric(wrappedData: wrappedData, profileId: profileId2)
        XCTAssertNil(result)
    }
}
