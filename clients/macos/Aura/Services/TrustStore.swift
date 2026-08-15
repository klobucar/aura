import Foundation

// MARK: - State

/// How far a member's key has been checked. Drives every trust badge.
///
/// The design handoff's trust sheet renders exactly these, and nothing in
/// between: "three states only: verified / TOFU · not compared / key changed
/// today", plus the outcome of the sheet's Block action.
///
/// Owned here rather than in the view models because `TrustStore` is what
/// decides it; the rail and roster only display the result.
enum TrustState {
    /// Seen, recorded, never compared out of band. The honest default.
    case tofu

    /// Fingerprint compared out of band and accepted by the user.
    case verified

    /// This user presented a different key than the one first recorded for
    /// them. A reinstall looks identical to a takeover from here, which is
    /// precisely why it is surfaced rather than resolved automatically.
    case keyChanged

    /// Rejected from the trust sheet.
    case blocked
}

// MARK: - Persistence

/// The slice of `UserDefaults` the store needs, so tests can hand it a
/// throwaway backing store instead of the real user's preferences.
protocol TrustStoreDefaults: AnyObject {
    func data(forKey defaultName: String) -> Data?
    func set(_ value: Any?, forKey defaultName: String)
}

extension UserDefaults: TrustStoreDefaults {}

// MARK: - Store

/// Remembers which identity keys the user has verified, blocked, or seen
/// before, and turns that history into a `TrustState` for any member.
///
/// Everything is keyed by the Ed25519 public key, not by user id or display
/// name. A user who rotates keys — or an impostor who takes over a name —
/// starts from `TrustState.tofu` rather than inheriting trust, and a block
/// cannot be shaken off by reconnecting under a new session.
///
/// The keys themselves come from the authenticated MLS ratchet tree. They are
/// never taken from the server's user list, which carries no key material
/// precisely so it cannot be used this way.
///
/// The four UserDefaults keys below are shared verbatim with the desktop
/// client's settings file — the handoff requires identical persisted setting
/// keys across clients.
final class TrustStore {

    // MARK: Defaults keys

    private static let verifiedKeysDefaultsKey = "AuraVerifiedKeys"
    private static let blockedKeysDefaultsKey = "AuraBlockedKeys"
    private static let knownUserKeysDefaultsKey = "AuraKnownUserKeys"
    private static let keyChangedAtDefaultsKey = "AuraKeyChangedAt"

    // MARK: State

    /// Identity keys the user compared out of band and accepted, keyed by
    /// lowercase hex of the public key and valued with when it happened.
    ///
    /// Keyed by *key*, never by user: that is what makes a key rotation drop
    /// back to unverified instead of silently inheriting the old key's trust.
    private(set) var verifiedKeys: [String: Date] = [:]

    /// Identity keys the user rejected from the trust sheet. Also keyed by key
    /// hex, so an impostor cannot clear a block by reconnecting.
    private(set) var blockedKeys: [String: Date] = [:]

    /// First key seen for each user UUID — the TOFU baseline. A later key that
    /// disagrees with this is what raises "key changed"; without it a takeover
    /// would be indistinguishable from a first sighting.
    private(set) var knownUserKeys: [String: String] = [:]

    /// When a user's key was first seen to differ from its baseline, so the UI
    /// can say "key changed today".
    private(set) var keyChangedAt: [String: Date] = [:]

    private let defaults: TrustStoreDefaults
    private let clock: () -> Date

    // MARK: Lifecycle

    /// - Parameters:
    ///   - defaults: Backing store; also read at init, which is what makes a
    ///     verification survive a restart.
    ///   - clock: Injectable for tests.
    init(defaults: TrustStoreDefaults = UserDefaults.standard, clock: @escaping () -> Date = { Date() }) {
        self.defaults = defaults
        self.clock = clock
        load()
    }

    private func load() {
        let decoder = JSONDecoder()
        if let data = defaults.data(forKey: Self.verifiedKeysDefaultsKey),
           let decoded = try? decoder.decode([String: Date].self, from: data) {
            verifiedKeys = decoded
        }
        if let data = defaults.data(forKey: Self.blockedKeysDefaultsKey),
           let decoded = try? decoder.decode([String: Date].self, from: data) {
            blockedKeys = decoded
        }
        if let data = defaults.data(forKey: Self.knownUserKeysDefaultsKey),
           let decoded = try? decoder.decode([String: String].self, from: data) {
            knownUserKeys = decoded
        }
        if let data = defaults.data(forKey: Self.keyChangedAtDefaultsKey),
           let decoded = try? decoder.decode([String: Date].self, from: data) {
            keyChangedAt = decoded
        }
    }

