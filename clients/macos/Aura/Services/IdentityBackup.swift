import AuraCore
import Foundation
import UniformTypeIdentifiers

/// Passphrase-encrypted identity backups (`.aura-identity`), made and opened
/// by aura-core so a backup from this Mac restores on the desktop client too.
nonisolated enum IdentityBackup {
    static let fileExtension = "aura-identity"
    static let minimumPassphraseLength = 8

    /// UserDefaults key: when the login identity was last backed up, as
    /// seconds since 1970. Absent until the first backup.
    static let backedUpAtKey = "AuraIdentityBackedUpAt"

    static var contentType: UTType {
        UTType(filenameExtension: fileExtension, conformingTo: .data) ?? .data
    }

    /// `Alice.aura-identity`, with characters a file name can't hold removed.
    static func suggestedFileName(displayName: String) -> String {
        let cleaned = displayName
            .components(separatedBy: CharacterSet(charactersIn: "/:\\"))
            .joined()
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return "\(cleaned.isEmpty ? "Aura identity" : cleaned).\(fileExtension)"
    }

    /// Why a new backup passphrase can't be used yet, or nil when it can.
    static func passphraseProblem(_ passphrase: String, repeated: String) -> String? {
        if passphrase.count < minimumPassphraseLength {
            return "Use at least \(minimumPassphraseLength) characters."
        }
        if passphrase != repeated {
            return "The passphrases don't match."
        }
        return nil
    }

    /// True when `data` starts like a backup file (the format's magic).
    static func looksLikeBackup(_ data: Data) -> Bool {
        data.starts(with: Array("AURAIDB".utf8))
    }

    // MARK: Encryption (off the main actor)

    /// Encrypt on a background thread: the key derivation (scrypt, N = 2^17)
    /// takes long enough to stall the UI.
    static func encrypt(secret: Data, displayName: String, passphrase: String) async throws -> Data {
        try await Task.detached(priority: .userInitiated) {
            try encryptIdentityBackup(identitySecret: secret, displayName: displayName, passphrase: passphrase)
        }.value
    }

    /// Decrypt on a background thread; see `encrypt`.
    static func decrypt(_ backup: Data, passphrase: String) async throws -> (secret: Data, displayName: String) {
        let contents = try await Task.detached(priority: .userInitiated) {
            try decryptIdentityBackup(backup: backup, passphrase: passphrase)
        }.value
        return (contents.identitySecret, contents.displayName)
    }

    /// What to tell the user when a backup can't be made or opened.
    static func message(for error: Error) -> String {
        guard let backupError = error as? IdentityBackupError else {
            return error.localizedDescription
        }
        switch backupError {
        case .NotABackup:
            return "This file isn't an Aura identity backup."
        case .UnsupportedVersion:
            return "This backup was made by a newer version of Aura. Update Aura to open it."
        case .WrongPassphraseOrDamaged:
            return "Wrong passphrase, or the file is damaged."
        case .UnreasonableParameters:
            return "This backup asks for unsafe decryption settings, so it wasn't opened."
        case .EmptyPassphrase:
            return "Enter the passphrase."
        case .InvalidContents:
            return "This backup doesn't hold a usable identity."
        }
    }
}

