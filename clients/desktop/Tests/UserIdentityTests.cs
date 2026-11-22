using System;
using System.IO;
using System.Text.Json;
using Aura.Desktop.Services;
using Xunit;

namespace Aura.Desktop.Tests;

/// <summary>
/// identity.json storage and the v1 → v2 upgrade. These call into aura-core,
/// so they need the native library next to the test assembly.
///
/// The upgrade must keep the Ed25519 key: the account is pinned to it, and a
/// server's AURA_BOOTSTRAP_ADMIN_KEY names it as <see cref="UserIdentity.PublicKeyHex"/>.
/// </summary>
public sealed class UserIdentityTests : IDisposable
{
    // RFC 8032 §7.1, TEST 1: an Ed25519 secret key and its public key.
    private const string Rfc8032Secret = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    private const string Rfc8032Public = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

    private const int IdentityKeyLength = 1984;

    private readonly string _dir = Path.Combine(Path.GetTempPath(), $"aura-identity-tests-{Guid.NewGuid():N}");
    private string IdentityPath => Path.Combine(_dir, "identity.json");

    public void Dispose()
    {
        if (Directory.Exists(_dir)) Directory.Delete(_dir, recursive: true);
    }

    private void WriteV1(string privateKeyHex, string publicKeyHex, DateTime createdAt)
    {
        Directory.CreateDirectory(_dir);
        // What the NSec-based client wrote: no Version field.
        File.WriteAllText(IdentityPath, $$"""
            {
              "DisplayName": "alice",
              "PrivateKeyHex": "{{privateKeyHex}}",
              "PublicKeyHex": "{{publicKeyHex}}",
              "CreatedAt": "{{createdAt:O}}"
            }
            """);
    }

    private JsonElement ReadFile()
    {
        using var doc = JsonDocument.Parse(File.ReadAllText(IdentityPath));
        return doc.RootElement.Clone();
    }

    private void AssertOwnerOnly()
    {
        if (OperatingSystem.IsWindows()) return;
        Assert.Equal(UnixFileMode.UserRead | UnixFileMode.UserWrite, File.GetUnixFileMode(IdentityPath));
    }

    [Fact]
    public void V2_RoundTrips()
    {
        using var created = UserIdentity.LoadOrCreate("alice", IdentityPath);
        Assert.Equal(IdentityKeyLength, created.IdentityPublicKey.Length);
        Assert.Equal(32, created.Ed25519PublicKey.Length);
        Assert.Matches("^[0-9a-f]{64}$", created.PublicKeyHex);

        var json = ReadFile();
        Assert.Equal(UserIdentity.FileVersion, json.GetProperty("Version").GetInt32());
        Assert.Equal("alice", json.GetProperty("DisplayName").GetString());
        Assert.Matches("^[0-9a-f]{128}$", json.GetProperty("IdentitySecretHex").GetString());
        Assert.True(json.TryGetProperty("CreatedAt", out _));
        Assert.False(json.TryGetProperty("PrivateKeyHex", out _));
        Assert.False(json.TryGetProperty("PublicKeyHex", out _));
        AssertOwnerOnly();

        using var loaded = UserIdentity.LoadOrCreate("ignored", IdentityPath);
        Assert.Equal("alice", loaded.DisplayName);
        Assert.Equal(created.PublicKeyHex, loaded.PublicKeyHex);
        Assert.Equal(created.IdentityPublicKey, loaded.IdentityPublicKey);
        Assert.Equal(created.CreatedAt, loaded.CreatedAt);
    }

    [Fact]
    public void V1_UpgradesToV2_KeepingTheEd25519Key()
    {
        var createdAt = new DateTime(2025, 3, 1, 9, 30, 0, DateTimeKind.Utc);
        WriteV1(Rfc8032Secret, Rfc8032Public, createdAt);

        using var upgraded = UserIdentity.Load(IdentityPath);

        Assert.Equal(Rfc8032Public, upgraded.PublicKeyHex);
        Assert.Equal(IdentityKeyLength, upgraded.IdentityPublicKey.Length);
        Assert.Equal("alice", upgraded.DisplayName);
        Assert.Equal(createdAt, upgraded.CreatedAt);

        // Rewritten as v2; the secret starts with the same Ed25519 key.
        var json = ReadFile();
        Assert.Equal(2, json.GetProperty("Version").GetInt32());
        var secretHex = json.GetProperty("IdentitySecretHex").GetString()!;
        Assert.Equal(128, secretHex.Length);
        Assert.StartsWith(Rfc8032Secret, secretHex);
        Assert.False(json.TryGetProperty("PrivateKeyHex", out _));
        AssertOwnerOnly();

        // The new ML-DSA seed was saved, not regenerated on each load.
        using var reloaded = UserIdentity.Load(IdentityPath);
        Assert.Equal(Rfc8032Public, reloaded.PublicKeyHex);
        Assert.Equal(upgraded.IdentityPublicKey, reloaded.IdentityPublicKey);
        Assert.Equal(secretHex, ReadFile().GetProperty("IdentitySecretHex").GetString());
    }

    [Fact]
    public void V1_WithMismatchedPublicKey_IsLeftUntouched()
    {
        WriteV1(Rfc8032Secret, new string('0', 64), DateTime.UtcNow);
        var before = File.ReadAllText(IdentityPath);

        Assert.Throws<InvalidDataException>(() => UserIdentity.Load(IdentityPath));
        Assert.Equal(before, File.ReadAllText(IdentityPath));
    }

    [Fact]
    public void UnknownVersion_IsRejectedAndLeftUntouched()
    {
        Directory.CreateDirectory(_dir);
        File.WriteAllText(IdentityPath, """{ "Version": 3, "DisplayName": "alice", "IdentitySecretHex": "00" }""");
        var before = File.ReadAllText(IdentityPath);

        Assert.Throws<InvalidDataException>(() => UserIdentity.Load(IdentityPath));
        Assert.Equal(before, File.ReadAllText(IdentityPath));
    }

    [Fact]
    public void V2_WithTruncatedSecret_IsRejected()
    {
        Directory.CreateDirectory(_dir);
        File.WriteAllText(IdentityPath, $$"""{ "Version": 2, "DisplayName": "alice", "IdentitySecretHex": "{{new string('a', 64)}}" }""");

        Assert.Throws<InvalidDataException>(() => UserIdentity.Load(IdentityPath));
    }

    [Fact]
    public void DisposedIdentity_CannotBeSaved()
    {
        // Dispose zeroes the secret; saving after that would replace the
        // identity file with an all-zero key.
        var identity = UserIdentity.Generate("alice");
        identity.Dispose();

        Assert.Throws<ObjectDisposedException>(() => identity.Save(IdentityPath));
        Assert.False(File.Exists(IdentityPath));
    }
}
