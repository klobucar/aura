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
