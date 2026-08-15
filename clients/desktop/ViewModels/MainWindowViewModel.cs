using System;
using System.Collections.ObjectModel;
using System.Linq;
using System.Net;
using System.Threading;
using System.Threading.Tasks;
using System.Collections.Generic;
using System.Security.Authentication;
using Aura.Desktop.Services;
using Aura.V1Alpha1;
using Avalonia.Media;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>
/// Main ViewModel for the Aura desktop client.
/// Handles connection, authentication, and voice streaming.
/// </summary>
public partial class MainWindowViewModel : ObservableObject, IAsyncDisposable
{
    private AuraNetworkClient? _client;
    private UserIdentity? _identity;
    private RustAudioEngine? _audioEngine;
    private AudioManager? _audioManager;
    private CancellationTokenSource? _audioCts;

    /// <summary>Rolling 3-minute talk history behind the voice rail.</summary>
    private readonly TalkSegmentStore _talkStore = new();

    /// <summary>Trim + lane re-layout, once a second (handoff: "re-laid out every 1s").</summary>
    private readonly DispatcherTimer _laneTimer = new() { Interval = TimeSpan.FromSeconds(1) };

    /// <summary>Meters, waveform and the current-speaker card.</summary>
    private readonly DispatcherTimer _meterTimer = new() { Interval = TimeSpan.FromMilliseconds(100) };

    /// <summary>
    /// Loudest capture frame since the last meter tick. Written on the audio
    /// thread ~50×/s, drained on the UI thread — a lock beats marshalling every
    /// frame onto the dispatcher.
    /// </summary>
    private readonly object _levelLock = new();
    private float _pendingLevel;
    private bool _lastFrameProducedPacket = true;

    /// <summary>
    /// Local speech gate, in RMS. The Rust VAD is opt-in and off by default, so
    /// "a packet was produced" alone would mark us as talking forever; the level
    /// is what actually separates speech from an open mic.
    /// </summary>
    private const double LocalSpeechGate = 0.02;   // ≈ −34 dBFS

    private DateTime? _pttPressedAt;

    /// <summary>The audio engine and manager survive a disconnect; wire their events once.</summary>
    private bool _audioEventsWired;

    // ==========================================================================
    // Observable Properties
    // ==========================================================================
    
    [ObservableProperty]
    private bool _isConnected;
    
    [ObservableProperty]
    private bool _isAuthenticated;
    
    [ObservableProperty]
    private string _connectionStatus = "Disconnected";
    
    [ObservableProperty]
    private string _serverAddress = "127.0.0.1";
    
    [ObservableProperty]
    private int _serverPort = 8443;
    
    [ObservableProperty]
    private string _serverPassword = "";
    
    [ObservableProperty]
    private string _displayName = "";
    
    [ObservableProperty]
    private string _publicKeyDisplay = "";
    
    [ObservableProperty]
    private bool _isMicEnabled;
    
    [ObservableProperty]
    private bool _isDeafened;
    
    [ObservableProperty]
    private string _audioStats = "";
    
    [ObservableProperty]
    private Channel? _selectedChannel;
    
    [ObservableProperty]
    private ObservableCollection<ChatMessage> _messages = new();
    
    [ObservableProperty]
    private ObservableCollection<Channel> _channels = new();
    
    [ObservableProperty]
    private string _messageInput = "";

    // ==========================================================================
    // Layout modes (handoff Overview + "Interactions & behavior" > Mode swap)
    // ==========================================================================

    /// <summary>
    /// Which column is wide. Persisted globally per user under the
    /// <c>AuraLayoutMode</c> key, shared with the macOS client.
    /// </summary>
    [ObservableProperty]
    private LayoutMode _layoutMode = LayoutMode.ChatFirst;

    /// <summary>Window width, fed from the view so the flex column can be sized.</summary>
    [ObservableProperty]
    private double _windowWidth = 1100;

    /// <summary>The voice rail (§3) / stage (§4).</summary>
    public VoiceRailViewModel VoiceRail { get; } = new();

    /// <summary>
    /// Backing store for everything persisted, held rather than rebuilt so the
    /// <see cref="TrustStore"/> has somewhere durable to write.
    /// </summary>
    private AppSettings _settings = new();

    private TrustStore? _trustStore;

    /// <summary>Verification bookkeeping; see <see cref="TrustStore"/>.</summary>
    private TrustStore Trust => _trustStore ??= new TrustStore(_settings, SaveSettings);

    /// <summary>The trust sheet (§7), opened over the channel.</summary>
    public TrustSheetViewModel TrustSheet => _trustSheet ??=
        new TrustSheetViewModel(Trust, NameForUuid);

    private TrustSheetViewModel? _trustSheet;

    /// <summary>
    /// Resolve an MLS credential identity (a user UUID) to the display name the
    /// roster knows. Names are labels for a key here, never the thing trusted.
    /// </summary>
    private string? NameForUuid(string uuid)
    {
        if (_client == null || string.IsNullOrEmpty(uuid)) return null;

        foreach (var channel in Channels)
        {
            foreach (var user in channel.Users)
            {
                if (_client.UuidForSession(user.Id) == uuid) return user.Name;
            }
        }

        return _client.UserUuid == uuid ? DisplayName : null;
    }

