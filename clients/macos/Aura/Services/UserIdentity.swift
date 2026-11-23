import Foundation
import Combine
import Security

// MARK: - Keychain

/// Generic-password storage for identity secrets.
///
/// `SystemKeychain` is the macOS Keychain. Tests substitute an in-memory
/// implementation, so the storage and migration logic can be checked where no
/// Keychain is usable (CI runners).
nonisolated public protocol SecretKeychain {
    /// The item's data, or nil and the failing status (`errSecItemNotFound`
    /// when there is no such item).
    func read(service: String, account: String) -> (data: Data?, status: OSStatus)
    /// Add a new item; `errSecDuplicateItem` if it already exists.
    func add(_ data: Data, service: String, account: String) -> OSStatus
    /// Replace the data of an existing item.
    func update(_ data: Data, service: String, account: String) -> OSStatus
    func delete(service: String, account: String) -> OSStatus
}

nonisolated extension SecretKeychain {
    /// Add the item, or replace the data of an existing one. Unlike
    /// delete-then-add, a failure leaves the old item in place.
    func write(_ data: Data, service: String, account: String) -> OSStatus {
        let status = add(data, service: service, account: account)
        return status == errSecDuplicateItem ? update(data, service: service, account: account) : status
    }
}

/// The macOS Keychain. Every call returns its `OSStatus`; none traps, so a
/// machine without a usable Keychain gets errors to handle, not a crash.
nonisolated public struct SystemKeychain: SecretKeychain {
    public init() {}

    private func baseQuery(service: String, account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
    }

    public func read(service: String, account: String) -> (data: Data?, status: OSStatus) {
        var query = baseQuery(service: service, account: account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess else { return (nil, status) }
        guard let data = result as? Data else { return (nil, errSecDecode) }
        return (data, status)
    }

    public func add(_ data: Data, service: String, account: String) -> OSStatus {
        var item = baseQuery(service: service, account: account)
        item[kSecValueData as String] = data
        return SecItemAdd(item as CFDictionary, nil)
    }

    public func update(_ data: Data, service: String, account: String) -> OSStatus {
        let changes: [String: Any] = [kSecValueData as String: data]
        return SecItemUpdate(baseQuery(service: service, account: account) as CFDictionary, changes as CFDictionary)
    }

    public func delete(service: String, account: String) -> OSStatus {
        SecItemDelete(baseQuery(service: service, account: account) as CFDictionary)
    }
}

/// Where identity secrets live in the Keychain.
enum IdentityKeychainItem {
    /// Service of the login identity, the one used when no profile is chosen.
    static let loginService = "com.aura.identity.default"

    /// Service of a profile's identity.
    static func service(forProfile id: UUID) -> String {
        "com.aura.identity.\(id.uuidString)"
    }

    /// The 64-byte hybrid identity secret.
    static let secretAccount = "identity-secret"
    /// The identity secret encrypted to the profile's Secure Enclave key.
    static let wrappedSecretAccount = "identity-secret-wrapped"
    /// Before the hybrid identity: a 32-byte Ed25519 secret key, stored
    /// plain. Read once, upgraded, then deleted.
    static let legacyKeyAccount = "ed25519-private-key"
    /// Before the hybrid identity: the Ed25519 secret key, wrapped.
    static let legacyWrappedKeyAccount = "ed25519-wrapped-key"

    /// Every account a profile's identity may be stored under.
    static let profileAccounts = [secretAccount, wrappedSecretAccount, legacyKeyAccount, legacyWrappedKeyAccount]
}

// MARK: - Identity

/// The user's long-term hybrid identity: an Ed25519 key and an ML-DSA-65 key,
/// held as one 64-byte secret (Ed25519 secret key || ML-DSA-65 seed).
///
/// All signing happens in aura-core: this class hands the secret to the FFI
/// to build the login request and the MLS client, and never signs anything
/// itself. Secrets are kept in the Keychain; a profile's may be encrypted to
/// a Secure Enclave key that needs biometric authentication.
@MainActor
public class UserIdentity: ObservableObject {

    @Published public var displayName: String = ""

    /// Lowercase hex of the 32-byte Ed25519 public key. The upgrade to the
    /// hybrid identity keeps this key, and it is the value a server's
    /// `AURA_BOOTSTRAP_ADMIN_KEY` takes.
    @Published public private(set) var publicKeyHex: String = ""

    public var id: UUID? // Profile ID for keychain storage

    /// The 64-byte identity secret.
    private var secret: Data?

    /// The 32-byte Ed25519 public key.
    public private(set) var publicKey: Data?

    /// The 1,984-byte hybrid identity public key (ML-DSA-65 key || Ed25519
    /// key): what the server pins and what other members verify.
    public private(set) var identityKey: Data?

    /// Login identity for when the Keychain cannot be used. Shared by the
    /// whole process, so reconnects within one run keep the same identity.
    private static var sessionSecret: Data?

    public init() {}

    /// True once a secret is loaded or generated.
    public var hasIdentity: Bool { secret != nil }

    // MARK: - Key Generation & Loading

    /// Generate a new identity (Ed25519 + ML-DSA-65).
    public func generateKeypair() {
        adopt(generateIdentitySecret())
        print("[Identity] Generated new identity, Ed25519 public key: \(publicKeyHex)")
    }

    /// Load the login identity from the keychain, creating and saving one on
    /// first use, so login presents the same identity across launches.
    ///
    /// If the keychain cannot be read or written, the identity lasts only for
    /// this run (every call in the process gets the same one) and nothing
    /// stored is overwritten.
    public func loadOrGenerate(keychain: SecretKeychain = SystemKeychain()) {
        // Keep display name stable in UserDefaults
        if let savedName = UserDefaults.standard.string(forKey: "AuraDisplayName"), !savedName.isEmpty {
            displayName = savedName
        } else if displayName.isEmpty {
            displayName = "User\(Int.random(in: 1000...9999))"
            UserDefaults.standard.set(displayName, forKey: "AuraDisplayName")
        }

        let service = IdentityKeychainItem.loginService
        let account = IdentityKeychainItem.secretAccount
        let (stored, status) = keychain.read(service: service, account: account)

        if let stored {
            if adopt(stored) {
                print("[Identity] Loaded login identity: \(publicKeyHex)")
                return
            }
            print("[Identity] Stored login identity is unusable (\(stored.count) bytes); using a temporary identity")
        } else if status == errSecItemNotFound {
            let fresh = generateIdentitySecret()
            // Add, never overwrite: an existing item is an identity to keep.
            let addStatus = keychain.add(fresh, service: service, account: account)
            if addStatus == errSecSuccess, adopt(fresh) {
                print("[Identity] Created login identity: \(publicKeyHex)")
                return
            }
            print("[Identity] Could not save the login identity (\(addStatus)); using a temporary identity")
            if UserIdentity.sessionSecret == nil { UserIdentity.sessionSecret = fresh }
        } else {
            print("[Identity] Keychain unavailable (\(status)); using a temporary identity")
        }

        let session = UserIdentity.sessionSecret ?? generateIdentitySecret()
        UserIdentity.sessionSecret = session
        adopt(session)
    }

    /// Save display name to UserDefaults.
    public func saveDisplayName(_ name: String) {
        displayName = name
        UserDefaults.standard.set(name, forKey: "AuraDisplayName")
    }

    /// Make `secret` this identity. Leaves the identity unchanged and returns
    /// false if aura-core rejects it (wrong length).
    @discardableResult
    private func adopt(_ secret: Data) -> Bool {
        do {
            let identityKey = try identityPublicKey(identitySecret: secret)
            let ed25519Key = try identityEd25519PublicKey(identitySecret: secret)
            self.secret = secret
            self.identityKey = identityKey
            self.publicKey = ed25519Key
            self.publicKeyHex = ed25519Key.hexString
            return true
        } catch {
            print("[Identity] Rejected identity secret: \(error)")
            return false
        }
    }

    private static func make(id: UUID, secret: Data) -> UserIdentity? {
        let identity = UserIdentity()
        identity.id = id
        return identity.adopt(secret) ? identity : nil
    }

    /// An identity from a 32-byte Ed25519 secret key saved before the hybrid
    /// identity, upgraded with a fresh ML-DSA-65 seed. The Ed25519 public
    /// key, and so `publicKeyHex`, stays the same.
    private static func upgraded(id: UUID, ed25519Secret: Data) -> UserIdentity? {
        do {
            return make(id: id, secret: try upgradeEd25519Identity(ed25519Secret: ed25519Secret))
        } catch {
            print("[Identity] Could not upgrade Ed25519 key: \(error)")
            return nil
        }
    }

    // MARK: - Login and MLS

    /// The complete, signed login frame (`[0x03][u32 LE length][AuthRequest]`)
    /// answering the server's `challenge`.
    func authRequestFrame(challenge: Data, serverPassword: String?) throws -> Data {
        guard let secret else { throw IdentityError.InvalidSecret(message: "no identity loaded") }
        return try buildAuthRequest(
            identitySecret: secret,
            displayName: displayName,
            challenge: challenge,
            serverPassword: serverPassword
        )
    }

    /// The MLS client for this identity. `userId` is the account's `U_…` id
    /// from the login response.
    func makeMlsWrapper(userId: String) throws -> MlsWrapper {
        guard let secret else { throw IdentityError.InvalidSecret(message: "no identity loaded") }
        return try MlsWrapper(userId: userId, identitySecret: secret)
    }

    // MARK: - Profile Keychain Storage

    /// Save the identity secret to the keychain, encrypted to this profile's
    /// Secure Enclave key when `requiresBiometric` is set.
    ///
    /// On success the profile is left with exactly this one item: the other
    /// form (so turning on biometric protection leaves no plain copy) and any
    /// pre-hybrid item are deleted.
    @discardableResult
    public func saveToKeychain(requiresBiometric: Bool = false, keychain: SecretKeychain = SystemKeychain()) -> Bool {
        guard let id = id, let secret = secret else {
            print("[Identity] Cannot save to keychain: missing id or key")
            return false
        }

        let service = IdentityKeychainItem.service(forProfile: id)
        let data: Data
        let account: String
        if requiresBiometric {
            guard let wrapped = UserIdentity.wrapKeyWithBiometric(keyData: secret, profileId: id) else {
                print("[Identity] Failed to wrap key with biometric protection")
                return false
            }
            data = wrapped
            account = IdentityKeychainItem.wrappedSecretAccount
        } else {
            data = secret
            account = IdentityKeychainItem.secretAccount
        }

        let status = keychain.write(data, service: service, account: account)
        guard status == errSecSuccess else {
            print("[Identity] Failed to save key to keychain: \(status)")
            return false
        }

        for stale in IdentityKeychainItem.profileAccounts where stale != account {
            _ = keychain.delete(service: service, account: stale)
        }
        print("[Identity] Saved \(requiresBiometric ? "biometric-protected " : "")key to keychain for profile \(id)")
        return true
    }

    /// Load a profile's identity from the keychain.
    ///
    /// A profile saved before the hybrid identity holds only an Ed25519 key.
    /// It is upgraded (same Ed25519 public key, new ML-DSA-65 key), saved in
    /// the new form, and the old item deleted. If the upgrade cannot be
    /// saved, this returns nil and keeps the old item: an upgraded identity
    /// that is not stored would present a new ML-DSA key on every load.
    public static func loadFromKeychain(
        id: UUID,
        requiresBiometric: Bool = false,
        keychain: SecretKeychain = SystemKeychain()
    ) -> UserIdentity? {
        let service = IdentityKeychainItem.service(forProfile: id)

        if requiresBiometric {
            if let wrapped = keychain.read(service: service, account: IdentityKeychainItem.wrappedSecretAccount).data {
                guard let secret = unwrapKeyWithBiometric(wrappedData: wrapped, profileId: id) else {
                    print("[Identity] Failed to unwrap key (biometric auth failed or cancelled)")
                    return nil
                }
                print("[Identity] Loaded biometric-protected key from keychain for profile \(id)")
                return make(id: id, secret: secret)
            }

            if let legacyWrapped = keychain.read(service: service, account: IdentityKeychainItem.legacyWrappedKeyAccount).data {
                guard let edSecret = unwrapLegacyKeyWithBiometric(wrappedData: legacyWrapped) else {
                    print("[Identity] Failed to unwrap legacy key (biometric auth failed or cancelled)")
                    return nil
                }
                guard let identity = upgraded(id: id, ed25519Secret: edSecret),
                      identity.saveToKeychain(requiresBiometric: true, keychain: keychain) else {
                    print("[Identity] Could not migrate biometric-protected Ed25519 key for profile \(id)")
                    return nil
                }
                print("[Identity] Upgraded biometric-protected Ed25519 key for profile \(id)")
                return identity
            }
        }

        let (plain, status) = keychain.read(service: service, account: IdentityKeychainItem.secretAccount)
        if let plain {
            print("[Identity] Loaded key from keychain for profile \(id)")
            return make(id: id, secret: plain)
        }

        if let legacy = keychain.read(service: service, account: IdentityKeychainItem.legacyKeyAccount).data {
            guard let identity = upgraded(id: id, ed25519Secret: legacy),
                  identity.saveToKeychain(requiresBiometric: false, keychain: keychain) else {
                print("[Identity] Could not migrate Ed25519 key for profile \(id)")
                return nil
            }
            print("[Identity] Upgraded Ed25519 key for profile \(id)")
            return identity
        }

        print("[Identity] Failed to load key from keychain: \(status)")
        return nil
    }

    /// Delete every keychain item for a profile: the identity secret in each
    /// form, any pre-hybrid item, and the profile's Secure Enclave key.
    public static func deleteFromKeychain(id: UUID, keychain: SecretKeychain = SystemKeychain()) {
        let service = IdentityKeychainItem.service(forProfile: id)
        var deleted = 0
        for account in IdentityKeychainItem.profileAccounts {
            let status = keychain.delete(service: service, account: account)
            if status == errSecSuccess {
                deleted += 1
            } else if status != errSecItemNotFound {
                print("[Identity] Failed to delete \(account) from keychain: \(status)")
            }
        }
        deleteSecureEnclaveKey(profileId: id)
        print("[Identity] Deleted \(deleted) keychain item(s) for profile \(id)")
    }

    // MARK: - Backup

    /// The identity secret, for `IdentityBackup.encrypt` and nothing else:
    /// it must never be written anywhere unencrypted.
    func secretForBackup() -> Data? {
        secret
    }

    /// An identity restored from a backup, under a fresh profile id so that
    /// restoring can never take over an existing profile's keychain item.
    static func restored(secret: Data, displayName: String) -> UserIdentity? {
        guard let identity = make(id: UUID(), secret: secret) else { return nil }
        identity.displayName = displayName
        return identity
    }

    // MARK: - Import/Export

    /// Export profile as JSON bundle for cross-platform transfer.
    ///
    /// Version 2: `identitySecret` is the base64 64-byte identity secret;
    /// `publicKey` is the Ed25519 public key hex, for display only.
    public func exportProfile() -> Data? {
        guard let secret = secret else {
            print("[Identity] Cannot export: no identity")
            return nil
        }

        let bundle: [String: Any] = [
            "version": 2,
            "id": id?.uuidString ?? UUID().uuidString,
            "displayName": displayName,
            "publicKey": publicKeyHex,
            "identitySecret": secret.base64EncodedString()
        ]

        do {
            let data = try JSONSerialization.data(withJSONObject: bundle, options: .prettyPrinted)
            print("[Identity] Exported profile bundle")
            return data
        } catch {
            print("[Identity] Failed to export profile: \(error)")
            return nil
        }
    }

    /// Import a profile from a JSON bundle: version 2, or version 1 (a base64
    /// 32-byte Ed25519 `privateKey`), which is upgraded on import with its
    /// Ed25519 public key unchanged. Store the result: importing the same
    /// version 1 bundle again gives a different ML-DSA-65 key.
    public static func importProfile(from data: Data) -> UserIdentity? {
        guard let bundle = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let version = bundle["version"] as? Int,
              let idString = bundle["id"] as? String,
              let id = UUID(uuidString: idString),
              let displayName = bundle["displayName"] as? String else {
            print("[Identity] Invalid profile bundle format")
            return nil
        }

        func base64Field(_ name: String) -> Data? {
            (bundle[name] as? String).flatMap { Data(base64Encoded: $0) }
        }

        let identity: UserIdentity?
        switch version {
        case 2:
            identity = base64Field("identitySecret").flatMap { make(id: id, secret: $0) }
        case 1:
            identity = base64Field("privateKey").flatMap { upgraded(id: id, ed25519Secret: $0) }
        default:
            print("[Identity] Unsupported profile bundle version \(version)")
            return nil
        }

        guard let identity else {
            print("[Identity] Profile bundle has no usable key")
            return nil
        }
        identity.displayName = displayName
        print("[Identity] Imported profile: \(displayName)")
        return identity
    }
}

// MARK: - Data Extensions

extension Data {
    var hexString: String {
        map { String(format: "%02x", $0) }.joined()
    }

    init?(hexString: String) {
        var data = Data()
        var hex = hexString
        while hex.count >= 2 {
            let byte = String(hex.prefix(2))
            hex = String(hex.dropFirst(2))
            if let b = UInt8(byte, radix: 16) {
                data.append(b)
            } else {
                return nil
            }
        }
        self = data
    }
}
