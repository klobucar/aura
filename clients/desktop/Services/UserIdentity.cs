using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text.Json;
using System.Text.Json.Serialization;
using uniffi.aura_core;

namespace Aura.Desktop.Services;

/// <summary>
/// The user's long-term hybrid identity: an Ed25519 key and an ML-DSA-65 key
/// held as one 64-byte secret (Ed25519 secret key, then ML-DSA-65 seed).
///
/// All signing happens in aura-core. This class keeps the secret, stores it in
/// <c>identity.json</c>, and hands it to the FFI calls that need it, so the
/// signature format and login transcript are defined once in Rust.
///
/// Version 1 files held only an Ed25519 key. <see cref="Load"/> upgrades them
/// in place and keeps that key, so the account it is pinned to (and any
/// <c>AURA_BOOTSTRAP_ADMIN_KEY</c> naming it) carries over.
/// </summary>
public sealed class UserIdentity : IDisposable
{
    /// <summary>The <c>identity.json</c> format this client writes.</summary>
    public const int FileVersion = 2;

    private const int SecretLength = 64;
    private const int LegacyEd25519SecretLength = 32;

    private static readonly JsonSerializerOptions WriteOptions = new() { WriteIndented = true };

    private readonly byte[] _secret;
    private bool _disposed;

    /// <summary>The 1,984-byte hybrid public key that the server and MLS groups see.</summary>
    public byte[] IdentityPublicKey { get; }

    /// <summary>The 32-byte Ed25519 part of <see cref="IdentityPublicKey"/>.</summary>
    public byte[] Ed25519PublicKey { get; }

    /// <summary>
    /// <see cref="Ed25519PublicKey"/> as 64 lowercase hex characters, the form a
    /// server's <c>AURA_BOOTSTRAP_ADMIN_KEY</c> takes. The v1 → v2 upgrade does
    /// not change it.
    /// </summary>
    public string PublicKeyHex => Convert.ToHexStringLower(Ed25519PublicKey);

    /// <summary>Display name (claimed on first connect).</summary>
    public string DisplayName { get; set; }

    /// <summary>When the identity was first created. Kept across upgrades.</summary>
    public DateTime CreatedAt { get; }

    private UserIdentity(byte[] secret, string displayName, DateTime createdAt)
    {
        if (secret.Length != SecretLength)
        {
            throw new ArgumentException($"Identity secret must be {SecretLength} bytes, got {secret.Length}", nameof(secret));
        }

        _secret = secret;
        IdentityPublicKey = AuraCoreMethods.IdentityPublicKey(secret);
        Ed25519PublicKey = AuraCoreMethods.IdentityEd25519PublicKey(secret);
        DisplayName = displayName;
        CreatedAt = createdAt;
    }

    /// <summary>Generate a new identity with a fresh secret.</summary>
    public static UserIdentity Generate(string displayName) =>
        new(AuraCoreMethods.GenerateIdentitySecret(), displayName, DateTime.UtcNow);