    /// <summary>
    /// Open the trust sheet for the current channel. Reached from the roster,
    /// and from the rail's `verify key` row with that member preselected.
    /// </summary>
    [RelayCommand]
    private async Task OpenTrustSheet(string? focusUuid)
    {
        var mls = _client?.Mls;
        if (mls == null)
        {
            TrustSheet.IsOpen = true;
            TrustSheet.Error = "Connect to a channel before verifying identities.";
            return;
        }

        var channel = Channels.FirstOrDefault(c => c.IsCurrent);
        if (channel == null) return;

        await TrustSheet.OpenAsync(mls, channel.Id, channel.Name, focusUuid);
    }

    public bool IsChatFirst => LayoutMode == LayoutMode.ChatFirst;
    public bool IsVoiceFocus => LayoutMode == LayoutMode.VoiceFocus;

    /// <summary>Roster is 268 in both modes.</summary>
    public double RosterWidth => 268;

    /// <summary>
    /// The only animated dimension. Chat-first pins the rail at 336 and lets
    /// chat flex; voice-focus pins chat at 320 and gives the rest to the stage.
    /// Both are the same property so a mode swap is one cubic-ease-out tween.
    /// </summary>
    public double VoiceColumnWidth => IsChatFirst
        ? 336
        : Math.Max(336, WindowWidth - RosterWidth - 320);

    // Everything below is the same component set at two sizes (§3 vs §4).
    public double LaneHeight => IsChatFirst ? 16 : 22;
    public double LaneSegmentHeight => IsChatFirst ? 8 : 12;
    public double LaneSegmentRadius => IsChatFirst ? 4 : 6;
    public double LaneNameColumnWidth => IsChatFirst ? 0 : 74;
    public double TimeAxisHeight => IsChatFirst ? 0 : 16;
    public double ModeExtrasOpacity => IsChatFirst ? 0 : 1;
    public int WaveformBarCount => IsChatFirst ? VoiceRailViewModel.WaveformBars : VoiceRailViewModel.StageWaveformBars;
    public double WaveformHeight => IsChatFirst ? 30 : 44;
    public double HeroAvatarSize => IsChatFirst ? 52 : 88;
    public double HeroNameFontSize => IsChatFirst ? 14 : 18;
    public double ChatBubbleMaxWidth => IsChatFirst ? 470 : 230;

    /// <summary>34 beside each message in chat-first; 0 in the narrow column.</summary>
    public double MessageAvatarWidth => IsChatFirst ? 34 : 0;
    public string LayoutToggleGlyph => IsChatFirst ? "◫" : "◉";
    public string LayoutToggleTip => IsChatFirst ? "Voice focus (Ctrl+Alt+V)" : "Chat first (Ctrl+Alt+V)";

    partial void OnLayoutModeChanged(LayoutMode value)
    {
        NotifyLayoutDerived();
        SaveSettings();
    }

    partial void OnWindowWidthChanged(double value) => OnPropertyChanged(nameof(VoiceColumnWidth));

    private void NotifyLayoutDerived()
    {
        OnPropertyChanged(nameof(IsChatFirst));
        OnPropertyChanged(nameof(IsVoiceFocus));
        OnPropertyChanged(nameof(VoiceColumnWidth));
        OnPropertyChanged(nameof(LaneHeight));
        OnPropertyChanged(nameof(LaneSegmentHeight));
        OnPropertyChanged(nameof(LaneSegmentRadius));
        OnPropertyChanged(nameof(LaneNameColumnWidth));
        OnPropertyChanged(nameof(TimeAxisHeight));
        OnPropertyChanged(nameof(ModeExtrasOpacity));
        OnPropertyChanged(nameof(WaveformBarCount));
        OnPropertyChanged(nameof(WaveformHeight));
        OnPropertyChanged(nameof(HeroAvatarSize));
        OnPropertyChanged(nameof(HeroNameFontSize));
        OnPropertyChanged(nameof(ChatBubbleMaxWidth));
        OnPropertyChanged(nameof(MessageAvatarWidth));
        OnPropertyChanged(nameof(LayoutToggleGlyph));
        OnPropertyChanged(nameof(LayoutToggleTip));
    }

    /// <summary>Header toggle and <c>Ctrl+Alt+V</c>.</summary>
    [RelayCommand]
    private void ToggleLayoutMode()
    {
        LayoutMode = IsChatFirst ? LayoutMode.VoiceFocus : LayoutMode.ChatFirst;
    }

    // ==========================================================================
    // Header / roster labels. Copy is shared byte-for-byte with macOS.
    // ==========================================================================

    /// <summary>`ROSTER · 7 ONLINE`</summary>
    public string RosterLabel => $"ROSTER · {Channels.Sum(c => c.Users.Count)} ONLINE";

    /// <summary>`verified · 3f8a…5503`, the identity card's fingerprint line.</summary>
    public string FingerprintDisplay
    {
        get
        {
            var hex = _identity?.PublicKeyHex;
            if (string.IsNullOrEmpty(hex) || hex.Length < 8) return "no identity";
            return $"verified · {hex[..4]}…{hex[^4..]}";
        }
    }

    /// <summary>`E2EE · epoch 14`, hidden until the channel has an MLS group.</summary>
    [ObservableProperty]
    private string _epochLabel = "";

    public bool HasEpoch => !string.IsNullOrEmpty(EpochLabel);