    /// Called after every mutation. Verification is rare and losing one to a
    /// crash would mean asking the user to redo an out-of-band comparison, so
    /// this writes through rather than batching.
    private func save() {
        let encoder = JSONEncoder()
        if let data = try? encoder.encode(verifiedKeys) {
            defaults.set(data, forKey: Self.verifiedKeysDefaultsKey)
        }
        if let data = try? encoder.encode(blockedKeys) {
            defaults.set(data, forKey: Self.blockedKeysDefaultsKey)
        }
        if let data = try? encoder.encode(knownUserKeys) {
            defaults.set(data, forKey: Self.knownUserKeysDefaultsKey)
        }
        if let data = try? encoder.encode(keyChangedAt) {
            defaults.set(data, forKey: Self.keyChangedAtDefaultsKey)
        }
    }

    // MARK: Identity

    /// Lowercase hex, the form every dictionary here is keyed by.
    static func keyId(_ publicKey: Data) -> String {
        publicKey.hexString
    }

    // MARK: Mutation

    /// Record that `userUuid` is presenting `publicKey`.
    ///
    /// The first key seen for a user becomes their baseline. Later keys are
    /// deliberately *not* written over it — the mismatch is the signal, and
    /// silently re-baselining would erase it. Call this for every member
    /// whenever group membership is refreshed.
    func observe(userUuid: String, publicKey: Data) {
        guard !userUuid.isEmpty, !publicKey.isEmpty else { return }

        let id = Self.keyId(publicKey)

        guard let baseline = knownUserKeys[userUuid] else {
            knownUserKeys[userUuid] = id
            save()
            return
        }

        if baseline == id {
            // Back to a key we already knew — clear any stale changed-marker so
            // the badge does not linger after the user resolves it.
            if keyChangedAt.removeValue(forKey: userUuid) != nil { save() }
            return
        }

        // Stamp the first time we notice the disagreement, and only then, so
        // "key changed today" keeps meaning the day it actually changed.
        if keyChangedAt[userUuid] == nil {
            keyChangedAt[userUuid] = clock()
            save()
        }
    }

    /// The user compared fingerprints out of band and accepted this key.
    ///
    /// Also re-baselines the user, so a verified rotation stops reporting as
    /// changed — the user has just told us this key is the right one.
    func markVerified(userUuid: String, publicKey: Data) {
        guard !publicKey.isEmpty else { return }

        let id = Self.keyId(publicKey)
        verifiedKeys[id] = clock()
        blockedKeys.removeValue(forKey: id)

        if !userUuid.isEmpty {
            knownUserKeys[userUuid] = id
            keyChangedAt.removeValue(forKey: userUuid)
        }

        save()
    }

    /// The user rejected this key from the trust sheet. Blocks the key, not the
    /// person: if they later present the key we originally trusted, that one is
    /// still fine.
    func block(publicKey: Data) {
        guard !publicKey.isEmpty else { return }

        let id = Self.keyId(publicKey)
        blockedKeys[id] = clock()
        verifiedKeys.removeValue(forKey: id)
        save()
    }

    /// Undo a block. The key drops back to whatever it would otherwise be.
    func unblock(publicKey: Data) {
        guard !publicKey.isEmpty else { return }
        if blockedKeys.removeValue(forKey: Self.keyId(publicKey)) != nil { save() }
    }

    // MARK: Queries

    /// Current trust state for a member.
    func evaluate(userUuid: String, publicKey: Data) -> TrustState {
        guard !publicKey.isEmpty else { return .tofu }

        let id = Self.keyId(publicKey)

        // Block outranks everything: a blocked key stays blocked even if it was
        // verified earlier, because blocking is the later, more deliberate act.
        if blockedKeys[id] != nil { return .blocked }
        if verifiedKeys[id] != nil { return .verified }

        if let baseline = knownUserKeys[userUuid], baseline != id {
            return .keyChanged
        }

        return .tofu
    }

    /// When this user's key was first seen to change, if it has.
    func keyChangedAt(userUuid: String) -> Date? {
        keyChangedAt[userUuid]
    }

    /// When this key was verified, if it was.
    func verifiedAt(publicKey: Data) -> Date? {
        publicKey.isEmpty ? nil : verifiedKeys[Self.keyId(publicKey)]
    }
}
