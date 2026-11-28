using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using Avalonia.Media;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>
/// Main ViewModel for the Aura desktop client. Split by concern:
/// this file (layout, voice rail, mic/deafen/PTT), <c>.Connection</c>,
/// <c>.Chat</c>, <c>.Settings</c> and <c>.Roster</c>.
/// </summary>
public partial class MainWindowViewModel : ObservableObject, IAsyncDisposable
{
    private AuraNetworkClient? _client;
    private UserIdentity? _identity;
    private RustAudioEngine? _audioEngine;
    private AudioManager? _audioManager;

    /// <summary>Cancels in-flight audio sends when the mic stops.</summary>
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
    /// Local speech gate, in RMS. In always-on and PTT modes every frame
    /// produces a packet, so "a packet was produced" alone would mark us as
    /// talking forever; the level is what separates speech from an open mic.
    /// </summary>
    private const double LocalSpeechGate = 0.02;   // ≈ −34 dBFS

    /// <summary>Push-to-talk hold state; see <see cref="PushToTalkLatch"/>.</summary>
    private readonly PushToTalkLatch _ptt = new();

    /// <summary>The audio engine and manager survive a disconnect; wire their events once.</summary>
    private bool _audioEventsWired;

    // ==========================================================================
    // Observable Properties
    // ==========================================================================

    [ObservableProperty] private bool _isConnected;
    [ObservableProperty] private bool _isAuthenticated;
    [ObservableProperty] private string _connectionStatus = "Disconnected";
    [ObservableProperty] private string _serverAddress = "127.0.0.1";
    [ObservableProperty] private int _serverPort = 8443;
    [ObservableProperty] private string _serverPassword = "";
    [ObservableProperty] private string _displayName = "";
    [ObservableProperty] private string _publicKeyDisplay = "";
    [ObservableProperty] private bool _isMicEnabled;
    [ObservableProperty] private bool _isDeafened;
    [ObservableProperty] private Channel? _selectedChannel;
    [ObservableProperty] private ObservableCollection<Channel> _channels = new();

    /// <summary>Admin per the login response: shows channel management.</summary>
    [ObservableProperty] private bool _isAdmin;

    // ==========================================================================
    // Layout modes (handoff Overview + "Interactions & behavior" > Mode swap)
    // ==========================================================================

    /// <summary>
    /// Which column is wide. Persisted globally per user under the
    /// <c>AuraLayoutMode</c> key, shared with the macOS client.
    /// </summary>
    [ObservableProperty] private LayoutMode _layoutMode = LayoutMode.ChatFirst;

    /// <summary>Window width, fed from the view so the flex column can be sized.</summary>
    [ObservableProperty] private double _windowWidth = 1100;

    /// <summary>The voice rail (§3) / stage (§4).</summary>
    public VoiceRailViewModel VoiceRail { get; } = new();

    /// <summary>
    /// The QUIC banner, raised on a connect attempt that cannot succeed. Not
    /// shown at startup: on a machine that can never run Aura, an unprompted
    /// wall of remedy text on first launch is worse than the status line, which
    /// already says what is wrong.
    /// </summary>
    [ObservableProperty] private bool _showQuicUnavailable;

    public string QuicUnavailableHeadline => QuicSupport.Availability.Headline;
    public string QuicUnavailableRemedy => QuicSupport.Availability.Remedy ?? "";

    /// <summary>
    /// Everything persisted. Mutated in place and saved whole, so state owned
    /// by other components (the trust store's dictionaries) is never dropped.
    /// </summary>
    private AppSettings _settings = new();

    private TrustStore? _trustStore;
    private TrustStore Trust => _trustStore ??= new TrustStore(_settings, SaveSettings);

    /// <summary>The trust sheet (§7), opened over the channel.</summary>
    public TrustSheetViewModel TrustSheet => _trustSheet ??= CreateTrustSheet();
    private TrustSheetViewModel? _trustSheet;