    partial void OnEpochLabelChanged(string value) => OnPropertyChanged(nameof(HasEpoch));

    /// <summary>`12 messages today`</summary>
    [ObservableProperty]
    private string _messagesTodayLabel = "0 messages today";

    /// <summary>
    /// `… ms` until there is a round-trip measurement to show. The client has no
    /// RTT probe yet, so the pill stays in the handoff's reconnecting state
    /// rather than inventing a number.
    /// </summary>
    [ObservableProperty]
    private string _latencyLabel = "… ms";

    /// <summary>Latency dot: good/caution/danger by the &lt;80 / &lt;200 / ≥200ms rule.</summary>
    [ObservableProperty]
    private IBrush? _latencyBrush;

    /// <summary>`Message #Lounge…`</summary>
    public string ComposerPlaceholder =>
        SelectedChannel == null ? "Message…" : $"Message #{SelectedChannel.Name}…";

    /// <summary>PTT hold readout: `held 0:04`.</summary>
    [ObservableProperty]
    private string _pttHeldLabel = "";

    public bool IsPushToTalkHeld => _pttPressedAt != null;

    partial void OnSelectedChannelChanged(Channel? value)
    {
        OnPropertyChanged(nameof(ComposerPlaceholder));
        foreach (var channel in Channels) channel.IsCurrent = ReferenceEquals(channel, value);
        RefreshEpoch();
        // The rail rebuilds from the same segment store — switching channels
        // re-projects it rather than resetting the history.
        VoiceRail.Rebuild(BuildParticipants(), _talkStore);
    }

    // ==========================================================================
    // Audio Settings
    // ==========================================================================
    
    [ObservableProperty]
    private bool _rnnoiseEnabled = true;
    
    [ObservableProperty]
    private bool _aecEnabled = true;
    
    [ObservableProperty]
    private bool _webrtcNsEnabled = false;
    
    [ObservableProperty]
    private bool _agcEnabled = true;
    
    [ObservableProperty]
    private int _dredDuration = 10;  // 100ms
    
    [ObservableProperty]
    private int _jitterBufferMs = 40;

    [ObservableProperty]
    private int _masterVolume = 100;  // 0–100 %

    [ObservableProperty]
    private bool _showAudioSettings = false;
    
    [ObservableProperty]
    private bool _showingCertWarning = false;
    
    [ObservableProperty]
    private string _certFingerprint = "";
    
    [ObservableProperty]
    private string _certHost = "";
    
    partial void OnRnnoiseEnabledChanged(bool value) { _audioManager?.SetNoiseSuppressionEnabled(value); SaveSettings(); }
    partial void OnAecEnabledChanged(bool value) { _audioManager?.SetWebrtcAecEnabled(value); SaveSettings(); }
    partial void OnWebrtcNsEnabledChanged(bool value)
    {
        _audioManager?.SetWebrtcNsEnabled(value);
        // Auto-disable RNNoise when WebRTC NS is enabled
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

    /// <summary>True while loading persisted settings, to suppress redundant saves.</summary>
    private bool _suppressSettingsSave;

    /// <summary>
    /// Local-only mixer preferences, keyed by stable user UUID so they survive
    /// the other party reconnecting with a fresh session id.
    /// </summary>
    private Dictionary<string, float> _localVolumes = new();
    private List<string> _locallyMutedUsers = new();

    /// <summary>Persist local mixer preferences after the client mutates them.</summary>
    private void SaveLocalMixerPrefs()
    {
        if (_client == null) return;
        var (volumes, muted) = _client.LocalMixerPrefs();
        _localVolumes = volumes;
        _locallyMutedUsers = muted;
        SaveSettings();
    }

    /// <summary>Persist the current settings (no-op during initial load).</summary>
    private void SaveSettings()
    {
        if (_suppressSettingsSave) return;
        new AppSettings
        {
            RnnoiseEnabled = RnnoiseEnabled,
            AecEnabled = AecEnabled,
            WebrtcNsEnabled = WebrtcNsEnabled,
            AgcEnabled = AgcEnabled,
            DredDuration = DredDuration,
            JitterBufferMs = JitterBufferMs,
            MasterVolume = MasterVolume,
            LayoutMode = LayoutMode,
            Theme = AuraThemeManager.ToSettingValue(AuraThemeManager.Current),
            LocalVolumes = _localVolumes,
            LocallyMutedUsers = _locallyMutedUsers,
            // Carried across from the loaded settings rather than rebuilt: the
            // TrustStore mutates these in place, and rebuilding from scratch
            // here would silently discard every verification the user has made.
            VerifiedKeys = _settings.VerifiedKeys,
            BlockedKeys = _settings.BlockedKeys,
            KnownUserKeys = _settings.KnownUserKeys,
            KeyChangedAt = _settings.KeyChangedAt,
        }.Save();
    }

    /// <summary>Push the current settings into freshly-created audio components.</summary>
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
        if (_audioEngine != null) _audioEngine.Volume = MasterVolume / 100f;
    }
    
    // ==========================================================================
    // Initialization
    // ==========================================================================
    
