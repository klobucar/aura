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
    
    private var signingKey: Curve25519.Signing.PrivateKey?
    
    public var publicKey: Data? {
        signingKey?.publicKey.rawRepresentation
    }
    
    private static let keychainService = "com.aura.identity"
    private static let keychainAccount = "ed25519-private-key"
    
    public init() {}
    
    // MARK: - Key Generation & Loading
    
    /// Generate a new Ed25519 keypair.
    public func generateKeypair() {
        signingKey = Curve25519.Signing.PrivateKey()
        updatePublicKeyHex()
        print("[Identity] Generated new Ed25519 keypair")
        print("[Identity] Public key: \(publicKeyHex)")
    }
    
    /// Load keypair from Keychain, or generate new one if not found.
    public func loadOrGenerate() {
        if loadFromKeychain() {
            print("[Identity] Loaded existing keypair from Keychain")
        } else {
            generateKeypair()
            saveToKeychain()
            print("[Identity] Saved new keypair to Keychain")
        }
        
        // Load display name from UserDefaults
        displayName = UserDefaults.standard.string(forKey: "AuraDisplayName") ?? ""
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
    
    // MARK: - Keychain Operations
    
    private func saveToKeychain() {
        guard let key = signingKey else { return }
        
        let privateKeyData = key.rawRepresentation
        
        // Delete existing key first
        let deleteQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.keychainService,
            kSecAttrAccount as String: Self.keychainAccount
        ]
        SecItemDelete(deleteQuery as CFDictionary)
        
        // Add new key
        let addQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.keychainService,
            kSecAttrAccount as String: Self.keychainAccount,
            kSecValueData as String: privateKeyData,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        ]
        
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        if status != errSecSuccess {
            print("[Identity] Keychain save failed: \(status)")
        }
    }
    
    private func loadFromKeychain() -> Bool {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.keychainService,
            kSecAttrAccount as String: Self.keychainAccount,
            kSecReturnData as String: true
        ]
        
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        
        guard status == errSecSuccess,
              let data = result as? Data else {
            return false
        }
        
        do {
            signingKey = try Curve25519.Signing.PrivateKey(rawRepresentation: data)
            updatePublicKeyHex()
            return true
        } catch {
            print("[Identity] Failed to load key from Keychain: \(error)")
            return false
        }
    }
    
    private func updatePublicKeyHex() {
        if let pk = publicKey {
            publicKeyHex = pk.hexString
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
