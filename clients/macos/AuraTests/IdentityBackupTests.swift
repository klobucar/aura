import XCTest
import AuraCore
@testable import Aura

/// Encrypted identity backups through aura-core. The key derivation is
/// deliberately slow (scrypt, N = 2^17), so the round trip runs once.
@MainActor
final class IdentityBackupTests: XCTestCase {

    func testRoundTripOffTheMainActor() async throws {
        let secret = generateIdentitySecret()
        let backup = try await IdentityBackup.encrypt(secret: secret, displayName: "Alice", passphrase: "pw")

        XCTAssertTrue(IdentityBackup.looksLikeBackup(backup))
        XCTAssertNil(backup.range(of: secret), "the secret must not appear in the file")

        let restored = try await IdentityBackup.decrypt(backup, passphrase: "pw")
        XCTAssertEqual(restored.secret, secret)
        XCTAssertEqual(restored.displayName, "Alice")
    }

    func testNotABackupIsRejectedBeforeAnyKeyDerivation() async {
        do {
            _ = try await IdentityBackup.decrypt(Data("{\"version\":2}".utf8), passphrase: "pw")
            XCTFail("expected NotABackup")
        } catch {
            XCTAssertEqual(error as? IdentityBackupError, .NotABackup)
            XCTAssertEqual(IdentityBackup.message(for: error), "This file isn't an Aura identity backup.")
        }
    }

    func testEmptyPassphraseIsRejected() async {
        do {
            _ = try await IdentityBackup.encrypt(secret: generateIdentitySecret(), displayName: "A", passphrase: "")
            XCTFail("expected EmptyPassphrase")
        } catch {
            XCTAssertEqual(error as? IdentityBackupError, .EmptyPassphrase)
        }
    }

    func testErrorMessages() {
        XCTAssertEqual(IdentityBackup.message(for: IdentityBackupError.WrongPassphraseOrDamaged),
                       "Wrong passphrase, or the file is damaged.")
        XCTAssertEqual(IdentityBackup.message(for: IdentityBackupError.UnsupportedVersion(version: 9)),
                       "This backup was made by a newer version of Aura. Update Aura to open it.")
    }

    func testPassphraseRules() {
        XCTAssertEqual(IdentityBackup.passphraseProblem("short", repeated: "short"), "Use at least 8 characters.")
        XCTAssertEqual(IdentityBackup.passphraseProblem("long enough", repeated: "long enougH"),
                       "The passphrases don't match.")
        XCTAssertNil(IdentityBackup.passphraseProblem("long enough", repeated: "long enough"))
        XCTAssertNil(IdentityBackup.passphraseProblem("12345678", repeated: "12345678"))
    }

    func testSuggestedFileName() {
        XCTAssertEqual(IdentityBackup.suggestedFileName(displayName: "Alice"), "Alice.aura-identity")
        XCTAssertEqual(IdentityBackup.suggestedFileName(displayName: "a/b:c"), "abc.aura-identity")
        XCTAssertEqual(IdentityBackup.suggestedFileName(displayName: " "), "Aura identity.aura-identity")
    }

    func testRestoredIdentityGetsAFreshProfileId() throws {
        let original = UserIdentity()
        original.generateKeypair()
        original.id = UUID()
        let secret = try XCTUnwrap(original.secretForBackup())

        let restored = try XCTUnwrap(UserIdentity.restored(secret: secret, displayName: "Alice"))
        XCTAssertEqual(restored.publicKeyHex, original.publicKeyHex)
        XCTAssertEqual(restored.identityKey, original.identityKey)
        XCTAssertEqual(restored.displayName, "Alice")
        XCTAssertNotNil(restored.id)
        XCTAssertNotEqual(restored.id, original.id)

        XCTAssertNil(UserIdentity.restored(secret: Data(count: 3), displayName: "x"))
    }
}