    public MainWindowViewModel()
    {
        // Try to load libmsquic explicitly on macOS
        MsQuicLoader.TryLoadMsQuic();
        
        // Check QUIC support
        QuicSupport.CheckQuicSupport();
        
        // Try to load existing identity
        var identityPath = UserIdentity.GetIdentityFilePath();
        try
        {
            if (System.IO.File.Exists(identityPath))
            {
                _identity = UserIdentity.Load(identityPath);
                DisplayName = _identity.DisplayName;
                PublicKeyDisplay = _identity.PublicKeyHex[..16] + "...";
                ConnectionStatus = "Ready (identity loaded)";
            }
            else
            {
                ConnectionStatus = "Ready (no identity - will generate on connect)";
            }
        }
        catch (Exception ex)
        {
            ConnectionStatus = $"Error loading identity: {ex.Message}";
        }
        
        // Restore persisted audio settings (defaults if none saved). Suppress
        // saves while loading so setting each property doesn't rewrite the file.
        var settings = AppSettings.Load();
        _settings = settings;
        _suppressSettingsSave = true;
        RnnoiseEnabled = settings.RnnoiseEnabled;
        AecEnabled = settings.AecEnabled;
        WebrtcNsEnabled = settings.WebrtcNsEnabled;
        AgcEnabled = settings.AgcEnabled;
        DredDuration = settings.DredDuration;
        JitterBufferMs = settings.JitterBufferMs;
        MasterVolume = settings.MasterVolume;
        LayoutMode = settings.LayoutMode;
        AuraThemeManager.Apply(AuraThemeManager.Parse(settings.Theme));
        // Held here until a client exists to hand them to; the network client
        // owns the session → UUID mapping these are keyed on.
        _localVolumes = settings.LocalVolumes;
        _locallyMutedUsers = settings.LocallyMutedUsers;
        _suppressSettingsSave = false;
        NotifyLayoutDerived();

        // Start with empty channels - server will sync them on connect
        Channels = new ObservableCollection<Channel>();
        Messages.CollectionChanged += (_, _) => RefreshMessageCount();

        // No RTT probe exists yet, so the pill starts in the handoff's unknown
        // state: a textDim dot and `… ms`.
        LatencyBrush = AuraBrushes.Named("AuraTextDimBrush");

        _laneTimer.Tick += (_, _) => OnLaneTick();
        _meterTimer.Tick += (_, _) => OnMeterTick();
        _laneTimer.Start();
        _meterTimer.Start();
    }

    // ==========================================================================
    // Voice rail: talk segments, meters, current speaker
    // ==========================================================================

    /// <summary>
    /// Channel occupants as the rail sees them — the remote members plus us. The
    /// local user is never in <see cref="Channel.Users"/> (the roster shows them
    /// in the identity card instead), but the lanes need them: the mocks show
    /// "you" with a lane of their own.
    /// </summary>
    private List<VoiceParticipant> BuildParticipants()
    {
        var participants = new List<VoiceParticipant>();
        if (SelectedChannel == null) return participants;

        foreach (var user in SelectedChannel.Users)
        {
            participants.Add(new VoiceParticipant(
                user.Id, user.Name, user.IsMuted, user.IsDeafened, user.Trust, IsLocal: false));
        }

        if (_client != null && IsAuthenticated)
        {
            participants.Add(new VoiceParticipant(
                _client.UserId,
                string.IsNullOrWhiteSpace(DisplayName) ? "you" : DisplayName,
                IsMuted: !IsMicEnabled,
                IsDeafened,
                TrustState.Verified,
                IsLocal: true));
        }

        return participants;
    }

    /// <summary>1s tick: trim the window, re-lay out the lanes, refresh the epoch badge.</summary>
    private void OnLaneTick()
    {
        _talkStore.Trim();
        VoiceRail.Rebuild(BuildParticipants(), _talkStore);
        RefreshEpoch();
        RefreshPushToTalkLabel();
    }

    /// <summary>
    /// 100ms tick: drain the capture level into the meters, gate the local talking
    /// signal, and keep `speaking 0:12` counting.
    /// </summary>
    private void OnMeterTick()
    {
        float level;
        bool producedPacket;
        lock (_levelLock)
        {
            level = _pendingLevel;
            producedPacket = _lastFrameProducedPacket;
            _pendingLevel = 0;
        }

        VoiceRail.PushLevel(IsMicEnabled ? level : 0);

        if (_client != null && IsAuthenticated)
        {
            // Our session id is learned after connect, so keep the store in
            // step rather than setting it once — until it knows, it cannot tell
            // our lane apart from anyone else's and would put us back in the
            // hero card.
            _talkStore.LocalSessionId = _client.UserId;

            var speaking = IsMicEnabled && producedPacket && level >= LocalSpeechGate;
            _talkStore.SetSpeaking(_client.UserId, speaking);
        }

        VoiceRail.RefreshSpeaker(BuildParticipants(), _talkStore);
    }

    /// <summary>Capture-thread level sample; kept as a max so a tick can't miss a peak.</summary>
    private void OnCaptureLevel(float level)
    {
        lock (_levelLock)
        {
            if (level > _pendingLevel) _pendingLevel = level;
        }
    }

    /// <summary>Capture-thread packet gate (see <see cref="AudioManager.OnLocalCapture"/>).</summary>
    private void OnLocalCapture(bool producedPacket)
    {
        lock (_levelLock)
        {
            _lastFrameProducedPacket = producedPacket;
        }
    }