    private TrustSheetViewModel CreateTrustSheet()
    {
        var sheet = new TrustSheetViewModel(Trust, NameForUuid);
        // A verification made in the sheet must show up in the roster and rail
        // as soon as it closes, not on the next epoch.
        sheet.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(TrustSheetViewModel.IsOpen) && !sheet.IsOpen) RefreshTrust(force: true);
        };
        return sheet;
    }

    /// <summary>
    /// Resolve an MLS credential identity (a user UUID) to the display name the
    /// roster knows. Names are labels for a key here, never the thing trusted.
    /// </summary>
    private string? NameForUuid(string uuid)
    {
        if (_client == null || string.IsNullOrEmpty(uuid)) return null;
        if (_client.UserUuid == uuid) return DisplayName;

        foreach (var user in Channels.SelectMany(c => c.Users))
        {
            if (!user.IsSelf && _client.UuidForSession(user.Id) == uuid) return user.Name;
        }
        return null;
    }

    /// <summary>
    /// Open the trust sheet for the current channel. Reached from the E2EE pill,
    /// the profile card's Compare keys, and the rail's `verify key` row.
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

        var channel = SelectedChannel;
        if (channel == null) return;

        await TrustSheet.OpenAsync(mls, channel.Id, channel.Name, focusUuid);
    }

    /// <summary>The rail's SILENT `verify key` rows know a session, not a uuid.</summary>
    [RelayCommand]
    private Task OpenTrustForSession(uint sessionId) =>
        OpenTrustSheet(_client?.UuidForSession(sessionId));

    public bool IsChatFirst => LayoutMode == LayoutMode.ChatFirst;
    public bool IsVoiceFocus => LayoutMode == LayoutMode.VoiceFocus;

    /// <summary>Roster is 268 in both modes.</summary>
    public double RosterWidth => 268;

    /// <summary>
    /// The only animated dimension. Chat-first pins the rail at 336 and lets
    /// chat flex; voice-focus pins chat at 320 and gives the rest to the stage.
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
        foreach (var name in new[]
                 {
                     nameof(IsChatFirst), nameof(IsVoiceFocus), nameof(VoiceColumnWidth), nameof(LaneHeight),
                     nameof(LaneSegmentHeight), nameof(LaneSegmentRadius), nameof(LaneNameColumnWidth),
                     nameof(TimeAxisHeight), nameof(ModeExtrasOpacity), nameof(WaveformBarCount),
                     nameof(WaveformHeight), nameof(HeroAvatarSize), nameof(HeroNameFontSize),
                     nameof(ChatBubbleMaxWidth), nameof(MessageAvatarWidth), nameof(LayoutToggleGlyph),
                     nameof(LayoutToggleTip),
                 })
        {
            OnPropertyChanged(name);
        }
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

    /// <summary>`ROSTER · 7 ONLINE` — everyone on the server, us included.</summary>
    public string RosterLabel => $"ROSTER · {Channels.Sum(c => c.Users.Count)} ONLINE";

    /// <summary>
    /// `verified · 3f8a…5503`, the identity card's fingerprint line. Same short
    /// fingerprint the trust sheet shows for our own key.
    /// </summary>
    public string FingerprintDisplay
    {
        get
        {
            var key = _identity?.IdentityPublicKey;
            if (key == null || key.Length == 0) return "no identity";
            try
            {
                return $"verified · {uniffi.aura_core.AuraCoreMethods.IdentityFingerprintShort(key)}";
            }
            catch
            {
                return "no identity";
            }
        }
    }

    /// <summary>`E2EE · epoch 14`, hidden until the channel has an MLS group.</summary>
    [ObservableProperty] private string _epochLabel = "";

    public bool HasEpoch => !string.IsNullOrEmpty(EpochLabel);

    partial void OnEpochLabelChanged(string value) => OnPropertyChanged(nameof(HasEpoch));

    /// <summary>
    /// Control-stream RTT in ms (`18 ms`), measured via `MSG_PING` / `MSG_PONG`
    /// (`System.Net.Quic` has no datagram API). Stays `"… ms"` until the first
    /// pong arrives or while disconnected.
    /// </summary>
    [ObservableProperty] private string _latencyLabel = "… ms";

    [ObservableProperty] private IBrush? _latencyBrush;

    /// <summary>Call quality tooltip/readout from AudioReceiverWrapper.GetStats().</summary>
    [ObservableProperty] private string _callQualitySummary = "Loss: 0.0% · PLC: 0 · Jitter: 0.0 ms";

    /// <summary>True when an administrator has server-muted our live session.</summary>
    [ObservableProperty] private bool _isServerMuted;

    /// <summary>PTT hold readout: `held 0:04`.</summary>
    [ObservableProperty] private string _pttHeldLabel = "";

    public bool IsPushToTalkHeld => _ptt.IsHeld;

    partial void OnSelectedChannelChanged(Channel? value)
    {
        foreach (var channel in Channels) channel.IsCurrent = ReferenceEquals(channel, value);
        OnPropertyChanged(nameof(ComposerPlaceholder));
        OnPropertyChanged(nameof(ChannelMotd));
        OnPropertyChanged(nameof(HasChannelMotd));
        ShowMessagesFor(value?.Id);
        ClearMentionsBelow();
        RefreshInvite();
        RefreshEpoch();
        // The rail rebuilds from the same segment store — switching channels
        // re-projects it rather than resetting the history.
        VoiceRail.Rebuild(BuildParticipants(), _talkStore);
    }

    // ==========================================================================
    // Initialization
    // ==========================================================================

    public MainWindowViewModel()
    {
        MsQuicLoader.TryLoadMsQuic();
        QuicSupport.CheckQuicSupport();

        // Loading goes through aura-core (and may upgrade a v1 file), so the
        // native library resolver comes first.
        var identityPath = UserIdentity.GetIdentityFilePath();
        try
        {
            NativeLibraryLoader.Initialize();
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

        // Aura speaks QUIC and nothing else, so this is not a degraded mode to
        // discover later — the app cannot work at all here.
        if (!QuicSupport.Availability.IsSupported)
        {
            ConnectionStatus = QuicSupport.Availability.Headline;
        }

        LoadSettings();
        WireChannelEditor();

        Channels.CollectionChanged += (_, _) => OnPropertyChanged(nameof(RosterLabel));
        ShowMessagesFor(null);

        LatencyBrush = AuraBrushes.Named("AuraTextDimBrush");

        _laneTimer.Tick += (_, _) => OnLaneTick();
        _meterTimer.Tick += (_, _) => OnMeterTick();
        _laneTimer.Start();
        _meterTimer.Start();

        InstallGlobalPushToTalk();
    }

    // ==========================================================================
    // Voice rail: talk segments, meters, current speaker
    // ==========================================================================

    /// <summary>
    /// Channel occupants as the rail sees them — the remote members plus us,
    /// with us always last so the lanes put "you" at the bottom like the mocks.
    /// </summary>
    private List<VoiceParticipant> BuildParticipants()
    {
        var participants = new List<VoiceParticipant>();
        if (SelectedChannel == null) return participants;

        foreach (var user in SelectedChannel.Users.Where(u => !u.IsSelf))
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

    /// <summary>1s tick: trim the window, re-lay out the lanes, refresh badges.</summary>
    private void OnLaneTick()
    {
        _talkStore.Trim();
        VoiceRail.Rebuild(BuildParticipants(), _talkStore);
        RefreshEpoch();
        RefreshCallQualityStats();
        RefreshPushToTalkLabel();
        RefreshTrust(force: false);
    }

    private void RefreshCallQualityStats()
    {
        if (!IsAuthenticated || SelectedChannel == null) return;
        if (_audioManager?.GetReceiverStats() is { } stats)
        {
            CallQualitySummary = stats.FormatSummary();
        }
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
        LoopbackLevel = IsMicLoopbackActive ? Math.Clamp(Math.Sqrt(level) * 100.0, 0, 100) : 0;

        if (_client != null && IsAuthenticated)
        {
            // Our session id is learned after connect, so keep the store in
            // step rather than setting it once.
            _talkStore.LocalSessionId = _client.UserId;

            var speaking = IsMicEnabled && producedPacket && level >= LocalSpeechGate;
            _talkStore.SetSpeaking(_client.UserId, speaking);
            if (SelfUser is { } self) self.IsSpeaking = speaking;
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

    // ==========================================================================
    // Microphone, deafen, push to talk
    // ==========================================================================

    /// <summary>
    /// Create the audio engine and manager once and wire their events once.
    /// Subscribing per mic toggle (as before) stacked a new capture handler on
    /// every toggle, so each frame was sent once per toggle so far.
    /// </summary>
    private void EnsureAudio()
    {
        NativeLibraryLoader.Initialize();
        var createdEngine = _audioEngine == null;
        _audioEngine ??= new RustAudioEngine(SelectedInputDeviceOrNull, SelectedOutputDeviceOrNull);
        if (createdEngine && !string.IsNullOrEmpty(_audioEngine.FallbackNotice))
        {
            ShowBanner(_audioEngine.FallbackNotice, isError: false);
        }
        _audioManager ??= new AudioManager();
        if (_audioEventsWired) return;

        _audioEngine.OnAudioData += OnCapturedAudio;
        _audioEngine.OnError += error =>
            Dispatcher.UIThread.Post(() => ShowBanner($"Audio error: {error}", isError: true));
        _audioEngine.OnCaptureLevel += OnCaptureLevel;
        _audioManager.OnLocalCapture += OnLocalCapture;
        // The jitter buffer's talking signal: remote speakers only, never us.
        _audioManager.OnActiveSpeakersChanged += speakers =>
            Dispatcher.UIThread.Post(() => UpdateSpeakingIndicators(speakers));
        _audioEventsWired = true;
        ApplyAudioSettings();
    }

    /// <summary>Capture thread: one 20ms frame. Encryption happens in the client.</summary>
    private void OnCapturedAudio(byte[] frame)
    {
        if (IsMicLoopbackActive)
        {
            _audioEngine?.PlayAudio(frame);
        }
        var client = _client;
        var cts = _audioCts;
        if (client == null || cts == null || cts.IsCancellationRequested) return;
        _ = client.SendAudioFrameAsync(frame, cts.Token);
    }

    [RelayCommand]
    private Task ToggleMicrophoneAsync()
    {
        // A click always wins over a hold in progress.
        _ptt.Disown();
        return SetMicAsync(!IsMicEnabled);
    }

    /// <summary>
    /// Open or close the mic. State changes before the first await, so calls
    /// interleaved by push-to-talk apply in the order they were made.
    /// </summary>
    private async Task SetMicAsync(bool open)
    {
        if (open && IsDeafened) return;
        if (IsMicEnabled == open) return;

        IsMicEnabled = open;
        if (open) StartMic(); else StopMic();
        if (VoiceSoundCuesEnabled && IsAuthenticated)
        {
            VoiceSoundCues.Play(_audioManager, _audioEngine, open ? VoiceSoundCues.CueKind.Unmute : VoiceSoundCues.CueKind.Mute);
        }
        UpdateSelfRow();

        if (_client != null)
        {
            await _client.UpdateStatusAsync(!IsMicEnabled, IsDeafened, CurrentPresenceStatus, CustomStatus);
        }
    }

    [RelayCommand]
    private async Task ToggleDeafenAsync()
    {
        IsDeafened = !IsDeafened;
        if (_audioEngine != null) _audioEngine.OutputMuted = IsDeafened;

        if (IsDeafened && IsMicEnabled)
        {
            // Auto-mute on deafen (sends the status update itself).
            _ptt.Disown();
            await SetMicAsync(false);
        }
        else if (_client != null)
        {
            await _client.UpdateStatusAsync(!IsMicEnabled, IsDeafened, CurrentPresenceStatus, CustomStatus);
        }
        UpdateSelfRow();
    }

    private void StartMic()
    {
        if (_client == null) return;
        EnsureAudio();
        _audioCts?.Cancel();
        _audioCts = new CancellationTokenSource();
        _audioEngine!.StartCapture();
    }

    private void StopMic()
    {
        _audioCts?.Cancel();
        _audioCts = null;
        if (!IsMicLoopbackActive) _audioEngine?.StopCapture();
    }

    /// <summary>
    /// PTT pressed. Opens the mic for the hold unless it is already open (a
    /// latched mic stays latched on release). Ignored while deafened, outside
    /// push-to-talk mode, or when not connected.
    /// </summary>
    public async Task BeginPushToTalkAsync()
    {
        if (TransmissionMode != TransmissionMode.PushToTalk) return;
        if (!IsAuthenticated || IsDeafened) return;

        var open = _ptt.Press(IsMicEnabled, DateTime.UtcNow);
        RefreshPushToTalkLabel();
        if (open) await SetMicAsync(true);
    }

    public async Task EndPushToTalkAsync()
    {
        if (!_ptt.IsHeld) return;
        var close = _ptt.Release();
        PttHeldLabel = "";
        OnPropertyChanged(nameof(IsPushToTalkHeld));
        if (close) await SetMicAsync(false);
    }

    private void RefreshPushToTalkLabel()
    {
        if (_ptt.HeldSince is not { } since) return;
        var held = DateTime.UtcNow - since;
        PttHeldLabel = $"held {held.Minutes}:{held.Seconds:00}";
        OnPropertyChanged(nameof(IsPushToTalkHeld));
    }

    private void UpdateSpeakingIndicators(HashSet<uint> speakers)
    {
        foreach (var user in Channels.SelectMany(c => c.Users).Where(u => !u.IsSelf))
        {
            user.IsSpeaking = speakers.Contains(user.Id);
        }

        // OnMeterTick drives our own lane; this set can never contain us.
        _talkStore.ApplyRemoteSpeakers(speakers, _client?.UserId);
        VoiceRail.RefreshSpeaker(BuildParticipants(), _talkStore);
    }

    public async ValueTask DisposeAsync()
    {
        IsMicLoopbackActive = false;
        _laneTimer.Stop();
        _meterTimer.Stop();
        _reconnectTimer.Stop();
        DisposeGlobalPushToTalk();
        await DisconnectInternalAsync();
        _identity?.Dispose();
        GC.SuppressFinalize(this);
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

    /// <summary>The emoji icon, or the speaker glyph when none is set.</summary>
    [ObservableProperty] private string _icon = "🔊";

    [ObservableProperty] private bool _isLobby;

    /// <summary>The channel we're in: 3px accent bar + subtle fill.</summary>
    [ObservableProperty] private bool _isCurrent;

    /// <summary>Admin context menu (edit / delete) is offered.</summary>
    [ObservableProperty] private bool _canManage;

    public bool HasOccupants => Users.Count > 0;

    /// <summary>First line of the comment, for the roster subtitle.</summary>
    public string CommentPreview => InlineMarkdown.PlainText(Comment.Split('\n')[0]);
    public bool HasComment => !string.IsNullOrWhiteSpace(Comment);

    // Set by the owning view model: context menus live in a popup outside the
    // window's visual tree, so they bind to the item rather than to an ancestor.
    public IRelayCommand? EditCommand { get; set; }
    public IRelayCommand? DeleteCommand { get; set; }

    partial void OnCommentChanged(string value)
    {
        OnPropertyChanged(nameof(CommentPreview));
        OnPropertyChanged(nameof(HasComment));
    }

    public Channel()
    {
        Users.CollectionChanged += (_, _) => OnPropertyChanged(nameof(HasOccupants));
    }
}

public partial class User : ObservableObject
{
    [ObservableProperty] private uint _id;
    [ObservableProperty] private string _name = "";

    /// <summary>The user's profile comment (Markdown).</summary>
    [ObservableProperty] private string _comment = "";
    [ObservableProperty] private bool _isMuted;
    [ObservableProperty] private bool _isDeafened;
    [ObservableProperty] private bool _isServerMuted;
    [ObservableProperty] private bool _isSpeaking;
    [ObservableProperty] private bool _isLocallyMuted;
    [ObservableProperty] private UserPresence _presence = UserPresence.Online;
    [ObservableProperty] private string _customStatus = "";
    [ObservableProperty] private Position3D _position = new(0, 0, 0);

    /// <summary>The local user's own row ("you").</summary>
    public bool IsSelf { get; init; }

    /// <summary>Admin items (server mute, "Moderate…") are offered in the context menu.</summary>
    [ObservableProperty] private bool _canModerate;

    /// <summary>Opens the moderation console on this member.</summary>
    public IRelayCommand? ModerateCommand { get; set; }

    /// <summary>Admin server-mute/unmute toggle for this member.</summary>
    public IRelayCommand? ServerMuteCommand { get; set; }

    public string ServerMuteLabel => IsServerMuted ? "Server unmute" : "Server mute";
    public bool HasCustomStatus => !string.IsNullOrWhiteSpace(CustomStatus);
    public string PresenceTooltip => Presence switch
    {
        UserPresence.Idle => "Idle",
        UserPresence.Dnd => "Do Not Disturb",
        _ => "Online",
    };

    /// <summary>
    /// Trust in this user's identity key, from the MLS group of the channel
    /// we're in. Users in other channels stay at TOFU: we hold no key for them.
    /// </summary>
    [ObservableProperty] private TrustState _trust = TrustState.Tofu;

    /// <summary>
    /// Roster key.fill in caution: a key that differs from the one first seen.
    /// A merely uncompared (TOFU) key is the normal state and gets no glyph.
    /// </summary>
    public bool NeedsKeyCheck => Trust == TrustState.KeyChanged;

    /// <summary>The trailing mic.slash glyph (deafen implies muted).</summary>
    public bool ShowMuted => IsMuted || IsDeafened;

    public string Initial => string.IsNullOrWhiteSpace(Name) ? "?" : Name.Trim()[..1].ToUpperInvariant();

    public IRelayCommand? OpenProfileCommand { get; set; }
    public IRelayCommand? ToggleLocalMuteCommand { get; set; }
    public IRelayCommand<string>? SetVolumeCommand { get; set; }

    /// <summary>Local mute and volume make no sense on our own row.</summary>
    public bool CanAdjust => !IsSelf;

    public string LocalMuteLabel => IsLocallyMuted ? "Unmute locally" : "Mute locally";

    partial void OnTrustChanged(TrustState value) => OnPropertyChanged(nameof(NeedsKeyCheck));
    partial void OnIsMutedChanged(bool value) => OnPropertyChanged(nameof(ShowMuted));
    partial void OnIsDeafenedChanged(bool value) => OnPropertyChanged(nameof(ShowMuted));
    partial void OnNameChanged(string value) => OnPropertyChanged(nameof(Initial));
    partial void OnIsLocallyMutedChanged(bool value) => OnPropertyChanged(nameof(LocalMuteLabel));
    partial void OnIsServerMutedChanged(bool value) => OnPropertyChanged(nameof(ServerMuteLabel));
    partial void OnCustomStatusChanged(string value) => OnPropertyChanged(nameof(HasCustomStatus));
    partial void OnPresenceChanged(UserPresence value) => OnPropertyChanged(nameof(PresenceTooltip));
}

public record Position3D(float X, float Y, float Z);
