using System;
using System.Collections.Generic;
using Aura.Desktop.Services;
using Avalonia;
using Avalonia.Input;
using Avalonia.Styling;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>Settings sheet, persistence, theme, transmission and push-to-talk.</summary>
public partial class MainWindowViewModel
{
    [ObservableProperty] private bool _showSettings;

    [ObservableProperty] private bool _rnnoiseEnabled = true;
    [ObservableProperty] private bool _aecEnabled = true;
    [ObservableProperty] private bool _webrtcNsEnabled;
    [ObservableProperty] private bool _agcEnabled = true;
    [ObservableProperty] private int _dredDuration = 10;  // 10ms units
    [ObservableProperty] private int _jitterBufferMs = 40;
    [ObservableProperty] private int _masterVolume = 100;  // 0–100 %

    public IReadOnlyList<string> ThemeOptions { get; } = new[] { "Zenith", "Frost", "Bloom" };
    public IReadOnlyList<string> AppearanceOptions { get; } = new[] { "Dark", "Light", "System" };
    public IReadOnlyList<string> TransmissionOptions { get; } = new[]
    {
        TransmissionModes.DisplayName(TransmissionMode.PushToTalk),
        TransmissionModes.DisplayName(TransmissionMode.VoiceActivation),
        TransmissionModes.DisplayName(TransmissionMode.AlwaysOn),
    };

    [ObservableProperty] private int _themeIndex;
    [ObservableProperty] private int _appearanceIndex;
    [ObservableProperty] private int _transmissionIndex;

