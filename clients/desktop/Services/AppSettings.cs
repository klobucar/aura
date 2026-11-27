using System;
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

    /// <summary>Preferred audio input device name (empty = system default).</summary>
    [JsonPropertyName("AuraAudioInputDevice")]
    public string AudioInputDevice { get; set; } = "";

    /// <summary>Preferred audio output device name (empty = system default).</summary>
    [JsonPropertyName("AuraAudioOutputDevice")]
    public string AudioOutputDevice { get; set; } = "";

    /// <summary>Chat-first (default) or voice-focus. macOS key: AuraLayoutMode.</summary>
    [JsonPropertyName("AuraLayoutMode")]
    [JsonConverter(typeof(JsonStringEnumConverter))]
    public LayoutMode LayoutMode { get; set; } = LayoutMode.ChatFirst;

    /// <summary>zenith / frost / bloom. macOS key: AuraThemeSelection.</summary>
    [JsonPropertyName("AuraThemeSelection")]
    public string Theme { get; set; } = "zenith";

    /// <summary>dark / light / system. The palette above comes in both variants.</summary>
    [JsonPropertyName("AuraAppearance")]
    public string Appearance { get; set; } = "dark";

    /// <summary>ptt / vad / always_on — the macOS raw values. macOS key: AuraTransmissionMode.</summary>
    [JsonPropertyName("AuraTransmissionMode")]
    public string TransmissionMode { get; set; } = "ptt";

    /// <summary>0 = very sensitive … 1 = loud speech only. macOS key: AuraVADSensitivity.</summary>
    [JsonPropertyName("AuraVADSensitivity")]
    public double VadSensitivity { get; set; } = 0.5;

    /// <summary>
    /// Push-to-talk key. Same key name as macOS; the value is this platform's
    /// own key encoding (see <see cref="PushToTalkBinding.ToSetting"/>), since
    /// key codes do not carry across operating systems.
    /// </summary>
    [JsonPropertyName("AuraPTTHotkey")]
    public string PttBinding { get; set; } = PushToTalkBinding.Default.ToSetting();

    /// <summary>Last server connected to, so the form is filled in next launch.</summary>
    [JsonPropertyName("AuraLastServerHost")]
    public string LastServerHost { get; set; } = "127.0.0.1";

    [JsonPropertyName("AuraLastServerPort")]
    public int LastServerPort { get; set; } = 8443;

    /// <summary>all / mentions / none. Same key and values as macOS.</summary>
    [JsonPropertyName("AuraNotifyLevel")]
    public string NotifyLevel { get; set; } = "mentions";

    /// <summary>Comma-separated words that also count as mentions.</summary>
    [JsonPropertyName("AuraNotifyKeywords")]
    public string NotifyKeywords { get; set; } = "";

    [JsonPropertyName("AuraNotifyShowText")]
    public bool NotifyShowText { get; set; } = true;

    [JsonPropertyName("AuraNotifyBadge")]
    public bool NotifyBadge { get; set; } = true;

    [JsonPropertyName("AuraNotifyJoinLeave")]
    public bool NotifyJoinLeave { get; set; }

    /// <summary>When the identity was last saved to an encrypted backup; null = never.</summary>
    [JsonPropertyName("AuraIdentityBackedUpAt")]
    public DateTimeOffset? IdentityBackedUpAt { get; set; }

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

    /// <summary>
    /// Identity keys the user compared out of band and accepted, keyed by
    /// lowercase hex of the Ed25519 public key and valued with when it happened.
    ///
    /// Keyed by <em>key</em>, never by user: that is what makes a key rotation
    /// drop back to unverified instead of silently inheriting the old key's
    /// trust. macOS key: AuraVerifiedKeys.
    /// </summary>
    [JsonPropertyName("AuraVerifiedKeys")]
    public Dictionary<string, DateTimeOffset> VerifiedKeys { get; set; } = new();

    /// <summary>
    /// Identity keys the user rejected from the trust sheet. Also keyed by key
    /// hex, so an impostor cannot clear a block by reconnecting.
    /// macOS key: AuraBlockedKeys.
    /// </summary>
    [JsonPropertyName("AuraBlockedKeys")]
    public Dictionary<string, DateTimeOffset> BlockedKeys { get; set; } = new();

    /// <summary>
    /// First key seen for each user UUID — the TOFU baseline. A later key that
    /// disagrees with this is what raises "key changed"; without it a takeover
    /// would be indistinguishable from a first sighting.
    /// macOS key: AuraKnownUserKeys.
    /// </summary>
    [JsonPropertyName("AuraKnownUserKeys")]
    public Dictionary<string, string> KnownUserKeys { get; set; } = new();

    /// <summary>
    /// When a user's key was first seen to differ from its baseline, so the UI
    /// can say "key changed today". macOS key: AuraKeyChangedAt.
    /// </summary>
    [JsonPropertyName("AuraKeyChangedAt")]
    public Dictionary<string, DateTimeOffset> KeyChangedAt { get; set; } = new();

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
