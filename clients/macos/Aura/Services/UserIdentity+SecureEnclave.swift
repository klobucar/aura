import Foundation

/// Secure Enclave helper methods for biometric key wrapping
extension UserIdentity {
    
    // MARK: - Secure Enclave Key Wrapping
    
    /// Generate or retrieve Secure Enclave P-256 key for wrapping
    static func getOrCreateSecureEnclaveKey(profileId: UUID) -> SecKey? {
        let tag = "com.aura.enclave.\\(profileId.uuidString)".data(using: .utf8)!
        
        // Try to retrieve existing key
        let query: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrApplicationTag as String: tag,
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecReturnRef as String: true
        ]
        
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        
        if status == errSecSuccess {
            return (item as! SecKey)
        }
        
        // Create new Secure Enclave key
        guard let access = SecAccessControlCreateWithFlags(
            kCFAllocatorDefault,
            kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            [.privateKeyUsage, .biometryCurrentSet],
            nil
        ) else {
            print("[Identity] Failed to create access control")
            return nil
        }
        
        let attributes: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256,
            kSecAttrTokenID as String: kSecAttrTokenIDSecureEnclave,
            kSecPrivateKeyAttrs as String: [
                kSecAttrIsPermanent as String: true,
                kSecAttrApplicationTag as String: tag,
                kSecAttrAccessControl as String: access
            ]
        ]
        
        var error: Unmanaged<CFError>?
        guard let privateKey = SecKeyCreateRandomKey(attributes as CFDictionary, &error) else {
            if let error = error {
                print("[Identity] Failed to create Secure Enclave key: \\(error.takeRetainedValue())")
            }
            return nil
        }
        
        return privateKey
    }
    
    /// Wrap Ed25519 key with Secure Enclave key (requires biometric)
    static func wrapKeyWithBiometric(keyData: Data, profileId: UUID) -> Data? {
        guard let enclaveKey = getOrCreateSecureEnclaveKey(profileId: profileId) else {
            return nil
        }
        
        // Get public key for encryption
        guard let publicKey = SecKeyCopyPublicKey(enclaveKey) else {
            print("[Identity] Failed to get public key from Secure Enclave key")
            return nil
        }
        
        // Use ECIES encryption
        let algorithm = SecKeyAlgorithm.eciesEncryptionCofactorVariableIVX963SHA256AESGCM
        
        guard SecKeyIsAlgorithmSupported(publicKey, .encrypt, algorithm) else {
            print("[Identity] Algorithm not supported")
            return nil
        }
        
        var error: Unmanaged<CFError>?
        guard let encryptedData = SecKeyCreateEncryptedData(
            publicKey,
            algorithm,
            keyData as CFData,
            &error
        ) as Data? else {
            if let error = error {
                print("[Identity] Failed to encrypt key: \\(error.takeRetainedValue())")
            }
            return nil
        }
        
        return encryptedData
    }
    
    /// Unwrap Ed25519 key with Secure Enclave key (requires biometric)
    static func unwrapKeyWithBiometric(wrappedData: Data, profileId: UUID) -> Data? {
        guard let enclaveKey = getOrCreateSecureEnclaveKey(profileId: profileId) else {
            return nil
        }
        
        // Decrypt with private key (triggers biometric prompt)
        let algorithm = SecKeyAlgorithm.eciesEncryptionCofactorVariableIVX963SHA256AESGCM
        
        guard SecKeyIsAlgorithmSupported(enclaveKey, .decrypt, algorithm) else {
            print("[Identity] Algorithm not supported")
            return nil
        }
        
        var error: Unmanaged<CFError>?
        guard let decryptedData = SecKeyCreateDecryptedData(
            enclaveKey,
            algorithm,
            wrappedData as CFData,
            &error
        ) as Data? else {
            if let error = error {
                print("[Identity] Failed to decrypt key (biometric auth may have been cancelled): \\(error.takeRetainedValue())")
            }
            return nil
        }
        
        return decryptedData
    }
}
