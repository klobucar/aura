import Foundation
import Combine
import CryptoKit
import Security

/// Ed25519 identity management for TOFU authentication using CryptoKit.
/// Keys are stored securely in the macOS Keychain.
@MainActor
public class UserIdentity: ObservableObject {
    
    @Published public var displayName: String = ""
    @Published public var publicKeyHex: String = ""
    
    public var id: UUID? // Profile ID for keychain storage
    
    private var signingKey: Curve25519.Signing.PrivateKey?
    private static var sessionKey: Curve25519.Signing.PrivateKey?

    public var publicKey: Data? {
        signingKey?.publicKey.rawRepresentation
    }
    
    public init() {}
    
    // MARK: - Key Generation & Loading
    
    /// Generate a new Ed25519 keypair.
    public func generateKeypair() {
        signingKey = Curve25519.Signing.PrivateKey()
        updatePublicKeyHex()
        print("[Identity] Generated new Ed25519 keypair")
        print("[Identity] Public key: \(publicKeyHex)")
    }
    
    /// Load existing key for this session or generate a new one.
    public func loadOrGenerate() {
        // Keep display name stable in UserDefaults
        if let savedName = UserDefaults.standard.string(forKey: "AuraDisplayName"), !savedName.isEmpty {
            displayName = savedName
        } else if displayName.isEmpty {
            displayName = "User\(Int.random(in: 1000...9999))"
            UserDefaults.standard.set(displayName, forKey: "AuraDisplayName")
        }
        
        if let existing = UserIdentity.sessionKey {
            self.signingKey = existing
            updatePublicKeyHex()
            print("[Identity] Reusing existing session key: \(publicKeyHex)")
        } else {
            generateKeypair()
            UserIdentity.sessionKey = self.signingKey
        }
    }
    
    /// Save display name to UserDefaults.
    public func saveDisplayName(_ name: String) {
        displayName = name
        UserDefaults.standard.set(name, forKey: "AuraDisplayName")
    }
    
    // MARK: - Signing
    
    /// Sign a challenge message using Ed25519.
    public func sign(_ data: Data) -> Data? {
        guard let key = signingKey else {
            print("[Identity] Error: No signing key available")
            return nil
        }
        
        do {
            let signature = try key.signature(for: data)
            print("[Identity] Signed \(data.count) bytes, signature: \(signature.prefix(8).hexString)...")
            return signature
        } catch {
            print("[Identity] Signing failed: \(error)")
            return nil
        }
    }
    
    private func updatePublicKeyHex() {
        if let pk = publicKey {
            publicKeyHex = pk.hexString
        }
    }
    
    // MARK: - Keychain Storage
    
    /// Save private key to keychain (optionally with biometric protection)
    public func saveToKeychain(requiresBiometric: Bool = false) {
        guard let id = id, let key = signingKey else {
            print("[Identity] Cannot save to keychain: missing id or key")
            return
        }
        
        let keyData = key.rawRepresentation
        let service = "com.aura.identity.\(id.uuidString)"
        
        if requiresBiometric {
            // Wrap key with Secure Enclave protection
            guard let wrappedData = UserIdentity.wrapKeyWithBiometric(keyData: keyData, profileId: id) else {
                print("[Identity] Failed to wrap key with biometric protection")
                return
            }
            
            // Store wrapped key
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: service,
                kSecAttrAccount as String: "ed25519-wrapped-key",
                kSecValueData as String: wrappedData
            ]
            
            SecItemDelete(query as CFDictionary)
            let status = SecItemAdd(query as CFDictionary, nil)
            if status == errSecSuccess {
                print("[Identity] Saved biometric-protected key to keychain for profile \(id)")
            } else {
                print("[Identity] Failed to save biometric-protected key: \(status)")
            }
        } else {
            // Store key directly (no biometric protection)
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: service,
                kSecAttrAccount as String: "ed25519-private-key",
                kSecValueData as String: keyData
            ]
            