    private void RefreshEpoch()
    {
        var channelId = SelectedChannel?.Id;
        if (_client == null || string.IsNullOrEmpty(channelId))
        {
            EpochLabel = "";
            return;
        }

        var epoch = _client.CurrentTextEpoch(channelId);
        EpochLabel = epoch == null ? "" : $"E2EE · epoch {epoch}";
    }

    private void RefreshMessageCount()
    {
        var today = DateTime.Now.Date;
        var count = Messages.Count(m => !m.System && m.Timestamp.Date == today);
        MessagesTodayLabel = count == 1 ? "1 message today" : $"{count} messages today";
    }

    // ==========================================================================
    // Push to talk. In-window only for now: the low-level keyboard hook the
    // handoff calls for ("PTT · must become a global hook") is separate work.
    // ==========================================================================

    public async Task BeginPushToTalkAsync()
    {
        if (_pttPressedAt != null) return;
        _pttPressedAt = DateTime.UtcNow;
        RefreshPushToTalkLabel();
        if (!IsMicEnabled) await ToggleMicrophoneAsync();
    }

    public async Task EndPushToTalkAsync()
    {
        if (_pttPressedAt == null) return;
        _pttPressedAt = null;
        PttHeldLabel = "";
        OnPropertyChanged(nameof(IsPushToTalkHeld));
        if (IsMicEnabled) await ToggleMicrophoneAsync();
    }

    private void RefreshPushToTalkLabel()
    {
        if (_pttPressedAt == null) return;
        var held = DateTime.UtcNow - _pttPressedAt.Value;
        PttHeldLabel = $"held {held.Minutes}:{held.Seconds:00}";
        OnPropertyChanged(nameof(IsPushToTalkHeld));
    }
    
    // ==========================================================================
    // Commands
    // ==========================================================================
    
    [RelayCommand]
    private async Task ConnectAsync()
    {
        if (string.IsNullOrWhiteSpace(DisplayName))
        {
            ConnectionStatus = "Error: Display name required";
            return;
        }
        
        try
        {
            Console.WriteLine("[ViewModel] Starting connection...");
            
            // 1. Generate or load identity
            _identity ??= UserIdentity.LoadOrCreate(DisplayName);
            _identity.DisplayName = DisplayName;
            PublicKeyDisplay = _identity.PublicKeyHex[..16] + "...";
            OnPropertyChanged(nameof(FingerprintDisplay));
            Console.WriteLine($"[ViewModel] Identity loaded: {_identity.PublicKeyHex[..16]}...");
            
            // 2. Create client and connect
            _client = new AuraNetworkClient();
            _audioEngine ??= new RustAudioEngine();
            Console.WriteLine("[ViewModel] Creating AudioManager...");
            NativeLibraryLoader.Initialize();
            _audioManager ??= new AudioManager();
            Console.WriteLine("[ViewModel] AudioManager created");
            _client.SetAudioEngine(_audioEngine);
            _client.SetAudioManager(_audioManager);
            // Apply persisted audio settings to the freshly-created components.
            ApplyAudioSettings();
            Console.WriteLine("[ViewModel] Audio components wired");
            
            // Listen for active speaker changes. This is the jitter buffer's
            // talking signal: remote speakers only, never us.
            _audioManager.OnActiveSpeakersChanged += speakers =>
                Dispatcher.UIThread.Post(() => UpdateSpeakingIndicators(speakers));

            // Local talking signal — the other half of the picture. Both come off
            // the capture thread and are accumulated, not dispatched per frame.
            // The engine and manager outlive a disconnect, so wire these once.
            if (!_audioEventsWired)
            {
                _audioManager.OnLocalCapture += OnLocalCapture;
                _audioEngine.OnCaptureLevel += OnCaptureLevel;
                _audioEventsWired = true;
            }
            _client.OnStatusChanged += status => 
                Dispatcher.UIThread.Post(() => ConnectionStatus = status);
            _client.OnError += error => 
                Dispatcher.UIThread.Post(() => ConnectionStatus = $"Error: {error}");
            
            _client.OnUserJoined += (cid, sid, name) => 
                Dispatcher.UIThread.Post(() => HandleUserJoined(cid, sid, name));
            _client.OnUserLeft += (cid, sid) => 
                Dispatcher.UIThread.Post(() => HandleUserLeft(cid, sid));
            _client.OnServerSnapshot += snapshot => 
                Dispatcher.UIThread.Post(() => HandleServerSnapshot(snapshot));
            
            _client.OnTextMessage += (mid, sid, cid, content, reply) => 
                Dispatcher.UIThread.Post(() => HandleTextMessage(mid, sid, cid, content, reply));
            
            _client.OnUserStatusUpdated += (sid, muted, deafened) =>
                Dispatcher.UIThread.Post(() => HandleUserStatusUpdate(sid, muted, deafened));

            _client.OnChannelDeleted += (deletedId, fallbackId) =>
                Dispatcher.UIThread.Post(() => _ = HandleChannelDeletedAsync(deletedId, fallbackId));

            // Per-user playback gain is local-only state the rail edits and the
            // network client owns (it holds the session → UUID mapping the
            // preference is keyed on).
            _client.LoadLocalMixerPrefs(_localVolumes, _locallyMutedUsers);
            _client.OnLocalMixerPrefsChanged += SaveLocalMixerPrefs;
            VoiceRail.LocalVolumeProvider = sid => _client!.LocalVolumeForSession(sid);
            VoiceRail.LocalVolumeSetter = (sid, gain) => _client!.SetLocalVolume(sid, gain);

            ConnectionStatus = "Connecting...";
            Console.WriteLine($"[ViewModel] Connecting to {ServerAddress}:{ServerPort}...");
            await _client.ConnectAsync(ServerAddress, ServerPort);
            IsConnected = true;
            Console.WriteLine($"[ViewModel] IsConnected = {IsConnected}");
            
            // 3. Authenticate with TOFU
            ConnectionStatus = "Authenticating...";
            Console.WriteLine("[ViewModel] Starting authentication...");
            var password = string.IsNullOrWhiteSpace(ServerPassword) ? null : ServerPassword;
            await _client.AuthenticateAsync(_identity, password);
            Console.WriteLine("[ViewModel] Authentication completed!");
            IsAuthenticated = true;
            Console.WriteLine($"[ViewModel] IsAuthenticated = {IsAuthenticated}, UserId = {_client.UserId}");
            
            ConnectionStatus = $"Connected as {DisplayName} (ID: {_client.UserId})";
            Console.WriteLine($"[ViewModel] ConnectionStatus = {ConnectionStatus}");
            
            Messages.Add(new ChatMessage 
            { 
                Content = $"Connected to {ServerAddress}:{ServerPort}",
                System = true 
            });
            
            Console.WriteLine("[ViewModel] Connection complete!");
        }
        catch (UntrustedCertificateException ex)
        {
            Console.WriteLine($"[ViewModel] UNTRUSTED CERTIFICATE: {ex.Fingerprint}");
            CertFingerprint = ex.Fingerprint;
            CertHost = ex.Host;
            ShowingCertWarning = true;
            ConnectionStatus = "Certificate Verification Failed";
            await DisconnectInternalAsync();
        }
        catch (AuthenticationException ex)
        {
            Console.WriteLine($"[ViewModel] AUTH EXCEPTION: {ex.Message}");
            Console.WriteLine($"[ViewModel] Stack trace: {ex.StackTrace}");
            ConnectionStatus = $"Auth failed: {ex.Message}";
            await DisconnectInternalAsync();
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[ViewModel] CONNECTION EXCEPTION: {ex.GetType().Name}: {ex.Message}");
            Console.WriteLine($"[ViewModel] Stack trace: {ex.StackTrace}");
            if (ex.InnerException != null)
            {
                Console.WriteLine($"[ViewModel] INNER EXCEPTION: {ex.InnerException.GetType().Name}: {ex.InnerException.Message}");
                Console.WriteLine($"[ViewModel] Inner stack trace: {ex.InnerException.StackTrace}");
            }
            ConnectionStatus = $"Connection failed: {ex.Message}";
            await DisconnectInternalAsync();
        }
    }

