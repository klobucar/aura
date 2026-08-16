import XCTest
@testable import Aura

/// Trust bookkeeping behind the trust sheet. The rules come from the design
/// handoff's §7 ("three states only: verified / TOFU · not compared / key
/// changed today") and its state table, where verifiedKeys is described as the
/// thing that "drives every trust badge".
///
/// The property worth defending here is that trust attaches to a *key*, not to
/// a name — otherwise a takeover inherits the trust of whoever held the name
/// before.
final class TrustStoreTests: XCTestCase {

    private final class FakeClock {
        /// 2026-01-01T12:00:00Z
        var now = Date(timeIntervalSince1970: 1_767_268_800)
        func advanceDays(_ days: Int) { now = now.addingTimeInterval(TimeInterval(days) * 86_400) }
    }

    /// Stands in for UserDefaults so the tests never touch the real user's
    /// preferences, and counts writes so write-through can be asserted.
    private final class InMemoryDefaults: TrustStoreDefaults {
        private(set) var storage: [String: Data] = [:]
        private(set) var writes = 0

        func data(forKey defaultName: String) -> Data? { storage[defaultName] }

        func set(_ value: Any?, forKey defaultName: String) {
            writes += 1
            storage[defaultName] = value as? Data
        }
    }

    private let alice = "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
    private let bob = "6ba7b811-9dad-11d1-80b4-00c04fd430c8"

    private func key(_ fill: UInt8) -> Data { Data(repeating: fill, count: 32) }

    private func newStore() -> (store: TrustStore, defaults: InMemoryDefaults, clock: FakeClock) {
        let defaults = InMemoryDefaults()
        let clock = FakeClock()
        let store = TrustStore(defaults: defaults, clock: { clock.now })
        return (store, defaults, clock)
    }