            SecItemDelete(query as CFDictionary)
            let status = SecItemAdd(query as CFDictionary, nil)
            if status == errSecSuccess {
                print("[Identity] Saved key to keychain for profile \(id)")
            } else {
                print("[Identity] Failed to save key to keychain: \(status)")
            }
        }
    }
    
    /// Load private key from keychain (handles both biometric and non-biometric)
    public static func loadFromKeychain(id: UUID, requiresBiometric: Bool = false) -> UserIdentity? {
        let service = "com.aura.identity.\(id.uuidString)"
        
        // Try biometric-protected key first if required
        if requiresBiometric {
            let wrappedQuery: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: service,
                kSecAttrAccount as String: "ed25519-wrapped-key",
                kSecReturnData as String: true
            ]
            
            var wrappedResult: AnyObject?
            let wrappedStatus = SecItemCopyMatching(wrappedQuery as CFDictionary, &wrappedResult)
            
            if wrappedStatus == errSecSuccess, let wrappedData = wrappedResult as? Data {
                // Unwrap with biometric authentication
                guard let keyData = UserIdentity.unwrapKeyWithBiometric(wrappedData: wrappedData, profileId: id) else {
                    print("[Identity] Failed to unwrap key (biometric auth failed or cancelled)")
                    return nil
                }
                
                do {
                    let identity = UserIdentity()
                    identity.id = id
                    identity.signingKey = try Curve25519.Signing.PrivateKey(rawRepresentation: keyData)
                    identity.updatePublicKeyHex()
                    print("[Identity] Loaded biometric-protected key from keychain for profile \(id)")
                    return identity
                } catch {
                    print("[Identity] Failed to create key from unwrapped data: \(error)")
                    return nil
                }
            }
        }
        
        // Fall back to non-biometric key
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: "ed25519-private-key",
            kSecReturnData as String: true
        ]
        
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        
        guard status == errSecSuccess, let keyData = result as? Data else {
            print("[Identity] Failed to load key from keychain: \(status)")
            return nil
        }
        
        do {
            let identity = UserIdentity()
            identity.id = id
            identity.signingKey = try Curve25519.Signing.PrivateKey(rawRepresentation: keyData)
            identity.updatePublicKeyHex()
            print("[Identity] Loaded key from keychain for profile \(id)")
            return identity
        } catch {
            print("[Identity] Failed to create key from keychain data: \(error)")
            return nil
        }
    }
    
    /// Delete private key from keychain
    public static func deleteFromKeychain(id: UUID) {
        let service = "com.aura.identity.\(id.uuidString)"
        
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: "ed25519-private-key"
        ]
        
        let status = SecItemDelete(query as CFDictionary)
        if status == errSecSuccess {
            print("[Identity] Deleted key from keychain for profile \(id)")
        } else {
            print("[Identity] Failed to delete key from keychain: \(status)")
        }
    }
    
    // MARK: - Import/Export
    
    /// Export profile as JSON bundle for cross-platform transfer
    public func exportProfile() -> Data? {
        guard let key = signingKey else {
            print("[Identity] Cannot export: no signing key")
            return nil
        }
        
        let bundle: [String: Any] = [
            "version": 1,
            "id": id?.uuidString ?? UUID().uuidString,
            "displayName": displayName,
            "publicKey": publicKeyHex,
            "privateKey": key.rawRepresentation.base64EncodedString()
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
    
    /// Import profile from JSON bundle
    public static func importProfile(from data: Data) -> UserIdentity? {
        do {
            guard let bundle = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let version = bundle["version"] as? Int,
                  version == 1,
                  let idString = bundle["id"] as? String,
                  let id = UUID(uuidString: idString),
                  let displayName = bundle["displayName"] as? String,
                  let privateKeyBase64 = bundle["privateKey"] as? String,
                  let privateKeyData = Data(base64Encoded: privateKeyBase64) else {
                print("[Identity] Invalid profile bundle format")
                return nil
            }
            
            let identity = UserIdentity()
            identity.id = id
            identity.displayName = displayName
            identity.signingKey = try Curve25519.Signing.PrivateKey(rawRepresentation: privateKeyData)
            identity.updatePublicKeyHex()
            
            print("[Identity] Imported profile: \(displayName)")
            return identity
        } catch {
            print("[Identity] Failed to import profile: \(error)")
            return nil
        }
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
