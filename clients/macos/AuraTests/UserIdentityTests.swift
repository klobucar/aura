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

        XCTAssertTrue(identity.hasIdentity)
        XCTAssertFalse(identity.publicKeyHex.isEmpty)
        XCTAssertEqual(identity.publicKey?.count, IdentitySizes.ed25519Key)
        XCTAssertEqual(identity.identityKey?.count, IdentitySizes.identityKey)
    }

    func testPublicKeyHexFormat() {
        identity.generateKeypair()

        let hexString = identity.publicKeyHex

        // The Ed25519 key as 64 lowercase hex characters, the form
        // AURA_BOOTSTRAP_ADMIN_KEY takes
        XCTAssertEqual(hexString.count, 64)
        let hexCharacterSet = CharacterSet(charactersIn: "0123456789abcdef")
        XCTAssertTrue(hexString.unicodeScalars.allSatisfy { hexCharacterSet.contains($0) })
        XCTAssertEqual(hexString, identity.publicKey?.hexString)
    }

    func testIdentityKeyEndsWithTheEd25519Key() {
        // ML-DSA-65 key || Ed25519 key
        identity.generateKeypair()

        XCTAssertEqual(identity.identityKey?.suffix(IdentitySizes.ed25519Key), identity.publicKey)
    }

    func testGeneratedIdentitiesAreDistinct() {
        identity.generateKeypair()
        let other = UserIdentity()
        other.generateKeypair()

        XCTAssertNotEqual(identity.identityKey, other.identityKey)
        XCTAssertNotEqual(identity.publicKeyHex, other.publicKeyHex)
    }

    // MARK: - Login Request Tests

    func testAuthRequestFrame() throws {
        identity.displayName = "Alice"
        identity.generateKeypair()
        let challenge = Data((0..<32).map { UInt8($0) })

        let frame = try identity.authRequestFrame(challenge: challenge, serverPassword: "hunter2")

        guard let fields = authRequestFields(frame) else {
            return XCTFail("Not a [0x03][u32 LE length][AuthRequest] frame")
        }
        XCTAssertEqual(fields[1], identity.identityKey)
        XCTAssertEqual(fields[2], Data("Alice".utf8))
        XCTAssertEqual(fields[3]?.count, IdentitySizes.signature)
        XCTAssertEqual(fields[4], Data("hunter2".utf8))
        XCTAssertEqual(fields[5], challenge)
    }

    func testAuthRequestWithoutPasswordOmitsIt() throws {
        identity.generateKeypair()

        let frame = try identity.authRequestFrame(challenge: Data(repeating: 7, count: 32), serverPassword: nil)

        XCTAssertNil(authRequestFields(frame)?[4])
    }

    func testAuthRequestSignsTheChallenge() throws {
        identity.generateKeypair()

        let first = try identity.authRequestFrame(challenge: Data(repeating: 1, count: 32), serverPassword: nil)
        let second = try identity.authRequestFrame(challenge: Data(repeating: 2, count: 32), serverPassword: nil)

        XCTAssertNotEqual(authRequestFields(first)?[3], authRequestFields(second)?[3])
    }

    func testAuthRequestRejectsWrongChallengeLength() {
        identity.generateKeypair()

        XCTAssertThrowsError(try identity.authRequestFrame(challenge: Data(repeating: 0, count: 31), serverPassword: nil))
    }

    func testAuthRequestWithoutIdentityThrows() {
        XCTAssertThrowsError(try identity.authRequestFrame(challenge: Data(repeating: 0, count: 32), serverPassword: nil))
    }

    // MARK: - MLS Tests

    func testMlsMemberCarriesTheIdentityKey() throws {
        identity.generateKeypair()
        let mls = try identity.makeMlsWrapper(userId: "U_alice")
        try mls.createGroup(channelId: "1", isVoice: true)

        let me = try mls.groupMembers(channelId: "1", isVoice: true).first { $0.isSelf }

        XCTAssertEqual(me?.identity, "U_alice")
        XCTAssertEqual(me?.identityKey, identity.identityKey)
    }

    // MARK: - Keychain Tests (Non-Biometric)

    func testKeychainSaveAndLoad() {
        identity.id = UUID()
        identity.displayName = "Test User"
        identity.generateKeypair()

        let originalPublicKey = identity.publicKeyHex

        // Save to keychain
        XCTAssertTrue(identity.saveToKeychain(requiresBiometric: false))

        // Load from keychain
        guard let loadedIdentity = UserIdentity.loadFromKeychain(id: identity.id!, requiresBiometric: false) else {
            XCTFail("Failed to load identity from keychain")
            return
        }

        XCTAssertEqual(loadedIdentity.publicKeyHex, originalPublicKey)
        XCTAssertEqual(loadedIdentity.identityKey, identity.identityKey)
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

    // MARK: - Profile Storage Tests (in-memory keychain)

    func testProfileStoresTheIdentitySecret() {
        let keychain = InMemoryKeychain()
        let id = UUID()
        let profile = UserIdentity()
        profile.id = id
        profile.generateKeypair()

        XCTAssertTrue(profile.saveToKeychain(keychain: keychain))

        let service = IdentityKeychainItem.service(forProfile: id)
        XCTAssertEqual(keychain.item(service: service, account: IdentityKeychainItem.secretAccount)?.count, IdentitySizes.secret)
        let loaded = UserIdentity.loadFromKeychain(id: id, keychain: keychain)
        XCTAssertEqual(loaded?.identityKey, profile.identityKey)
    }

    func testLegacyEd25519ItemIsUpgradedOnLoad() {
        let keychain = InMemoryKeychain()
        let id = UUID()
        let service = IdentityKeychainItem.service(forProfile: id)
        let legacyKey = Curve25519.Signing.PrivateKey()
        keychain.put(legacyKey.rawRepresentation, service: service, account: IdentityKeychainItem.legacyKeyAccount)

        guard let upgraded = UserIdentity.loadFromKeychain(id: id, keychain: keychain) else {
            return XCTFail("Legacy item was not loaded")
        }

        // Same Ed25519 key, now with an ML-DSA key
        XCTAssertEqual(upgraded.publicKeyHex, legacyKey.publicKey.rawRepresentation.hexString)
        XCTAssertEqual(upgraded.identityKey?.count, IdentitySizes.identityKey)
        // Stored in the new form, old item gone
        let stored = keychain.item(service: service, account: IdentityKeychainItem.secretAccount)
        XCTAssertEqual(stored?.count, IdentitySizes.secret)
        XCTAssertEqual(stored?.prefix(32), legacyKey.rawRepresentation)
        XCTAssertNil(keychain.item(service: service, account: IdentityKeychainItem.legacyKeyAccount))
        // The upgrade is stored, so the next load is the same identity
        XCTAssertEqual(UserIdentity.loadFromKeychain(id: id, keychain: keychain)?.identityKey, upgraded.identityKey)
    }

    func testLegacyItemIsKeptWhenTheUpgradeCannotBeSaved() {
        let keychain = InMemoryKeychain()
        let id = UUID()
        let service = IdentityKeychainItem.service(forProfile: id)
        let legacy = Curve25519.Signing.PrivateKey().rawRepresentation
        keychain.put(legacy, service: service, account: IdentityKeychainItem.legacyKeyAccount)
        keychain.writeFailure = errSecInteractionNotAllowed

        XCTAssertNil(UserIdentity.loadFromKeychain(id: id, keychain: keychain))
        XCTAssertEqual(keychain.item(service: service, account: IdentityKeychainItem.legacyKeyAccount), legacy)
    }

    func testSavingRemovesOtherFormsOfTheKey() {
        let keychain = InMemoryKeychain()
        let id = UUID()
        let service = IdentityKeychainItem.service(forProfile: id)
        for account in [IdentityKeychainItem.wrappedSecretAccount,
                        IdentityKeychainItem.legacyKeyAccount,
                        IdentityKeychainItem.legacyWrappedKeyAccount] {
            keychain.put(Data(repeating: 1, count: 32), service: service, account: account)
        }
        let profile = UserIdentity()
        profile.id = id
        profile.generateKeypair()

        XCTAssertTrue(profile.saveToKeychain(requiresBiometric: false, keychain: keychain))

        for account in IdentityKeychainItem.profileAccounts where account != IdentityKeychainItem.secretAccount {
            XCTAssertNil(keychain.item(service: service, account: account), account)
        }
    }

    func testDeleteRemovesEveryFormOfTheKey() {
        let keychain = InMemoryKeychain()
        let id = UUID()
        for account in IdentityKeychainItem.profileAccounts {
            keychain.put(Data(repeating: 1, count: 32), service: IdentityKeychainItem.service(forProfile: id), account: account)
        }

        UserIdentity.deleteFromKeychain(id: id, keychain: keychain)

        XCTAssertTrue(keychain.isEmpty)
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
        XCTAssertEqual(json?["version"] as? Int, 2)
        XCTAssertEqual(json?["displayName"] as? String, "Export Test")
        XCTAssertEqual(json?["publicKey"] as? String, identity.publicKeyHex)
        let secret = (json?["identitySecret"] as? String).flatMap { Data(base64Encoded: $0) }
        XCTAssertEqual(secret?.count, IdentitySizes.secret)
        XCTAssertNil(json?["privateKey"])
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

        // The whole hybrid key survives, not just the Ed25519 half
        XCTAssertEqual(importedIdentity.identityKey, identity.identityKey)
    }

    func testImportVersion1UpgradesAndKeepsTheEd25519Key() {
        let legacyKey = Curve25519.Signing.PrivateKey()
        let id = UUID()
        let bundle: [String: Any] = [
            "version": 1,
            "id": id.uuidString,
            "displayName": "Legacy",
            "publicKey": legacyKey.publicKey.rawRepresentation.hexString,
            "privateKey": legacyKey.rawRepresentation.base64EncodedString()
        ]
        let data = try! JSONSerialization.data(withJSONObject: bundle)

        guard let imported = UserIdentity.importProfile(from: data) else {
            return XCTFail("Failed to import a version 1 bundle")
        }

        XCTAssertEqual(imported.publicKeyHex, legacyKey.publicKey.rawRepresentation.hexString)
        XCTAssertEqual(imported.identityKey?.count, IdentitySizes.identityKey)
        XCTAssertEqual(imported.id, id)
        XCTAssertEqual(imported.displayName, "Legacy")

        // Re-exported as version 2
        let reexported = imported.exportProfile().flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
        XCTAssertEqual(reexported?["version"] as? Int, 2)
    }

    func testImportVersion2RejectsWrongSecretLength() {
        let json = """
        {
            "version": 2,
            "id": "\(UUID().uuidString)",
            "displayName": "Test",
            "identitySecret": "\(Data(repeating: 1, count: 32).base64EncodedString())"
        }
        """.data(using: .utf8)!

        XCTAssertNil(UserIdentity.importProfile(from: json))
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

        identity.loadOrGenerate(keychain: InMemoryKeychain())

        XCTAssertEqual(identity.displayName, "Saved Name")
        XCTAssertNotNil(identity.publicKey)
    }

    func testLoadOrGenerateWithoutSavedName() {
        UserDefaults.standard.removeObject(forKey: "AuraDisplayName")

        identity.loadOrGenerate(keychain: InMemoryKeychain())

        XCTAssertFalse(identity.displayName.isEmpty)
        XCTAssertTrue(identity.displayName.hasPrefix("User"))
        XCTAssertNotNil(identity.publicKey)
    }

    // MARK: - Login Identity Persistence Tests

    func testLoginIdentityPersistsAcrossLaunches() {
        let keychain = InMemoryKeychain()

        identity.loadOrGenerate(keychain: keychain)
        let nextLaunch = UserIdentity()
        nextLaunch.loadOrGenerate(keychain: keychain)

        XCTAssertEqual(identity.identityKey?.count, IdentitySizes.identityKey)
        XCTAssertEqual(nextLaunch.identityKey, identity.identityKey)
        XCTAssertEqual(
            keychain.item(service: IdentityKeychainItem.loginService, account: IdentityKeychainItem.secretAccount)?.count,
            IdentitySizes.secret
        )
    }

    func testLoginIdentityWithoutKeychainLastsForTheProcess() {
        let keychain = InMemoryKeychain()
        keychain.readFailure = errSecNotAvailable
        keychain.writeFailure = errSecNotAvailable

        identity.loadOrGenerate(keychain: keychain)
        let again = UserIdentity()
        again.loadOrGenerate(keychain: keychain)

        XCTAssertTrue(identity.hasIdentity)
        XCTAssertEqual(again.identityKey, identity.identityKey)
        XCTAssertTrue(keychain.isEmpty)
    }

    func testUnreadableLoginItemIsNotOverwritten() {
        // A locked keychain says nothing about whether an identity exists, so
        // one must not be written over it.
        let keychain = InMemoryKeychain()
        keychain.readFailure = errSecInteractionNotAllowed

        identity.loadOrGenerate(keychain: keychain)

        XCTAssertTrue(identity.hasIdentity)
        XCTAssertEqual(keychain.writes, 0)
    }
}