    /// <summary>0..100 in the UI, 0..1 persisted (macOS AuraVADSensitivity).</summary>
    [ObservableProperty] private int _vadSensitivity = 50;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PttKeyLabel))]
    [NotifyPropertyChangedFor(nameof(HudModeKey))]
    private PushToTalkBinding _pttBinding = PushToTalkBinding.Default;

    /// <summary>Waiting for the next key press to become the PTT binding.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PttKeyLabel))]
    private bool _isRecordingPttKey;

    /// <summary>True while loading persisted settings, to suppress redundant saves.</summary>
    private bool _suppressSettingsSave;

    public TransmissionMode TransmissionMode => (TransmissionMode)Math.Clamp(TransmissionIndex, 0, 2);
    public bool IsPushToTalkMode => TransmissionMode == TransmissionMode.PushToTalk;
    public bool IsVoiceActivationMode => TransmissionMode == TransmissionMode.VoiceActivation;

    public string PttKeyLabel => IsRecordingPttKey ? "Press a key…" : PttBinding.Display;

    /// <summary>HUD chip: `PTT · Ctrl+Space`, or the mode for the other two.</summary>
    public string HudModeLabel => IsPushToTalkMode ? "PTT ·" : TransmissionModes.DisplayName(TransmissionMode);
    public string HudModeKey => IsPushToTalkMode ? PttBinding.Display : "";

    /// <summary>Global on Windows; honest about in-window everywhere else.</summary>
    public string PttScopeNote => _globalHook?.IsInstalled == true
        ? "Works while Aura is in the background."
        : "Works only while the Aura window is focused on this platform.";

    public string HudModeTip => IsPushToTalkMode
        ? $"Hold {PttBinding.Display} to talk. {PttScopeNote}"
        : TransmissionMode == TransmissionMode.VoiceActivation
            ? "Voice Activation: the mic sends only when you speak."
            : "Always On: the mic sends whenever it's on.";

    public string VadThresholdLabel => $"threshold {TransmissionModes.VadThresholdDb(VadSensitivity / 100.0):0} dB";

    [RelayCommand]
    private void ToggleSettings()
    {
        ShowSettings = !ShowSettings;
        if (!ShowSettings) IsRecordingPttKey = false;
    }

    [RelayCommand]
    private void RecordPttKey() => IsRecordingPttKey = true;

    /// <summary>
    /// Called by the window while recording. Returns true if the key was
    /// consumed. Escape cancels; a modifier alone keeps waiting for a key.
    /// </summary>
    public bool TryRecordPttKey(Key key, KeyModifiers modifiers)
    {
        if (!IsRecordingPttKey) return false;
        if (key == Key.Escape)
        {
            IsRecordingPttKey = false;
            return true;
        }
        if (PushToTalkBinding.IsModifierKey(key) || key == Key.None) return true;

        PttBinding = new PushToTalkBinding(key, modifiers & (KeyModifiers.Control | KeyModifiers.Alt | KeyModifiers.Shift | KeyModifiers.Meta));
        IsRecordingPttKey = false;
        return true;
    }

    partial void OnRnnoiseEnabledChanged(bool value) { _audioManager?.SetNoiseSuppressionEnabled(value); SaveSettings(); }
    partial void OnAecEnabledChanged(bool value) { _audioManager?.SetWebrtcAecEnabled(value); SaveSettings(); }
    partial void OnWebrtcNsEnabledChanged(bool value)
    {
        _audioManager?.SetWebrtcNsEnabled(value);
        // RNNoise and WebRTC NS are alternatives.
        if (value) RnnoiseEnabled = false;
        SaveSettings();
    }
    partial void OnAgcEnabledChanged(bool value) { _audioManager?.SetWebrtcAgcEnabled(value); SaveSettings(); }
    partial void OnDredDurationChanged(int value) { _audioManager?.SetDredDuration(value); SaveSettings(); }
    partial void OnJitterBufferMsChanged(int value) { _audioManager?.SetJitterBufferMs((uint)value); SaveSettings(); }
    partial void OnMasterVolumeChanged(int value)
    {
        if (_audioEngine != null) _audioEngine.Volume = value / 100f;
        SaveSettings();
    }

    partial void OnThemeIndexChanged(int value)
    {
        AuraThemeManager.Apply((AuraThemeType)Math.Clamp(value, 0, 2));
        AppearanceChanged?.Invoke();
        SaveSettings();
    }

    partial void OnAppearanceIndexChanged(int value)
    {
        ApplyAppearance();
        SaveSettings();
    }

    partial void OnTransmissionIndexChanged(int value)
    {
        ApplyTransmission();
        foreach (var name in new[]
                 {
                     nameof(TransmissionMode), nameof(IsPushToTalkMode), nameof(IsVoiceActivationMode),
                     nameof(HudModeLabel), nameof(HudModeKey), nameof(HudModeTip),
                 })
        {
            OnPropertyChanged(name);
        }
        UpdateSelfRow();
        SaveSettings();
    }

    partial void OnVadSensitivityChanged(int value)
    {
        _audioManager?.SetVadThresholdDb(TransmissionModes.VadThresholdDb(value / 100.0));
        OnPropertyChanged(nameof(VadThresholdLabel));
        SaveSettings();
    }

    partial void OnPttBindingChanged(PushToTalkBinding value)
    {
        if (_globalHook != null) _globalHook.Binding = value;
        OnPropertyChanged(nameof(HudModeTip));
        SaveSettings();
    }

    /// <summary>Raised after a palette/appearance swap so the window re-tints its acrylic.</summary>
    public event Action? AppearanceChanged;

    private void ApplyAppearance()
    {
        if (Application.Current is { } app)
        {
            app.RequestedThemeVariant = AppearanceIndex switch
            {
                1 => ThemeVariant.Light,
                2 => ThemeVariant.Default,
                _ => ThemeVariant.Dark,
            };
        }
        AppearanceChanged?.Invoke();
    }

    private void ApplyTransmission()
    {
        // In push-to-talk and always-on every frame is sent; only voice
        // activation gates on the sender's VAD.
        _audioManager?.SetVadEnabled(TransmissionMode == TransmissionMode.VoiceActivation);
        _audioManager?.SetVadThresholdDb(TransmissionModes.VadThresholdDb(VadSensitivity / 100.0));
        if (_globalHook != null) _globalHook.Enabled = TransmissionMode == TransmissionMode.PushToTalk;
        if (TransmissionMode != TransmissionMode.PushToTalk) _ = EndPushToTalkAsync();
    }

    private void LoadSettings()
    {
        var s = AppSettings.Load();
        _settings = s;
        _suppressSettingsSave = true;
        RnnoiseEnabled = s.RnnoiseEnabled;
        AecEnabled = s.AecEnabled;
        WebrtcNsEnabled = s.WebrtcNsEnabled;
        AgcEnabled = s.AgcEnabled;
        DredDuration = s.DredDuration;
        JitterBufferMs = s.JitterBufferMs;
        MasterVolume = s.MasterVolume;
        LayoutMode = s.LayoutMode;
        ThemeIndex = (int)AuraThemeManager.Parse(s.Theme);
        AuraThemeManager.Apply((AuraThemeType)ThemeIndex);
        AppearanceIndex = s.Appearance switch { "light" => 1, "system" => 2, _ => 0 };
        ApplyAppearance();
        TransmissionIndex = (int)TransmissionModes.Parse(s.TransmissionMode);
        VadSensitivity = (int)Math.Round(Math.Clamp(s.VadSensitivity, 0, 1) * 100);
        PttBinding = PushToTalkBinding.Parse(s.PttBinding);
        ServerAddress = string.IsNullOrWhiteSpace(s.LastServerHost) ? "127.0.0.1" : s.LastServerHost;
        ServerPort = s.LastServerPort is > 0 and < 65536 ? s.LastServerPort : 8443;
        NotifyLevelIndex = (int)NotifyLevels.Parse(s.NotifyLevel);
        NotifyKeywords = s.NotifyKeywords ?? "";
        NotifyShowText = s.NotifyShowText;
        NotifyBadge = s.NotifyBadge;
        NotifyJoinLeave = s.NotifyJoinLeave;
        _suppressSettingsSave = false;
        NotifyLayoutDerived();
    }

    /// <summary>Persist the current settings (no-op during initial load).</summary>
    private void SaveSettings()
    {
        if (_suppressSettingsSave) return;
        var s = _settings;
        s.RnnoiseEnabled = RnnoiseEnabled;
        s.AecEnabled = AecEnabled;
        s.WebrtcNsEnabled = WebrtcNsEnabled;
        s.AgcEnabled = AgcEnabled;
        s.DredDuration = DredDuration;
        s.JitterBufferMs = JitterBufferMs;
        s.MasterVolume = MasterVolume;
        s.LayoutMode = LayoutMode;
        s.Theme = AuraThemeManager.ToSettingValue((AuraThemeType)Math.Clamp(ThemeIndex, 0, 2));
        s.Appearance = AppearanceIndex switch { 1 => "light", 2 => "system", _ => "dark" };
        s.TransmissionMode = TransmissionModes.ToSetting(TransmissionMode);
        s.VadSensitivity = VadSensitivity / 100.0;
        s.PttBinding = PttBinding.ToSetting();
        s.NotifyLevel = NotifyLevels.ToSetting(NotifyLevel);
        s.NotifyKeywords = NotifyKeywords;
        s.NotifyShowText = NotifyShowText;
        s.NotifyBadge = NotifyBadge;
        s.NotifyJoinLeave = NotifyJoinLeave;
        s.Save();
    }

    /// <summary>Push the current settings into the audio components.</summary>
    private void ApplyAudioSettings()
    {
        if (_audioManager != null)
        {
            _audioManager.SetNoiseSuppressionEnabled(RnnoiseEnabled);
            _audioManager.SetWebrtcAecEnabled(AecEnabled);
            _audioManager.SetWebrtcNsEnabled(WebrtcNsEnabled);
            _audioManager.SetWebrtcAgcEnabled(AgcEnabled);
            _audioManager.SetDredDuration(DredDuration);
            _audioManager.SetJitterBufferMs((uint)JitterBufferMs);
        }
        if (_audioEngine != null)
        {
            _audioEngine.Volume = MasterVolume / 100f;
            _audioEngine.OutputMuted = IsDeafened;
        }
        ApplyTransmission();
    }

    // ==========================================================================
    // Global push-to-talk (Windows). Elsewhere the window handles the key.
    // ==========================================================================

    private GlobalPushToTalkHook? _globalHook;

    /// <summary>Whether the window should handle the PTT key itself.</summary>
    public bool HandlesPushToTalkInWindow => _globalHook?.IsInstalled != true;

    private void InstallGlobalPushToTalk()
    {
        if (!OperatingSystem.IsWindows()) return;
        try
        {
            var hook = new GlobalPushToTalkHook { Binding = PttBinding, Enabled = IsPushToTalkMode };
            hook.Pressed += () => Dispatcher.UIThread.Post(() => _ = BeginPushToTalkAsync());
            hook.Released += () => Dispatcher.UIThread.Post(() => _ = EndPushToTalkAsync());
            if (hook.Install())
            {
                _globalHook = hook;
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[PTT] Global hook unavailable: {ex.Message}");
        }
        OnPropertyChanged(nameof(HudModeTip));
    }

    private void DisposeGlobalPushToTalk()
    {
        _globalHook?.Dispose();
        _globalHook = null;
    }
}
