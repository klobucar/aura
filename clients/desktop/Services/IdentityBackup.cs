using System;
using System.IO;
using uniffi.aura_core;

namespace Aura.Desktop.Services;

/// <summary>An identity recovered from a backup, not yet installed.</summary>
public sealed record RestoredIdentity(byte[] Secret, string DisplayName, string ShortFingerprint);

/// <summary>
/// Encrypted identity backups (<c>.aura-identity</c>), in the format aura-core
/// defines for every platform. Both directions run scrypt at N = 2^17 —
/// seconds of CPU — so callers must stay off the UI thread.
/// </summary>
public static class IdentityBackup
{
    public const string FileExtension = "aura-identity";

    public static byte[] Encrypt(UserIdentity identity, string passphrase)
    {
        var secret = identity.ExportSecretForBackup();
        try
        {
            return AuraCoreMethods.EncryptIdentityBackup(secret, identity.DisplayName, passphrase);
        }
        finally
        {
            Array.Clear(secret);
        }
    }

    public static RestoredIdentity Decrypt(byte[] backup, string passphrase)
    {
        var contents = AuraCoreMethods.DecryptIdentityBackup(backup, passphrase);
        return new RestoredIdentity(
            contents.IdentitySecret,
            contents.DisplayName,
            ShortFingerprint(AuraCoreMethods.IdentityPublicKey(contents.IdentitySecret)));
    }

    /// <summary>`3f8a…5503` for a hybrid public key.</summary>
    public static string ShortFingerprint(byte[] publicKey)
    {
        try { return AuraCoreMethods.IdentityFingerprintShort(publicKey); }
        catch { return "?"; }
    }

    /// <summary>Readable text for each failure.</summary>
    public static string ErrorMessage(Exception ex) => ex switch
    {
        IdentityBackupException.NotABackup => "That file isn't an Aura identity backup.",
        IdentityBackupException.UnsupportedVersion => "This backup was made by a newer version of Aura. Update, then try again.",
        IdentityBackupException.WrongPassphraseOrDamaged => "Wrong passphrase, or the file is damaged.",
        IdentityBackupException.UnreasonableParameters => "This backup asks for unreasonable decryption settings, so it wasn't opened.",
        IdentityBackupException.EmptyPassphrase => "Enter the backup's passphrase.",
        IdentityBackupException.InvalidContents => "The backup decrypted but doesn't hold a valid identity.",
        IOException io => $"Couldn't read or write the file: {io.Message}",
        _ => $"Something went wrong: {ex.Message}",
    };
}