    [RelayCommand]
    private async Task TrustAndConnectAsync()
    {
        if (string.IsNullOrEmpty(CertHost) || string.IsNullOrEmpty(CertFingerprint)) return;
        
        Console.WriteLine($"[ViewModel] Adding {CertHost} to trusted fingerprints: {CertFingerprint}");
        KnownServers.Trust(CertHost, CertFingerprint);
        
        ShowingCertWarning = false;
        await ConnectAsync();
    }

    [RelayCommand]
    private void DismissCertWarning()
    {
        ShowingCertWarning = false;
    }

    [RelayCommand]
    private async Task DisconnectAsync()
    {
        await DisconnectInternalAsync();
        ConnectionStatus = "Disconnected";
        Messages.Add(new ChatMessage 
        { 
            Content = "Disconnected from server",
            System = true 
        });
    }
    
    private async Task DisconnectInternalAsync()
    {
        StopMic();
        
        if (_client != null)
        {
            await _client.DisposeAsync();
            _client = null;
        }
        
        IsConnected = false;
        IsAuthenticated = false;
        Channels.Clear();
        _talkStore.Clear();
        VoiceRail.Rebuild(BuildParticipants(), _talkStore);
        EpochLabel = "";
        OnPropertyChanged(nameof(RosterLabel));
    }
    
    [RelayCommand]
    private async Task JoinChannelAsync(Channel? channel)
    {
        if (channel == null || _client == null) return;
        
        try
        {
            SelectedChannel = channel;
            channel.IsExpanded = true;
            
            await _client.JoinChannelAsync(channel.Id);
            
            Messages.Add(new ChatMessage 
            { 
                Content = $"Joined channel: {channel.Name}",
                System = true 
            });
        }
        catch (Exception ex)
        {
            ConnectionStatus = $"Failed to join channel: {ex.Message}";
        }
    }
    
    [RelayCommand]
    private async Task ToggleMicrophoneAsync()
    {
        if (IsDeafened && !IsMicEnabled) return;
        
        IsMicEnabled = !IsMicEnabled;
        if (IsMicEnabled)
        {
            StartMic();
        }
        else
        {
            StopMic();
        }
        
        if (_client != null)
        {
            await _client.UpdateStatusAsync(!IsMicEnabled, IsDeafened);
        }
    }
    
    [RelayCommand]
    private async Task ToggleDeafenAsync()
    {
        IsDeafened = !IsDeafened;
        
        if (IsDeafened && IsMicEnabled)
        {
            // Auto-mute on deafen
            await ToggleMicrophoneAsync();
        }
        else if (_client != null)
        {
            await _client.UpdateStatusAsync(!IsMicEnabled, IsDeafened);
        }
        
        // TODO: Actually mute output in RustAudioEngine if needed
    }
    
