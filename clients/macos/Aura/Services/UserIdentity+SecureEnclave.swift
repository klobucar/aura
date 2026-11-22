import Foundation

/// Secure Enclave helper methods for biometric key wrapping
extension UserIdentity {

    // MARK: - Secure Enclave Key Wrapping

    /// Application tag of a profile's Secure Enclave key. One key per profile.
    static func secureEnclaveKeyTag(profileId: UUID) -> Data {
        Data("com.aura.enclave.\(profileId.uuidString)".utf8)
    }

    /// The tag every profile's key was created under before the tag was made
    /// per-profile: the literal text `com.aura.enclave.\(profileId.uuidString)`
    /// (an escaped interpolation). Pre-hybrid wrapped keys were encrypted to
    /// this shared key, so it is still needed to read them during migration.
    static let legacySharedSecureEnclaveKeyTag = Data(#"com.aura.enclave.\(profileId.uuidString)"#.utf8)

    /// The existing Secure Enclave key with `tag`, if any.
    private static func secureEnclaveKey(tag: Data) -> SecKey? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrApplicationTag as String: tag,
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecReturnRef as String: true
        ]

        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let item, CFGetTypeID(item) == SecKeyGetTypeID() else {
            return nil
        }
        return (item as! SecKey)
    }

    /// Generate or retrieve Secure Enclave P-256 key for wrapping
    static func getOrCreateSecureEnclaveKey(profileId: UUID) -> SecKey? {
        let tag = secureEnclaveKeyTag(profileId: profileId)

        if let existing = secureEnclaveKey(tag: tag) {
            return existing
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
                print("[Identity] Failed to create Secure Enclave key: \(error.takeRetainedValue())")
            }
            return nil
        }

        return privateKey
    }

    /// Delete a profile's Secure Enclave key, if it has one.
    static func deleteSecureEnclaveKey(profileId: UUID) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrApplicationTag as String: secureEnclaveKeyTag(profileId: profileId),
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom
        ]
        SecItemDelete(query as CFDictionary)
    }

    /// Wrap the identity secret with the profile's Secure Enclave key
    /// (unwrapping it later requires biometric authentication).
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
                print("[Identity] Failed to encrypt key: \(error.takeRetainedValue())")
            }
            return nil
        }

        return encryptedData
    }

    /// Unwrap the identity secret with the profile's Secure Enclave key
    /// (requires biometric)
    static func unwrapKeyWithBiometric(wrappedData: Data, profileId: UUID) -> Data? {
        // Only an existing key can decrypt; creating one here would be useless.
        guard let enclaveKey = secureEnclaveKey(tag: secureEnclaveKeyTag(profileId: profileId)) else {
            print("[Identity] No Secure Enclave key for profile \(profileId)")
            return nil
        }
        return decrypt(wrappedData, with: enclaveKey)
    }

    /// Unwrap a pre-hybrid Ed25519 key, which was wrapped with the shared
    /// legacy Secure Enclave key (requires biometric).
    static func unwrapLegacyKeyWithBiometric(wrappedData: Data) -> Data? {
        guard let enclaveKey = secureEnclaveKey(tag: legacySharedSecureEnclaveKeyTag) else {
            print("[Identity] No legacy Secure Enclave key")
            return nil
        }
        return decrypt(wrappedData, with: enclaveKey)
    }

    private static func decrypt(_ wrappedData: Data, with enclaveKey: SecKey) -> Data? {
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
                print("[Identity] Failed to decrypt key (biometric auth may have been cancelled): \(error.takeRetainedValue())")
            }
            return nil
        }

        return decryptedData
    }
}
