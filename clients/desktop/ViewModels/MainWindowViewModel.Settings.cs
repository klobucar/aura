using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
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
    [ObservableProperty] private bool _isMicLoopbackActive;
    [ObservableProperty] private double _loopbackLevel;

    public IReadOnlyList<string> BitrateOptions { get; } = new[] { "16 kbps", "32 kbps", "64 kbps", "96 kbps" };
    [ObservableProperty] private int _bitrateIndex = 2; // 64 kbps default

    public int SelectedBitrateKbps =>
        AppSettings.SupportedBitratesKbps[Math.Clamp(BitrateIndex, 0, AppSettings.SupportedBitratesKbps.Length - 1)];

    partial void OnBitrateIndexChanged(int value)
    {
        _audioManager?.SetOpusBitrate(SelectedBitrateKbps);
        SaveSettings();
    }

    [ObservableProperty] private bool _voiceSoundCuesEnabled = true;

    partial void OnVoiceSoundCuesEnabledChanged(bool value) => SaveSettings();

    public IReadOnlyList<string> PresenceOptions { get; } = new[] { "Online", "Idle", "Do Not Disturb" };

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PresenceLabel))]
    private int _presenceIndex;

    [ObservableProperty] private string _customStatus = "";

    public string PresenceLabel => PresenceIndex switch
    {
        1 => "Idle",
        2 => "DND",
        _ => "Online",
    };

    public UserPresence CurrentPresenceStatus => PresenceIndex switch
    {
        1 => UserPresence.Idle,
        2 => UserPresence.Dnd,
        _ => UserPresence.Online,
    };

    private System.Threading.CancellationTokenSource? _customStatusCts;

    partial void OnPresenceIndexChanged(int value)
    {
        SaveSettings();
        UpdateSelfRow();
        if (!_suppressSettingsSave && _client != null && IsAuthenticated)
        {
            _ = _client.UpdateStatusAsync(!IsMicEnabled, IsDeafened, CurrentPresenceStatus, CustomStatus);
        }
    }

    partial void OnCustomStatusChanged(string value)
    {
        if (value.Length > 128)
        {
            CustomStatus = value[..128];
            return;
        }
        SaveSettings();
        UpdateSelfRow();
        if (!_suppressSettingsSave && _client != null && IsAuthenticated)
        {
            _customStatusCts?.Cancel();
            var cts = new System.Threading.CancellationTokenSource();
            _customStatusCts = cts;
            _ = System.Threading.Tasks.Task.Run(async () =>
            {
                try
                {
                    await System.Threading.Tasks.Task.Delay(350, cts.Token);
                    if (_client != null && IsAuthenticated)
                    {
                        await _client.UpdateStatusAsync(!IsMicEnabled, IsDeafened, CurrentPresenceStatus, CustomStatus, cts.Token);
                    }
                }
                catch (OperationCanceledException) { }
            });
        }
    }

    partial void OnIsMicLoopbackActiveChanged(bool value)
    {
        if (value)
        {
            EnsureAudio();
            _audioEngine?.StartCapture();
        }
        else
        {
            LoopbackLevel = 0;
            if (!IsMicEnabled) _audioEngine?.StopCapture();
        }
    }

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

    public const string SystemDefaultDeviceLabel = "System Default";
    public ObservableCollection<string> InputDeviceOptions { get; } = new() { SystemDefaultDeviceLabel };
    public ObservableCollection<string> OutputDeviceOptions { get; } = new() { SystemDefaultDeviceLabel };
    [ObservableProperty] private int _inputDeviceIndex;
    [ObservableProperty] private int _outputDeviceIndex;

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

    /// <summary>Global where a hook is installed; honest about in-window everywhere else.</summary>
    public string PttScopeNote => _globalHook?.IsInstalled == true
        ? "Works while Aura is in the background."
        : NeedsPttPermission
            ? "Works only while the Aura window is focused until Aura has Input Monitoring permission."
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
        if (!ShowSettings)
        {
            IsRecordingPttKey = false;
            IsMicLoopbackActive = false;
        }
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

    private string? SelectedInputDeviceOrNull =>
        InputDeviceIndex > 0 && InputDeviceIndex < InputDeviceOptions.Count
            ? InputDeviceOptions[InputDeviceIndex]
            : null;

    private string? SelectedOutputDeviceOrNull =>
        OutputDeviceIndex > 0 && OutputDeviceIndex < OutputDeviceOptions.Count
            ? OutputDeviceOptions[OutputDeviceIndex]
            : null;

    partial void OnInputDeviceIndexChanged(int value)
    {
        if (!_suppressSettingsSave)
            _audioEngine?.SetDevices(SelectedInputDeviceOrNull, SelectedOutputDeviceOrNull);
        SaveSettings();
    }

    partial void OnOutputDeviceIndexChanged(int value)
    {
        if (!_suppressSettingsSave)
            _audioEngine?.SetDevices(SelectedInputDeviceOrNull, SelectedOutputDeviceOrNull);
        SaveSettings();
    }

    [RelayCommand]
    private void RefreshAudioDevices()
    {
        var prevIn = SelectedInputDeviceOrNull;
        var prevOut = SelectedOutputDeviceOrNull;
        var currentIn = prevIn ?? "";
        var currentOut = prevOut ?? "";

        var wasSuppressed = _suppressSettingsSave;
        _suppressSettingsSave = true;
        try
        {
            InputDeviceOptions.Clear();
            InputDeviceOptions.Add(SystemDefaultDeviceLabel);
            OutputDeviceOptions.Clear();
            OutputDeviceOptions.Add(SystemDefaultDeviceLabel);

            try
            {
                foreach (var dev in uniffi.aura_core.AuraCoreMethods.ListInputAudioDevices())
                {
                    if (!string.IsNullOrWhiteSpace(dev.Name) && !InputDeviceOptions.Contains(dev.Name))
                        InputDeviceOptions.Add(dev.Name);
                }
                foreach (var dev in uniffi.aura_core.AuraCoreMethods.ListOutputAudioDevices())
                {
                    if (!string.IsNullOrWhiteSpace(dev.Name) && !OutputDeviceOptions.Contains(dev.Name))
                        OutputDeviceOptions.Add(dev.Name);
                }
            }
            catch
            {
                // Native library may not be loaded in unit tests.
            }

            if (!string.IsNullOrWhiteSpace(currentIn) && !InputDeviceOptions.Contains(currentIn))
                InputDeviceOptions.Add(currentIn);
            if (!string.IsNullOrWhiteSpace(currentOut) && !OutputDeviceOptions.Contains(currentOut))
                OutputDeviceOptions.Add(currentOut);

            InputDeviceIndex = string.IsNullOrWhiteSpace(currentIn) ? 0 : Math.Max(0, InputDeviceOptions.IndexOf(currentIn));
            OutputDeviceIndex = string.IsNullOrWhiteSpace(currentOut) ? 0 : Math.Max(0, OutputDeviceOptions.IndexOf(currentOut));
        }
        finally
        {
            _suppressSettingsSave = wasSuppressed;
        }

        if (!wasSuppressed &&
            (!string.Equals(SelectedInputDeviceOrNull, prevIn, StringComparison.Ordinal) ||
             !string.Equals(SelectedOutputDeviceOrNull, prevOut, StringComparison.Ordinal)))
        {
            _audioEngine?.SetDevices(SelectedInputDeviceOrNull, SelectedOutputDeviceOrNull);
            SaveSettings();
        }
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
        // Ask for the macOS permission only once someone actually uses PTT.
        if (TransmissionMode == TransmissionMode.PushToTalk) TryInstallGlobalHook(requestPermission: true);
        else StopPttPermissionPoll();
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
        var bitrateIdx = Array.IndexOf(AppSettings.SupportedBitratesKbps, s.OpusBitrateKbps);
        BitrateIndex = bitrateIdx >= 0 ? bitrateIdx : 2;
        VoiceSoundCuesEnabled = s.VoiceSoundCues;
        PresenceIndex = s.PresenceStatus switch { "idle" => 1, "dnd" => 2, _ => 0 };
        CustomStatus = s.CustomStatus ?? "";
        RefreshAudioDevices();
        if (!string.IsNullOrWhiteSpace(s.AudioInputDevice) && !InputDeviceOptions.Contains(s.AudioInputDevice))
            InputDeviceOptions.Add(s.AudioInputDevice);
        InputDeviceIndex = string.IsNullOrWhiteSpace(s.AudioInputDevice) ? 0 : Math.Max(0, InputDeviceOptions.IndexOf(s.AudioInputDevice));
        if (!string.IsNullOrWhiteSpace(s.AudioOutputDevice) && !OutputDeviceOptions.Contains(s.AudioOutputDevice))
            OutputDeviceOptions.Add(s.AudioOutputDevice);
        OutputDeviceIndex = string.IsNullOrWhiteSpace(s.AudioOutputDevice) ? 0 : Math.Max(0, OutputDeviceOptions.IndexOf(s.AudioOutputDevice));
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
        LoadSavedServers();
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
        s.OpusBitrateKbps = SelectedBitrateKbps;
        s.VoiceSoundCues = VoiceSoundCuesEnabled;
        s.PresenceStatus = PresenceIndex switch { 1 => "idle", 2 => "dnd", _ => "online" };
        s.CustomStatus = CustomStatus;
        s.AudioInputDevice = InputDeviceIndex > 0 && InputDeviceIndex < InputDeviceOptions.Count
            ? InputDeviceOptions[InputDeviceIndex] : "";
        s.AudioOutputDevice = OutputDeviceIndex > 0 && OutputDeviceIndex < OutputDeviceOptions.Count
            ? OutputDeviceOptions[OutputDeviceIndex] : "";
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
            _audioManager.SetOpusBitrate(SelectedBitrateKbps);
        }
        if (_audioEngine != null)
        {
            _audioEngine.Volume = MasterVolume / 100f;
            _audioEngine.OutputMuted = IsDeafened;
            _audioEngine.SetDevices(SelectedInputDeviceOrNull, SelectedOutputDeviceOrNull);
        }
        ApplyTransmission();
    }

    // ==========================================================================
    // Global push-to-talk (Windows hook, macOS event tap). Elsewhere, or until
    // the hook installs, the window handles the key.
    // ==========================================================================

    private IGlobalPushToTalkHook? _globalHook;
    private DispatcherTimer? _pttPermissionPoll;
    private bool _needsPttPermission;

    /// <summary>Whether the window should handle the PTT key itself.</summary>
    public bool HandlesPushToTalkInWindow => _globalHook?.IsInstalled != true;

    /// <summary>macOS: the event tap is waiting on Input Monitoring permission.</summary>
    public bool NeedsPttPermission
    {
        get => _needsPttPermission;
        private set
        {
            if (SetProperty(ref _needsPttPermission, value)) RefreshPttScope();
        }
    }

    private void InstallGlobalPushToTalk()
    {
        IGlobalPushToTalkHook? hook = null;
        if (OperatingSystem.IsWindows()) hook = new WindowsPushToTalkHook();
        else if (OperatingSystem.IsMacOS()) hook = new MacPushToTalkHook();
        if (hook == null) return;

        hook.Binding = PttBinding;
        hook.Enabled = IsPushToTalkMode;
        hook.Pressed += () => Dispatcher.UIThread.Post(() => _ = BeginPushToTalkAsync());
        hook.Released += () => Dispatcher.UIThread.Post(() => _ = EndPushToTalkAsync());
        _globalHook = hook;
        // Windows needs no permission, so it installs whatever the mode, as before.
        TryInstallGlobalHook(requestPermission: IsPushToTalkMode);
    }

    /// <summary>
    /// Install the hook if it isn't yet. On macOS without Input Monitoring this
    /// optionally asks for it, then polls until it is granted.
    /// </summary>
    private void TryInstallGlobalHook(bool requestPermission)
    {
        if (_globalHook is not { IsInstalled: false } hook) return;
        try
        {
            if (OperatingSystem.IsMacOS() && !MacPushToTalkHook.HasPermission)
            {
                if (requestPermission) MacPushToTalkHook.RequestPermission();
                NeedsPttPermission = true;
                StartPttPermissionPoll();
                return;
            }

            if (hook.Install())
            {
                // A hold that began in the window would never see its key-up
                // there now that the window ignores the key, so end it here.
                if (IsPushToTalkHeld) _ = EndPushToTalkAsync();
                NeedsPttPermission = false;
                StopPttPermissionPoll();
                Console.WriteLine("[PTT] Global hook installed");
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[PTT] Global hook unavailable: {ex.Message}");
        }
        RefreshPttScope();
    }

    private void StartPttPermissionPoll()
    {
        if (_pttPermissionPoll != null) return;
        Console.WriteLine("[PTT] Waiting for Input Monitoring permission");
        _pttPermissionPoll = new DispatcherTimer { Interval = TimeSpan.FromSeconds(2) };
        _pttPermissionPoll.Tick += (_, _) => TryInstallGlobalHook(requestPermission: false);
        _pttPermissionPoll.Start();
    }

    private void StopPttPermissionPoll()
    {
        _pttPermissionPoll?.Stop();
        _pttPermissionPoll = null;
    }

    [RelayCommand]
    private void OpenPttPermissionSettings()
    {
        if (OperatingSystem.IsMacOS()) MacPushToTalkHook.OpenPermissionSettings();
    }

    private void RefreshPttScope()
    {
        OnPropertyChanged(nameof(PttScopeNote));
        OnPropertyChanged(nameof(HudModeTip));
    }

    private void DisposeGlobalPushToTalk()
    {
        StopPttPermissionPoll();
        _globalHook?.Dispose();
        _globalHook = null;
    }
}