    private void StartMic()
    {
        if (_audioEngine == null || _client == null) return;
        
        _audioCts = new CancellationTokenSource();
        
        _audioEngine.OnAudioData += d =>
        {
            if (_client != null && IsAuthenticated)
            {
                // Off-thread send to avoid blocking engine
                _ = _client.SendAudioFrameAsync(d, _audioCts.Token);
            }
        };
        
        _audioEngine.OnError += error =>
            Dispatcher.UIThread.Post(() => ConnectionStatus = $"Audio error: {error}");
        
        _audioEngine.StartCapture();
        
        Messages.Add(new ChatMessage 
        { 
            Content = "Rust Audio Engine enabled (CPAL)",
            System = true 
        });
    }
    
    private void StopMic()
    {
        _audioCts?.Cancel();
        _audioEngine?.StopCapture();
        _audioCts = null;
        AudioStats = "";
    }
    
    [RelayCommand]
    private async Task SendMessage()
    {
        if (string.IsNullOrWhiteSpace(MessageInput)) return;
        if (_client == null || SelectedChannel == null) return;

        var content = MessageInput;
        MessageInput = ""; // Clear input immediately

        try 
        {
            string channelId = SelectedChannel.Id;
            string msgId = Guid.NewGuid().ToString();

            // Optimistic Add
            Messages.Add(new ChatMessage 
            { 
                UserId = _client.UserId,
                UserName = "You",
                Content = content,
                IsFromCurrentUser = true
            });

            await _client.SendTextMessageAsync(channelId, content, msgId);
        }
        catch (Exception ex)
        {
            Messages.Add(new ChatMessage { Content = $"Failed to send: {ex.Message}", System = true });
        }
    }

    private void HandleTextMessage(string msgId, uint senderId, string channelId, string content, string? replyToId)
    {
        // Don't show own messages again
        if (_client != null && senderId == _client.UserId) return;

        // Only show if for current channel or specific logic?
        // For simple parity, just show it.
        
        string senderName = $"User {senderId}";
        
        // Try to find name in channel
        var channel = Channels.FirstOrDefault(c => c.Id == channelId);
        if (channel != null)
        {
            var user = channel.Users.FirstOrDefault(u => u.Id == senderId);
            if (user != null) senderName = user.Name;
        }

        Messages.Add(new ChatMessage 
        { 
            UserId = senderId,
            UserName = senderName,
            Content = content,
            IsFromCurrentUser = false
        });
    }

    /// <summary>
    /// The channel we were in was deleted. Drop it from the list and move to a
    /// channel that still exists so we aren't left pointing at nothing.
    /// </summary>
    private async Task HandleChannelDeletedAsync(string deletedChannelId, string fallbackChannelId)
    {
        var wasCurrent = SelectedChannel?.Id == deletedChannelId;

        var deleted = Channels.FirstOrDefault(c => c.Id == deletedChannelId);
        if (deleted != null) Channels.Remove(deleted);

        if (!wasCurrent) return;

        SelectedChannel = null;
        Messages.Add(new ChatMessage { Content = "This channel was deleted", System = true });

        var fallback = Channels.FirstOrDefault(c => c.Id == fallbackChannelId)
                       ?? Channels.FirstOrDefault();
        if (fallback != null)
        {
            await JoinChannelAsync(fallback);
        }
    }

    private void HandleUserStatusUpdate(uint sessionId, bool isMuted, bool isDeafened)
    {
        foreach (var channel in Channels)
        {
            var user = channel.Users.FirstOrDefault(u => u.Id == sessionId);
            if (user != null)
            {
                user.IsMuted = isMuted;
                user.IsDeafened = isDeafened;
                break;
            }
        }
    }
    
    public async ValueTask DisposeAsync()
    {
        _laneTimer.Stop();
        _meterTimer.Stop();
        await DisconnectInternalAsync();
        _identity?.Dispose();
        GC.SuppressFinalize(this);
    }
    private void HandleUserJoined(string channelId, uint sessionId, string name)
    {
        var channel = GetOrCreateChannel(channelId);
        
        // Check if user already exists
        if (channel.Users.Any(u => u.Id == sessionId)) return;
        
        // Don't add self
        if (_client != null && sessionId == _client.UserId) return;
        
        channel.Users.Add(new User { Id = sessionId, Name = name });
        OnPropertyChanged(nameof(RosterLabel));

        // Log event
        Messages.Add(new ChatMessage { Content = $"{name} joined {channel.Name}", System = true });
    }
    
    private void HandleUserLeft(string channelId, uint sessionId)
    {
        var channel = Channels.FirstOrDefault(c => c.Id == channelId);
        if (channel == null) return;
        
        var user = channel.Users.FirstOrDefault(u => u.Id == sessionId);
        if (user != null)
        {
            channel.Users.Remove(user);
            _talkStore.Forget(sessionId);
            OnPropertyChanged(nameof(RosterLabel));
            Messages.Add(new ChatMessage { Content = $"{user.Name} left {channel.Name}", System = true });
        }
    }
    
