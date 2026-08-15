using System.Collections.Generic;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Aura.Desktop.Services;

/// <summary>
/// Which column is wide. Persisted globally per user (not per channel), like on
/// macOS. Both the key (<c>AuraLayoutMode</c>) and the stored strings
/// (<c>chat_first</c> / <c>voice_focus</c>) match the macOS client's
/// <c>AuraLayoutMode</c> enum exactly — the handoff requires identical persisted
/// setting keys across clients.
/// </summary>
public enum LayoutMode
{
    [JsonStringEnumMemberName("chat_first")]
    ChatFirst,

    [JsonStringEnumMemberName("voice_focus")]
    VoiceFocus,
}

/// <summary>
/// User-tunable settings, persisted to a JSON file so they survive restarts.
/// Mirrors <see cref="UserIdentity"/>'s load/save pattern and lives alongside
/// identity.json in the platform config directory.
///
/// Keys that also exist on macOS carry an explicit <see cref="JsonPropertyName"/>
/// matching the macOS UserDefaults key exactly — the design handoff requires the
/// persisted setting keys to be identical across clients.
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

    /// <summary>Chat-first (default) or voice-focus. macOS key: AuraLayoutMode.</summary>
    [JsonPropertyName("AuraLayoutMode")]
    [JsonConverter(typeof(JsonStringEnumConverter))]
    public LayoutMode LayoutMode { get; set; } = LayoutMode.ChatFirst;

    /// <summary>zenith / frost / bloom. macOS key: AuraThemeSelection.</summary>
    [JsonPropertyName("AuraThemeSelection")]
    public string Theme { get; set; } = "zenith";

    /// <summary>
    /// Per-user playback gain, keyed by the stable user UUID rather than the
    /// session id — session ids are reallocated every connection, so keying on
    /// them would silently reset everyone's volume on reconnect.
    /// macOS key: AuraLocalVolumes.
    /// </summary>
    [JsonPropertyName("AuraLocalVolumes")]
    public Dictionary<string, float> LocalVolumes { get; set; } = new();

    /// <summary>
    /// Users muted locally only — the server is never told, and the sender keeps
    /// decoding so its Opus state stays healthy. macOS key: AuraLocallyMutedUsers.
    /// </summary>
    [JsonPropertyName("AuraLocallyMutedUsers")]
    public List<string> LocallyMutedUsers { get; set; } = new();

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
