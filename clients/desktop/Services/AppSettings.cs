using System.IO;
using System.Text.Json;

namespace Aura.Desktop.Services;

/// <summary>
/// User-tunable audio settings, persisted to a JSON file so they survive
/// restarts. Mirrors <see cref="UserIdentity"/>'s load/save pattern and lives
/// alongside identity.json in the platform config directory.
/// </summary>
public class AppSettings
{
    public bool RnnoiseEnabled { get; set; } = true;
    public bool AecEnabled { get; set; } = true;
    public bool WebrtcNsEnabled { get; set; } = false;
    public bool AgcEnabled { get; set; } = true;
    public int DredDuration { get; set; } = 10;     // 10ms units (100ms)
    public int JitterBufferMs { get; set; } = 40;
    public int MasterVolume { get; set; } = 100;     // 0–100 %

    /// <summary>Load settings, falling back to defaults if missing or unreadable.</summary>
    public static AppSettings Load()
    {
        try
        {
            var path = GetSettingsFilePath();
            if (File.Exists(path))
            {
                var json = File.ReadAllText(path);
                return JsonSerializer.Deserialize<AppSettings>(json) ?? new AppSettings();
            }
        }
        catch
        {
            // Corrupt or unreadable settings must never block startup.
        }
        return new AppSettings();
    }

    /// <summary>Persist settings (best-effort; a failed write is swallowed).</summary>
    public void Save()
    {
        try
        {
            var path = GetSettingsFilePath();
            var dir = Path.GetDirectoryName(path);
            if (!string.IsNullOrEmpty(dir) && !Directory.Exists(dir))
                Directory.CreateDirectory(dir);

            var json = JsonSerializer.Serialize(this, new JsonSerializerOptions { WriteIndented = true });
            File.WriteAllText(path, json);
        }
        catch
        {
            // Settings persistence is non-critical; never surface as an error.
        }
    }

    /// <summary>settings.json next to identity.json, reusing its platform path logic.</summary>
    public static string GetSettingsFilePath()
    {
        var dir = Path.GetDirectoryName(UserIdentity.GetIdentityFilePath())!;
        return Path.Combine(dir, "settings.json");
    }
}