    private void HandleServerSnapshot(ServerState snapshot)
    {
        Console.WriteLine($"[ViewModel] Handling ServerSnapshot: {snapshot.Channels.Count} channels");
        
        // 1. Build a map of user profiles for easy lookup
        var profileMap = snapshot.Profiles.ToDictionary(p => p.UserId, p => p);
        
        // 2. Sync channels
        // We want to preserve the selected channel if possible
        var previousSelectedId = SelectedChannel?.Id;
        
        Channels.Clear();
        foreach (var chanInfo in snapshot.Channels.OrderBy(c => c.Position))
        {
            var channel = new Channel
            {
                Id = chanInfo.ChannelId,
                Name = chanInfo.Name,
                Comment = chanInfo.Comment,
                IsExpanded = true
            };
            
            foreach (var userStatus in chanInfo.Users)
            {
                uint userId = userStatus.SessionId;
                if (_client != null && userId == _client.UserId) continue; // Skip self
                
                string name = $"User {userId}";
                string comment = "";
                
                if (profileMap.TryGetValue(userId.ToString(), out var profile))
                {
                    name = profile.DisplayName;
                    comment = profile.Bio;
                }
                
                channel.Users.Add(new User 
                { 
                    Id = userId, 
                    Name = name,
                    Comment = comment,
                    IsMuted = userStatus.IsMuted,
                    IsDeafened = userStatus.IsDeafened
                });
            }
            Channels.Add(channel);
        }
        
        // 3. Restore selection or pick first
        if (previousSelectedId != null)
        {
            SelectedChannel = Channels.FirstOrDefault(c => c.Id == previousSelectedId);
        }
        
        if (SelectedChannel == null && Channels.Count > 0)
        {
            SelectedChannel = Channels[0];
        }

        OnPropertyChanged(nameof(RosterLabel));
    }
    
    private Channel GetOrCreateChannel(string channelId)
    {
        var idStr = channelId;
        var channel = Channels.FirstOrDefault(c => c.Id == idStr);
        if (channel == null)
        {
            channel = new Channel 
            { 
                Id = idStr, 
                Name = $"Channel {channelId}",
                IsExpanded = true
            };
            Channels.Add(channel);
        }
        return channel;
    }
    
    private void UpdateSpeakingIndicators(HashSet<uint> speakers)
    {
        foreach (var channel in Channels)
        {
            foreach (var user in channel.Users)
            {
                user.IsSpeaking = speakers.Contains(user.Id);
            }
        }

        // Feed the talk lanes. The local session id is excluded here because it
        // can never appear in this set — OnMeterTick drives our own lane.
        _talkStore.ApplyRemoteSpeakers(speakers, _client?.UserId);
        VoiceRail.RefreshSpeaker(BuildParticipants(), _talkStore);
    }
    
    [RelayCommand]
    private void ToggleAudioSettings()
    {
        ShowAudioSettings = !ShowAudioSettings;
    }
}

// ==========================================================================
// Models
// ==========================================================================

public partial class Channel : ObservableObject
{
    [ObservableProperty] private string _id = "";
    [ObservableProperty] private string _name = "";
    [ObservableProperty] private bool _isExpanded;
    [ObservableProperty] private ObservableCollection<User> _users = new();

    /// <summary>Channel comment / MOTD (Markdown on the wire).</summary>
    [ObservableProperty] private string _comment = "";

    /// <summary>The channel we're in: 3px accent bar + subtle fill.</summary>
    [ObservableProperty] private bool _isCurrent;

    public bool HasOccupants => Users.Count > 0;

    public Channel()
    {
        Users.CollectionChanged += (_, _) => OnPropertyChanged(nameof(HasOccupants));
    }
}

public partial class User : ObservableObject
{
    [ObservableProperty] private uint _id;
    [ObservableProperty] private string _name = "";
    [ObservableProperty] private string _comment = "";
    [ObservableProperty] private bool _isMuted;
    [ObservableProperty] private bool _isDeafened;
    [ObservableProperty] private bool _isSpeaking;
    [ObservableProperty] private Position3D _position = new(0, 0, 0);

    /// <summary>
    /// Drives the roster key glyph and the rail's `verify key` row. Everyone
    /// starts at TOFU: out-of-band verification lands with the trust sheet (§7),
    /// which is separate work, so nothing sets this to Verified yet.
    /// </summary>
    [ObservableProperty] private TrustState _trust = TrustState.Tofu;

    /// <summary>Roster trailing glyph: mic.slash.fill in warn, key.fill in caution.</summary>
    public bool NeedsKeyCheck => Trust != TrustState.Verified;

    partial void OnTrustChanged(TrustState value) => OnPropertyChanged(nameof(NeedsKeyCheck));
}

public record Position3D(float X, float Y, float Z);

public class ChatMessage
{
    public uint UserId { get; init; }
    public string UserName { get; init; } = "";
    public string Content { get; init; } = "";
    public DateTime Timestamp { get; init; } = DateTime.Now;
    public bool IsFromCurrentUser { get; init; }
    public bool System { get; init; }

    /// <summary>Initial for the 34px avatar beside the message.</summary>
    public string Initial => string.IsNullOrWhiteSpace(UserName) ? "?" : UserName.Trim()[..1].ToUpperInvariant();

    /// <summary>`20:14`, mono, on the name row.</summary>
    public string TimeLabel => Timestamp.ToString("HH:mm");
}
