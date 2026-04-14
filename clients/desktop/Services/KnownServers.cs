using System;
using System.Collections.Concurrent;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace Aura.Desktop.Services;

/// <summary>
/// Manages persistent storage of trusted server fingerprints (TOFU).
/// </summary>
public class KnownServers
{
    private static readonly string FilePath = GetKnownServersPath();
    private static ConcurrentDictionary<string, string> _fingerprints = new();

    static KnownServers()
    {
        Load();
    }

    /// <summary>
    /// Check if a server's fingerprint is already trusted.
    /// </summary>
    public static bool IsTrusted(string host, string fingerprint)
    {
        if (_fingerprints.TryGetValue(host.ToLowerInvariant(), out var trustedFingerprint))
        {
            return trustedFingerprint.Equals(fingerprint, StringComparison.OrdinalIgnoreCase);
        }
        return false;
    }

    /// <summary>
    /// Add a server's fingerprint to the trusted list.
    /// </summary>
    public static void Trust(string host, string fingerprint)
    {
        _fingerprints[host.ToLowerInvariant()] = fingerprint;
        Save();
    }

    private static void Load()
    {
        try
        {
            if (File.Exists(FilePath))
            {
                var json = File.ReadAllText(FilePath);
                _fingerprints = JsonSerializer.Deserialize<ConcurrentDictionary<string, string>>(json) 
                    ?? new ConcurrentDictionary<string, string>();
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[KnownServers] Error loading fingerprints: {ex.Message}");
            _fingerprints = new ConcurrentDictionary<string, string>();
        }
    }

    private static void Save()
    {
        try
        {
            var dir = Path.GetDirectoryName(FilePath);
            if (!string.IsNullOrEmpty(dir) && !Directory.Exists(dir))
            {
                Directory.CreateDirectory(dir);
            }

            var json = JsonSerializer.Serialize(_fingerprints, new JsonSerializerOptions { WriteIndented = true });
            File.WriteAllText(FilePath, json);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[KnownServers] Error saving fingerprints: {ex.Message}");
        }
    }

    private static string GetKnownServersPath()
    {
        if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
        {
            return Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "Aura", "known_servers.json"
            );
        }
        else if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
        {
            return Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
                "Library", "Application Support", "Aura", "known_servers.json"
            );
        }
        else
        {
            // Linux
            return Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
                ".config", "aura", "known_servers.json"
            );
        }
    }
}
