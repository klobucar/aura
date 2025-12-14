using NSec.Cryptography;
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace Aura.Desktop.Services;

/// <summary>
/// Ed25519 identity management for TOFU (Trust On First Use) authentication.
/// Generates and stores keypair in user's config directory.
/// </summary>
public class UserIdentity : IDisposable
{
    private readonly Key _signingKey;
    
    /// <summary>32-byte Ed25519 public key.</summary>
    public byte[] PublicKey { get; }
    
    /// <summary>Display name (claimed on first connect).</summary>
    public string DisplayName { get; set; }
    
    /// <summary>Hex-encoded public key for display/logging.</summary>
    public string PublicKeyHex => Convert.ToHexString(PublicKey).ToLowerInvariant();
    
    private UserIdentity(Key signingKey, string displayName)
    {
        _signingKey = signingKey;
        PublicKey = signingKey.PublicKey.Export(KeyBlobFormat.RawPublicKey);
        DisplayName = displayName;
    }
    
    /// <summary>
    /// Sign a challenge message using Ed25519.
    /// </summary>
    public byte[] Sign(byte[] message)
    {
        var algorithm = SignatureAlgorithm.Ed25519;
        return algorithm.Sign(_signingKey, message);
    }
    
    /// <summary>
    /// Generate a new identity with random keypair.
    /// </summary>
    public static UserIdentity Generate(string displayName)
    {
        var algorithm = SignatureAlgorithm.Ed25519;
        var key = Key.Create(algorithm, new KeyCreationParameters
        {
            ExportPolicy = KeyExportPolicies.AllowPlaintextExport
        });
        return new UserIdentity(key, displayName);
    }
    
    /// <summary>
    /// Load identity from file, or generate new one if not found.
    /// </summary>
    public static UserIdentity LoadOrCreate(string displayName)
    {
        var path = GetIdentityFilePath();
        
        if (File.Exists(path))
        {
            return Load(path);
        }
        
        // Generate new identity
        var identity = Generate(displayName);
        identity.Save(path);
        return identity;
    }
    
    /// <summary>
    /// Load identity from file.
    /// </summary>
    public static UserIdentity Load(string path)
    {
        var json = File.ReadAllText(path);
        var data = JsonSerializer.Deserialize<IdentityData>(json)
            ?? throw new InvalidOperationException("Invalid identity file");
        
        var privateKeyBytes = Convert.FromHexString(data.PrivateKeyHex);
        var algorithm = SignatureAlgorithm.Ed25519;
        var key = Key.Import(algorithm, privateKeyBytes, KeyBlobFormat.RawPrivateKey,
            new KeyCreationParameters { ExportPolicy = KeyExportPolicies.AllowPlaintextExport });
        
        return new UserIdentity(key, data.DisplayName);
    }
    
    /// <summary>
    /// Save identity to file with secure permissions.
    /// </summary>
    public void Save(string path)
    {
        var dir = Path.GetDirectoryName(path);
        if (!string.IsNullOrEmpty(dir) && !Directory.Exists(dir))
        {
            Directory.CreateDirectory(dir);
        }
        
        var privateKeyBytes = _signingKey.Export(KeyBlobFormat.RawPrivateKey);
        var data = new IdentityData
        {
            DisplayName = DisplayName,
            PrivateKeyHex = Convert.ToHexString(privateKeyBytes).ToLowerInvariant(),
            PublicKeyHex = PublicKeyHex,
            CreatedAt = DateTime.UtcNow
        };
        
        var json = JsonSerializer.Serialize(data, new JsonSerializerOptions { WriteIndented = true });
        File.WriteAllText(path, json);
        
        // Set file permissions on Unix (chmod 600)
        if (!RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
        {
            try
            {
                File.SetUnixFileMode(path, UnixFileMode.UserRead | UnixFileMode.UserWrite);
            }
            catch
            {
                // Ignore permission errors on non-Unix systems
            }
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
        _signingKey.Dispose();
    }
    
    private class IdentityData
    {
        public string DisplayName { get; set; } = "";
        public string PrivateKeyHex { get; set; } = "";
        public string PublicKeyHex { get; set; } = "";
        public DateTime CreatedAt { get; set; }
    }
}