    func testUnknownKeyStartsAsTofu() {
        let (store, _, _) = newStore()
        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key(0x11)), .tofu)
    }

    func testObservedThenVerifiedBecomesVerified() {
        let (store, _, _) = newStore()
        let key = key(0x11)

        store.observe(userUuid: alice, publicKey: key)
        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key), .tofu)

        store.markVerified(userUuid: alice, publicKey: key)
        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key), .verified)
    }

    func testNewKeyForKnownUserReportsKeyChanged() {
        let (store, _, _) = newStore()

        store.observe(userUuid: alice, publicKey: key(0x11))
        store.observe(userUuid: alice, publicKey: key(0x22))   // reinstall, or a takeover

        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key(0x22)), .keyChanged)
    }

    func testVerifyingOneKeyDoesNotVerifyAnother() {
        // The whole point of keying on the key: trust must not transfer to a
        // different key just because the same person presents it.
        let (store, _, _) = newStore()

        store.observe(userUuid: alice, publicKey: key(0x11))
        store.markVerified(userUuid: alice, publicKey: key(0x11))

        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key(0x11)), .verified)
        XCTAssertNotEqual(store.evaluate(userUuid: alice, publicKey: key(0x22)), .verified)
    }

    func testTrustDoesNotTransferBetweenUsersSharingAName() {
        // Bob has never been seen; presenting Alice's verified key does make
        // that key verified (trust is per key), but Bob's own key must not be.
        let (store, _, _) = newStore()

        store.observe(userUuid: alice, publicKey: key(0x11))
        store.markVerified(userUuid: alice, publicKey: key(0x11))

        XCTAssertEqual(store.evaluate(userUuid: bob, publicKey: key(0x99)), .tofu)
    }

    func testVerifyingTheNewKeyClearsKeyChanged() {
        let (store, _, _) = newStore()

        store.observe(userUuid: alice, publicKey: key(0x11))
        store.observe(userUuid: alice, publicKey: key(0x22))
        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key(0x22)), .keyChanged)

        store.markVerified(userUuid: alice, publicKey: key(0x22))

        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key(0x22)), .verified)
        XCTAssertNil(store.keyChangedAt(userUuid: alice))
    }

    func testReturningToTheBaselineKeyClearsTheChangedMarker() {
        // Someone switches devices and switches back. The warning should not
        // outlive the condition that raised it.
        let (store, _, _) = newStore()

        store.observe(userUuid: alice, publicKey: key(0x11))
        store.observe(userUuid: alice, publicKey: key(0x22))
        XCTAssertNotNil(store.keyChangedAt(userUuid: alice))

        store.observe(userUuid: alice, publicKey: key(0x11))

        XCTAssertNil(store.keyChangedAt(userUuid: alice))
        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key(0x11)), .tofu)
    }

    func testKeyChangedAtStampsFirstSightingOnly() {
        let (store, _, clock) = newStore()

        store.observe(userUuid: alice, publicKey: key(0x11))
        store.observe(userUuid: alice, publicKey: key(0x22))
        let first = store.keyChangedAt(userUuid: alice)

        clock.advanceDays(3)
        store.observe(userUuid: alice, publicKey: key(0x22))   // still the same disagreement

        XCTAssertEqual(first, store.keyChangedAt(userUuid: alice))
    }

    func testBlockOutranksVerified() {
        let (store, _, _) = newStore()
        let key = key(0x11)

        store.markVerified(userUuid: alice, publicKey: key)
        store.block(publicKey: key)

        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key), .blocked)
    }

    func testBlockAppliesToTheKeyNotThePerson() {
        let (store, _, _) = newStore()

        store.observe(userUuid: alice, publicKey: key(0x11))
        store.markVerified(userUuid: alice, publicKey: key(0x11))
        store.block(publicKey: key(0x22))

        // The impostor key is blocked; the key Alice actually verified is not.
        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key(0x22)), .blocked)
        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key(0x11)), .verified)
    }

    func testVerifyingAfterBlockClearsTheBlock() {
        let (store, _, _) = newStore()
        let key = key(0x11)

        store.block(publicKey: key)
        store.markVerified(userUuid: alice, publicKey: key)

        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key), .verified)
    }

    func testUnblockRestoresThePreviousState() {
        let (store, _, _) = newStore()
        let key = key(0x11)

        store.observe(userUuid: alice, publicKey: key)
        store.block(publicKey: key)
        store.unblock(publicKey: key)

        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: key), .tofu)
    }

    func testDecisionsArePersistedThroughSettings() {
        // A verification is an out-of-band ritual; losing it to a crash means
        // asking the user to do it again, so every mutation writes through.
        let (store, defaults, clock) = newStore()
        let key = key(0x11)

        store.markVerified(userUuid: alice, publicKey: key)

        let id = TrustStore.keyId(key)
        XCTAssertNotNil(store.verifiedKeys[id])
        XCTAssertEqual(store.verifiedKeys[id], clock.now)
        XCTAssertEqual(store.knownUserKeys[alice], id)
        XCTAssertGreaterThan(defaults.writes, 0)
    }

    func testReloadedSettingsPreserveVerification() {
        let (store, defaults, _) = newStore()
        let key = key(0x11)
        store.markVerified(userUuid: alice, publicKey: key)

        // Same backing store, fresh store object — stands in for a restart.
        let reloaded = TrustStore(defaults: defaults)

        XCTAssertEqual(reloaded.evaluate(userUuid: alice, publicKey: key), .verified)
    }

    func testKeyIdIsLowercaseHex() {
        XCTAssertEqual(TrustStore.keyId(key(0x11)), String(repeating: "1", count: 64))
        XCTAssertEqual(TrustStore.keyId(key(0xAA)), String(repeating: "a", count: 64))
    }

    func testEmptyInputsAreIgnoredRatherThanStored() {
        let (store, _, _) = newStore()

        store.observe(userUuid: alice, publicKey: Data())
        store.observe(userUuid: "", publicKey: key(0x11))
        store.markVerified(userUuid: alice, publicKey: Data())
        store.block(publicKey: Data())

        XCTAssertTrue(store.knownUserKeys.isEmpty)
        XCTAssertTrue(store.verifiedKeys.isEmpty)
        XCTAssertTrue(store.blockedKeys.isEmpty)
        XCTAssertEqual(store.evaluate(userUuid: alice, publicKey: Data()), .tofu)
    }
}