    /// <summary>
    /// The complete login frame (<c>[0x03][u32 LE length][AuthRequest]</c>),
    /// signed over the server's 32-byte <paramref name="challenge"/>.
    /// </summary>
    internal byte[] BuildAuthRequest(byte[] challenge, string? serverPassword)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        return AuraCoreMethods.BuildAuthRequest(_secret, DisplayName, challenge, serverPassword);
    }

    /// <summary>
    /// The MLS client for this identity. <paramref name="userUuid"/> is the
    /// server-assigned account id (<c>U_…</c>) from the login response.
    /// </summary>
    internal MlsWrapper CreateMlsWrapper(string userUuid)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        return new MlsWrapper(userUuid, _secret);
    }

    /// <summary>
    /// Load the identity from <paramref name="path"/> (default:
    /// <see cref="GetIdentityFilePath"/>), or generate and save a new one if
    /// the file does not exist.
    /// </summary>
    public static UserIdentity LoadOrCreate(string displayName, string? path = null)
    {
        path ??= GetIdentityFilePath();

        if (File.Exists(path))
        {
            return Load(path);
        }

        var identity = Generate(displayName);
        identity.Save(path);
        return identity;
    }

    /// <summary>
    /// Load an identity file. A version 1 file is upgraded to version 2 and
    /// rewritten before this returns.
    /// </summary>
    public static UserIdentity Load(string path)
    {
        var file = JsonSerializer.Deserialize<IdentityFile>(File.ReadAllText(path))
            ?? throw new InvalidDataException($"{path}: empty identity file");

        // Version 1 files had no Version field, so it reads as 0.
        if (file.Version is 0 or 1 && file.PrivateKeyHex != null)
        {
            return UpgradeV1(path, file);
        }

        if (file.Version != FileVersion)
        {
            // Probably written by a newer client. Leave it untouched.
            throw new InvalidDataException(
                $"{path}: identity file version {file.Version} is not supported (expected {FileVersion})");
        }

        var secret = DecodeHex(file.IdentitySecretHex, SecretLength, nameof(IdentityFile.IdentitySecretHex), path);
        return new UserIdentity(secret, file.DisplayName, file.CreatedAt);
    }

    /// <summary>
    /// Keep the v1 Ed25519 key, add a fresh ML-DSA-65 seed, and save the
    /// result as v2. The save has to succeed: the server pins the ML-DSA key
    /// at the next login, so a seed that was never written down would lock
    /// the account out on the following launch.
    /// </summary>
    private static UserIdentity UpgradeV1(string path, IdentityFile file)
    {
        var ed25519Secret = DecodeHex(
            file.PrivateKeyHex, LegacyEd25519SecretLength, nameof(IdentityFile.PrivateKeyHex), path);

        byte[] secret;
        try
        {
            secret = AuraCoreMethods.UpgradeEd25519Identity(ed25519Secret);
        }
        finally
        {
            CryptographicOperations.ZeroMemory(ed25519Secret);
        }

        var identity = new UserIdentity(secret, file.DisplayName, file.CreatedAt);

        // v1 stored the public key next to the private key. If they disagree,
        // the file is damaged; stop before replacing it.
        if (!string.IsNullOrEmpty(file.PublicKeyHex)
            && !string.Equals(file.PublicKeyHex, identity.PublicKeyHex, StringComparison.OrdinalIgnoreCase))
        {
            identity.Dispose();
            throw new InvalidDataException(
                $"{path}: PrivateKeyHex does not match PublicKeyHex; not upgrading this identity file");
        }

        identity.Save(path);
        Console.WriteLine($"[UserIdentity] Upgraded {path} to version {FileVersion} (Ed25519 key {identity.PublicKeyHex[..16]}... kept)");
        return identity;
    }

    /// <summary>
    /// Save the identity as a version 2 file, readable only by the current user
    /// on Linux and macOS. The file is written beside the target and then
    /// renamed over it, so a crash mid-write leaves the previous file intact.
    /// </summary>
    public void Save(string path)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);

        var dir = Path.GetDirectoryName(path);
        if (!string.IsNullOrEmpty(dir))
        {
            Directory.CreateDirectory(dir);
        }

        var json = JsonSerializer.SerializeToUtf8Bytes(new IdentityFile
        {
            Version = FileVersion,
            DisplayName = DisplayName,
            IdentitySecretHex = Convert.ToHexStringLower(_secret),
            CreatedAt = CreatedAt,
        }, WriteOptions);

        var tmp = path + ".tmp";
        try
        {
            File.Delete(tmp);

            var options = new FileStreamOptions
            {
                Mode = FileMode.CreateNew,
                Access = FileAccess.Write,
                Share = FileShare.None,
            };
            if (!OperatingSystem.IsWindows())
            {
                // chmod 600 from the moment the file exists, not after the
                // secret is already in it.
                options.UnixCreateMode = UnixFileMode.UserRead | UnixFileMode.UserWrite;
            }

            using (var stream = new FileStream(tmp, options))
            {
                stream.Write(json);
                stream.Flush(flushToDisk: true);
            }

            File.Move(tmp, path, overwrite: true);
        }
        finally
        {
            CryptographicOperations.ZeroMemory(json);
            try { File.Delete(tmp); } catch (IOException) { }
        }
    }

    /// <summary>
    /// Get platform-specific identity file path.
    /// </summary>
    public static string GetIdentityFilePath()
    {
        if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
        {
            return Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "Aura", "identity.json"
            );
        }
        else if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
        {
            return Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
                "Library", "Application Support", "Aura", "identity.json"
            );
        }
        else
        {
            // Linux
            return Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
                ".config", "aura", "identity.json"
            );
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        CryptographicOperations.ZeroMemory(_secret);
    }

    private static byte[] DecodeHex(string? hex, int length, string field, string path)
    {
        if (hex == null || hex.Length != length * 2)
        {
            throw new InvalidDataException($"{path}: {field} must be {length * 2} hex characters");
        }

        try
        {
            return Convert.FromHexString(hex);
        }
        catch (FormatException)
        {
            throw new InvalidDataException($"{path}: {field} is not valid hex");
        }
    }

    /// <summary>
    /// On-disk form. Version 2 writes <c>Version</c>, <c>DisplayName</c>,
    /// <c>IdentitySecretHex</c> and <c>CreatedAt</c>; the last two properties
    /// exist only to read version 1 files.
    /// </summary>
    private sealed class IdentityFile
    {
        public int Version { get; set; }
        public string DisplayName { get; set; } = "";

        /// <summary>The 64-byte identity secret as 128 lowercase hex characters.</summary>
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? IdentitySecretHex { get; set; }

        public DateTime CreatedAt { get; set; }

        /// <summary>Version 1: the raw 32-byte Ed25519 secret key.</summary>
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? PrivateKeyHex { get; set; }

        /// <summary>Version 1: the Ed25519 public key.</summary>
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? PublicKeyHex { get; set; }
    }
}
