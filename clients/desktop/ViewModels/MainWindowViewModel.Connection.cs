using System;
using System.Collections.ObjectModel;
using System.Linq;
using System.Security.Authentication;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using Aura.V1Alpha1;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>Connect, reconnect, disconnect, and the server's roster events.</summary>
public partial class MainWindowViewModel
{
    [ObservableProperty] private bool _showingCertWarning;
    [ObservableProperty] private string _certFingerprint = "";
    [ObservableProperty] private string _certHost = "";

    /// <summary>
    /// The prominent banner over the chat column: connection failures, server
    /// refusals, audio errors. The 9px status line under the roster is not
    /// somewhere anyone looks when something has gone wrong.
    /// </summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasBanner))]
    private string _bannerText = "";

    [ObservableProperty] private bool _bannerIsError;

    public bool HasBanner => !string.IsNullOrEmpty(BannerText);

    /// <summary>Reconnect countdown in progress (banner shows Retry now / Cancel).</summary>
    [ObservableProperty] private bool _isReconnecting;

    private readonly DispatcherTimer _reconnectTimer = new() { Interval = TimeSpan.FromSeconds(1) };
    private int _reconnectAttempt;
    private DateTime _reconnectAt;
    private string _lossReason = "";

    /// <summary>Channel to rejoin after a reconnect; the lobby otherwise.</summary>
    private string? _rejoinChannelId;

    /// <summary>Join a channel when the first snapshot after login arrives.</summary>
    private bool _autoJoinPending;

    /// <summary>Saved server bookmarks loaded from servers.json.</summary>
    public ObservableCollection<ServerBookmark> SavedServers { get; } = new();

    public bool HasSavedServers => SavedServers.Count > 0;

    [ObservableProperty] private string _bookmarkName = "";

    private readonly System.Collections.Generic.Dictionary<string, (UserPresence? Presence, string? CustomStatus, bool IsServerMuted)> _userStatusCache = new();

    private void LoadSavedServers()
    {
        SavedServers.Clear();
        foreach (var bookmark in AppSettings.LoadSavedServers())
        {
            SavedServers.Add(bookmark);
        }
        OnPropertyChanged(nameof(HasSavedServers));
    }

    [RelayCommand]
    private void SaveCurrentServer()
    {
        var host = ServerAddress.Trim();
        if (string.IsNullOrEmpty(host))
        {
            ShowBanner("Enter a server address before saving.", isError: true);
            return;
        }

        var port = ServerPort is > 0 and < 65536 ? ServerPort : 8443;
        var bookmark = new ServerBookmark
        {
            Name = string.IsNullOrWhiteSpace(BookmarkName) ? host : BookmarkName.Trim(),
            Host = host,
            Port = port,
        };
        AppSettings.UpsertSavedServer(SavedServers, bookmark);
        AppSettings.SaveSavedServers(SavedServers);
        BookmarkName = "";
        OnPropertyChanged(nameof(HasSavedServers));
    }

    [RelayCommand]
    private void SelectSavedServer(ServerBookmark? bookmark)
    {
        if (bookmark == null) return;
        ServerAddress = bookmark.Host;
        ServerPort = bookmark.Port is > 0 and < 65536 ? bookmark.Port : 8443;
    }

    [RelayCommand]
    private void RemoveSavedServer(ServerBookmark? bookmark)
    {
        if (bookmark == null) return;
        if (SavedServers.Remove(bookmark))
        {
            AppSettings.SaveSavedServers(SavedServers);
            OnPropertyChanged(nameof(HasSavedServers));
        }
    }

    private void ShowBanner(string text, bool isError)
    {
        BannerText = text;
        BannerIsError = isError;
    }

    [RelayCommand]
    private void DismissBanner()
    {
        if (IsReconnecting) return;
        BannerText = "";
    }

    [RelayCommand]
    private Task ConnectAsync()
    {
        CancelReconnectInternal();
        _reconnectAttempt = 0;
        return ConnectCoreAsync(isReconnect: false);
    }

    /// <summary>
    /// One connect + login attempt at a time. The reconnect timer, "Retry now"
    /// and the Connect button all land here; a caller arriving mid-attempt
    /// shares the attempt instead of racing it.
    /// </summary>
    private readonly SingleFlight<bool> _connectFlight = new();

    private Task<bool> ConnectCoreAsync(bool isReconnect) =>
        _connectFlight.Run(() => ConnectOnceAsync(isReconnect));

    /// <summary>Host / port / password of the session we're connected to.</summary>
    private string _sessionHost = "";
    private int _sessionPort;
    private string _sessionPassword = "";

    /// <summary>
    /// The certificate pin an invite asked for, kept for reconnects to the same
    /// host so a Debug build can't quietly accept a different certificate.
    /// </summary>
    private string? _sessionPin;

    private async Task<bool> ConnectOnceAsync(bool isReconnect)
    {
        // Checked first: without QUIC the attempt cannot get off the ground,
        // and a generic timeout would send the user looking for a bug.
        if (!QuicSupport.Availability.IsSupported)
        {
            ShowQuicUnavailable = true;
            ConnectionStatus = QuicSupport.Availability.Headline;
            return false;
        }

        if (string.IsNullOrWhiteSpace(DisplayName))
        {
            ShowBanner("Choose a display name before connecting.", isError: true);
            return false;
        }

        var host = ServerAddress.Trim();
        var port = ServerPort;
        var password = string.IsNullOrWhiteSpace(ServerPassword) ? null : ServerPassword;

        // An invite's certificate pin applies to its own host only.
        var pin = PinFor(host);
        if (pin != null && KnownServers.PinFor(host) is { } existing &&
            !existing.Equals(pin, StringComparison.OrdinalIgnoreCase))
        {
            // Already pinned to something else: the invite and our own record
            // disagree, and neither is overridden from here.
            FailCertificateMismatch();
            return false;
        }

        // A client left over from a session that died without cleanup.
        if (_client != null) await DisconnectInternalAsync();

        AuraNetworkClient? client = null;
        try
        {
            _identity ??= UserIdentity.LoadOrCreate(DisplayName);
            _identity.DisplayName = DisplayName;
            PublicKeyDisplay = _identity.PublicKeyHex[..16] + "...";
            OnPropertyChanged(nameof(FingerprintDisplay));

            EnsureAudio();
            client = new AuraNetworkClient { ExpectedCertificateFingerprint = pin };
            client.SetAudioEngine(_audioEngine!);
            client.SetAudioManager(_audioManager!);
            WireClient(client);
            _client = client;

            ConnectionStatus = isReconnect ? "Reconnecting..." : "Connecting...";
            await client.ConnectAsync(host, port);
            if (!ReferenceEquals(_client, client)) return await AbandonAsync(client);
            IsConnected = true;

            ConnectionStatus = "Authenticating...";
            // Armed before login: the receive loop starts inside
            // AuthenticateAsync, so the first snapshot can be handled before
            // this method resumes.
            _autoJoinPending = true;
            if (_pendingInvite != null && SameHost(_pendingInvite.Host, host))
            {
                _rejoinChannelId = _pendingInvite.ChannelId ?? _rejoinChannelId;
            }
            await client.AuthenticateAsync(_identity, password);
            // Superseded meanwhile (the user disconnected): this attempt must
            // not leave a live session or a keepalive behind.
            if (!ReferenceEquals(_client, client)) return await AbandonAsync(client);

            IsAuthenticated = true;
            IsAdmin = client.IsAdmin;

            ConnectionStatus = $"Connected as {DisplayName}";
            BannerText = "";
            IsReconnecting = false;
            _reconnectAttempt = 0;
            _leavingServer = false;

            _sessionHost = host;
            _sessionPort = port;
            _sessionPassword = password ?? "";
            _sessionPin = pin;
            OnPropertyChanged(nameof(CanIncludeInvitePassword));
            RefreshInvite();

            // Only now that the pinned certificate actually got us in is the
            // pin worth remembering.
            if (pin != null) KnownServers.Trust(host, pin);
            if (_pendingInvite != null && SameHost(_pendingInvite.Host, host)) _pendingInvite = null;

            // Remember where we connected so next launch is one click.
            _settings.LastServerHost = host;
            _settings.LastServerPort = port;
            SaveSettings();

            // Send initial mute/deafen/presence/custom status so others see it immediately.
            await client.UpdateStatusAsync(!IsMicEnabled, IsDeafened, CurrentPresenceStatus, CustomStatus);
            if (IsAdmin) _ = RefreshUserInfoFromServerAsync();
            return true;
        }
        catch (Exception ex) when (FindInner<CertificateMismatchException>(ex) != null)
        {
            await AbandonAsync(client);
            FailCertificateMismatch();
            return false;
        }
        catch (Exception ex) when (FindInner<UntrustedCertificateException>(ex) is { } untrusted)
        {
            await AbandonAsync(client);
            CertFingerprint = untrusted.Fingerprint;
            CertHost = untrusted.Host;
            ShowingCertWarning = true;
            ConnectionStatus = "Certificate Verification Failed";
            // A changed server certificate is not something to retry through.
            CancelReconnectInternal();
            return false;
        }
        catch (AuthenticationException ex)
        {
            await AbandonAsync(client);
            if (ModerationLogic.IsBannedLogin(ex.Message))
            {
                // A ban doesn't lift by retrying: stop the reconnect loop.
                CancelReconnectInternal();
                _rejoinChannelId = null;
                ConnectionStatus = "Banned";
                ShowBanner(ModerationLogic.BannedNotice, isError: true);
                return false;
            }
            ConnectionStatus = $"Auth failed: {ex.Message}";
            if (!isReconnect) ShowBanner($"Login refused: {ex.Message}", isError: true);
            return false;
        }
        catch (Exception ex)
        {
            await AbandonAsync(client);
            var reason = ex.InnerException?.Message ?? ex.Message;
            ConnectionStatus = $"Connection failed: {reason}";
            if (!isReconnect) ShowBanner($"Couldn't connect to {host}:{port} — {reason}", isError: true);
            return false;
        }
    }

    /// <summary>
    /// Tear down one attempt's client. Only if it is still the current client
    /// does the session state get reset; a newer client is never touched.
    /// </summary>
    private async Task<bool> AbandonAsync(AuraNetworkClient? client)
    {
        if (client == null) return false;
        if (ReferenceEquals(_client, client))
        {
            await DisconnectInternalAsync();
        }
        else
        {
            await client.DisposeAsync();
        }
        return false;
    }

    private void FailCertificateMismatch()
    {
        CancelReconnectInternal();
        ConnectionStatus = "Certificate mismatch";
        ShowBanner("Server certificate doesn't match the invite", isError: true);
    }

    /// <summary>The pin to enforce for <paramref name="host"/>: a loaded invite's, else this session's.</summary>
    private string? PinFor(string host)
    {
        if (_pendingInvite is { Fingerprint: { } invitePin } invite && SameHost(invite.Host, host)) return invitePin;
        if (_sessionPin != null && SameHost(_sessionHost, host)) return _sessionPin;
        return null;
    }

    private static bool SameHost(string a, string b) => string.Equals(a.Trim(), b.Trim(), StringComparison.OrdinalIgnoreCase);

    private static T? FindInner<T>(Exception? ex) where T : Exception
    {
        for (var e = ex; e != null; e = e.InnerException)
        {
            if (e is T match) return match;
        }
        return null;
    }

    /// <summary>
    /// The client's status line as shown in the roster footer. A join arrives
    /// as a channel id (`C_7d63…`), which means nothing to a person: show the
    /// channel's name, as macOS does ("In #Lounge").
    /// </summary>
    private string DisplayStatus(string status)
    {
        if (!status.StartsWith(AuraNetworkClient.JoinedChannelStatusPrefix, StringComparison.Ordinal))
            return status;

        var channelId = status[AuraNetworkClient.JoinedChannelStatusPrefix.Length..];
        var name = Channels.FirstOrDefault(c => c.Id == channelId)?.Name;
        return string.IsNullOrWhiteSpace(name) ? "In channel" : $"In #{name}";
    }

    /// <summary>
    /// Subscribe to one client's events. Each connect builds a fresh client, so
    /// these never accumulate; audio engine/manager events are wired once in
    /// <see cref="EnsureAudio"/>.
    /// </summary>
    private void WireClient(AuraNetworkClient client)
    {
        void Ui(Action action) => Dispatcher.UIThread.Post(() =>
        {
            // Late events from a client we've already replaced are ignored.
            if (ReferenceEquals(_client, client)) action();
        });

        client.OnStatusChanged += status => Ui(() => ConnectionStatus = DisplayStatus(status));
        client.OnError += error => Ui(() => ConnectionStatus = $"Error: {error}");
        client.OnUserJoined += (cid, sid, name) => Ui(() => HandleUserJoined(cid, sid, name));
        client.OnUserLeft += (cid, sid) => Ui(() => HandleUserLeft(cid, sid));
        client.OnServerSnapshot += snapshot => Ui(() => HandleServerSnapshot(snapshot));
        client.OnTextMessage += (mid, sid, cid, content, reply) => Ui(() => HandleTextMessage(mid, sid, cid, content, reply));
        client.OnFileAnnouncement += (mid, sid, cid, caption, reply, manifest) =>
            Ui(() => HandleFileAnnouncement(mid, sid, cid, caption, reply, manifest));
        client.OnFileTransferUpdated += (cid, fileIdHex, status, localPath) =>
            Ui(() => HandleFileTransferUpdated(cid, fileIdHex, status, localPath));
        client.OnUserStatusUpdated += (sid, muted, deafened, presence, customStatus, serverMuted) =>
            Ui(() => HandleUserStatusUpdate(sid, muted, deafened, presence, customStatus, serverMuted));
        client.OnChannelDeleted += removal => Ui(() => _ = HandleChannelDeletedAsync(removal));
        client.OnBanned += () => Ui(() => _ = HandleBannedAsync());
        client.OnProfileUpdated += profile => Ui(() => HandleProfileUpdated(profile));
        client.OnRequestResult += (action, ok, error) => Ui(() => HandleRequestResult(action, ok, error));
        client.OnLatencyMeasured += rttMs => Ui(() => UpdateLatency(rttMs));
        client.OnConnectionLost += reason => Ui(() => _ = HandleConnectionLostAsync(reason));

        // Per-user playback gain is local-only state the network client owns
        // (it holds the session → UUID mapping the preference is keyed on).
        client.LoadLocalMixerPrefs(_settings.LocalVolumes, _settings.LocallyMutedUsers);
        client.OnLocalMixerPrefsChanged += () => Ui(SaveLocalMixerPrefs);
        VoiceRail.LocalVolumeProvider = sid => _client?.LocalVolumeForSession(sid) ?? 1f;
        VoiceRail.LocalVolumeSetter = (sid, gain) => _client?.SetLocalVolume(sid, gain);
    }

    private void UpdateLatency(int rttMs)
    {
        LatencyLabel = $"{rttMs} ms";
        LatencyBrush = rttMs switch
        {
            < 80 => AuraBrushes.Named("AuraGoodBrush"),
            < 180 => AuraBrushes.Named("AuraCautionBrush"),
            _ => AuraBrushes.Named("AuraDangerBrush"),
        };
    }

    private void SaveLocalMixerPrefs()
    {
        if (_client == null) return;
        var (volumes, muted) = _client.LocalMixerPrefs();
        _settings.LocalVolumes = volumes;
        _settings.LocallyMutedUsers = muted;
        SaveSettings();
    }

    [RelayCommand]
    private async Task TrustAndConnectAsync()
    {
        if (string.IsNullOrEmpty(CertHost) || string.IsNullOrEmpty(CertFingerprint)) return;
        KnownServers.Trust(CertHost, CertFingerprint);
        ShowingCertWarning = false;
        await ConnectAsync();
    }

    [RelayCommand]
    private void DismissCertWarning() => ShowingCertWarning = false;

    [RelayCommand]
    private async Task DisconnectAsync()
    {
        var wasAuthenticated = IsAuthenticated;
        CancelReconnectInternal();
        _rejoinChannelId = null;
        await DisconnectInternalAsync();
        if (wasAuthenticated && VoiceSoundCuesEnabled)
        {
            VoiceSoundCues.Play(null, _audioEngine, VoiceSoundCues.CueKind.Leave);
        }
        ConnectionStatus = "Disconnected";
        BannerText = "";
    }

    private async Task DisconnectInternalAsync()
    {
        await EndPushToTalkAsync();
        StopMic();
        IsMicEnabled = false;
        IsServerMuted = false;

        var client = _client;
        _client = null;
        if (client != null) await client.DisposeAsync();

        IsConnected = false;
        IsAuthenticated = false;
        IsAdmin = false;
        _autoJoinPending = false;
        SelectedChannel = null;
        Channels.Clear();
        _userStatusCache.Clear();
        // Avatars are re-sent after the next login; dropping them here means
        // one removed while we were away doesn't linger.
        _avatars.Clear();
        _talkStore.Clear();
        VoiceRail.Rebuild(BuildParticipants(), _talkStore);
        EpochLabel = "";
        LatencyLabel = "… ms";
        LatencyBrush = AuraBrushes.Named("AuraTextDimBrush");
        ProfileCard.Close();
        ChannelEditor.Close();
        _moderation?.CloseCommand.Execute(null);
    }

    // ==========================================================================
    // Reconnect (mirrors the macOS client: 1, 2, 4, 8, 16 s, five attempts)
    // ==========================================================================

    private async Task HandleConnectionLostAsync(string reason)
    {
        if (_client == null) return;

        // Leaving the server ends the session on purpose; never reconnect
        // into an account we just deleted.
        if (_leavingServer)
        {
            var host = _sessionHost;
            CancelReconnectInternal();
            _rejoinChannelId = null;
            await DisconnectInternalAsync();
            _leavingServer = false;
            ShowLeaveConfirm = false;
            IsLeaving = false;
            ConnectionStatus = "Disconnected";
            ShowBanner($"Disconnected from {host} before it confirmed the deletion. Reconnect to check whether your account is still there.", isError: true);
            return;
        }

        _rejoinChannelId = SelectedChannel?.Id ?? _rejoinChannelId;
        _lossReason = reason;
        await DisconnectInternalAsync();
        ConnectionStatus = "Disconnected";
        _reconnectAttempt = 0;
        ScheduleReconnect();
    }

    private void ScheduleReconnect()
    {
        _reconnectAttempt++;
        if (_reconnectAttempt > ReconnectPolicy.MaxAttempts)
        {
            CancelReconnectInternal();
            ShowBanner($"Connection lost: {_lossReason}. Couldn't reconnect after {ReconnectPolicy.MaxAttempts} attempts.", isError: true);
            return;
        }

        IsReconnecting = true;
        _reconnectAt = DateTime.UtcNow + ReconnectPolicy.DelayFor(_reconnectAttempt);
        UpdateReconnectBanner();
        if (!_reconnectTimerWired)
        {
            _reconnectTimer.Tick += (_, _) => OnReconnectTick();
            _reconnectTimerWired = true;
        }
        _reconnectTimer.Start();
    }

    private bool _reconnectTimerWired;

    private void UpdateReconnectBanner()
    {
        var seconds = Math.Max(0, (int)Math.Ceiling((_reconnectAt - DateTime.UtcNow).TotalSeconds));
        ShowBanner(
            $"Connection lost — reconnecting in {seconds}s (attempt {_reconnectAttempt}/{ReconnectPolicy.MaxAttempts})",
            isError: false);
    }

    private async void OnReconnectTick()
    {
        if (!IsReconnecting) { _reconnectTimer.Stop(); return; }
        if (DateTime.UtcNow < _reconnectAt)
        {
            UpdateReconnectBanner();
            return;
        }

        _reconnectTimer.Stop();
        await AttemptReconnectAsync();
    }

    private async Task AttemptReconnectAsync()
    {
        ShowBanner($"Reconnecting… (attempt {_reconnectAttempt}/{ReconnectPolicy.MaxAttempts})", isError: false);
        if (await ConnectCoreAsync(isReconnect: true)) return;
        if (IsReconnecting) ScheduleReconnect();
    }

    [RelayCommand]
    private async Task RetryNowAsync()
    {
        if (!IsReconnecting) return;
        _reconnectTimer.Stop();
        await AttemptReconnectAsync();
    }

    [RelayCommand]
    private void CancelReconnect()
    {
        CancelReconnectInternal();
        ShowBanner($"Disconnected: {_lossReason}", isError: true);
    }

    private void CancelReconnectInternal()
    {
        _reconnectTimer.Stop();
        IsReconnecting = false;
    }

    // ==========================================================================
    // Channels and occupants
    // ==========================================================================

    [RelayCommand]
    private async Task JoinChannelAsync(Channel? channel)
    {
        if (channel == null || _client == null) return;
        if (ReferenceEquals(channel, SelectedChannel) && SelfUser != null && channel.Users.Contains(SelfUser)) return;

        try
        {
            SelectedChannel = channel;
            channel.IsExpanded = true;
            PlaceSelf(channel.Id);
            await _client.JoinChannelAsync(channel.Id);
            if (VoiceSoundCuesEnabled) VoiceSoundCues.Play(_audioManager, _audioEngine, VoiceSoundCues.CueKind.Join);
            AddNotice(channel.Id, $"Joined #{channel.Name}");
        }
        catch (Exception ex)
        {
            ShowBanner($"Couldn't join #{channel.Name}: {ex.Message}", isError: true);
        }
    }

    /// <summary>
    /// The server took us out of a channel: it was deleted, never existed, or
    /// an admin moved us out of it. Either way move to the fallback; only a
    /// deletion removes the channel from the list, since after a kick it
    /// still exists.
    /// </summary>
    private async Task HandleChannelDeletedAsync(ChannelRemoval removal)
    {
        var channelId = removal.ChannelId;
        var wasCurrent = SelectedChannel?.Id == channelId;

        if (removal.Kind != ChannelRemovalKind.Kicked)
        {
            var deleted = Channels.FirstOrDefault(c => c.Id == channelId);
            if (deleted != null) Channels.Remove(deleted);
            _history.Remove(channelId);
        }

        var notice = ModerationLogic.RemovalNotice(removal);
        // Kicks and refused joins aren't something to learn from the chat log alone.
        if (removal.Kind != ChannelRemovalKind.Deleted) ShowBanner(notice, isError: false);

        if (wasCurrent)
        {
            SelectedChannel = null;
            var fallback = Channels.FirstOrDefault(c => c.Id == removal.FallbackChannelId) ?? Channels.FirstOrDefault();
            if (fallback != null)
            {
                await JoinChannelAsync(fallback);
                AddNotice(fallback.Id, notice);
            }
        }
        _moderation?.RefreshPresence();
    }

    /// <summary>
    /// The server closed our connection because the account was banned.
    /// Reconnecting would only be refused, so the session just ends.
    /// </summary>
    private async Task HandleBannedAsync()
    {
        if (_client == null) return;
        CancelReconnectInternal();
        _rejoinChannelId = null;
        await DisconnectInternalAsync();
        ConnectionStatus = "Banned";
        ShowBanner(ModerationLogic.BannedNotice, isError: true);
    }

    private void HandleUserStatusUpdate(
        uint sessionId,
        bool isMuted,
        bool isDeafened,
        UserPresence? presence,
        string? customStatus,
        bool? isServerMuted)
    {
        // Our own update: the server-mute flag is the only part we don't
        // already know. This is how a non-admin learns a moderator muted them.
        if (_client != null && sessionId == _client.UserId)
        {
            if (isServerMuted is { } selfMuted && selfMuted != IsServerMuted)
            {
                IsServerMuted = selfMuted;
                UpdateSelfRow();
            }
            return;
        }

        if (_client?.UuidForSession(sessionId) is { } uuid)
        {
            _userStatusCache.TryGetValue(uuid, out var prev);
            _userStatusCache[uuid] = (presence ?? prev.Presence, customStatus ?? prev.CustomStatus, isServerMuted ?? prev.IsServerMuted);
        }

        var user = Channels.SelectMany(c => c.Users).FirstOrDefault(u => !u.IsSelf && u.Id == sessionId);
        if (user == null) return;
        user.IsMuted = isMuted;
        user.IsDeafened = isDeafened;
        if (presence.HasValue) user.Presence = presence.Value;
        if (customStatus != null) user.CustomStatus = customStatus;
        if (isServerMuted.HasValue) user.IsServerMuted = isServerMuted.Value;
    }

    private async Task RefreshUserInfoFromServerAsync()
    {
        var client = _client;
        if (client == null || !IsAdmin) return;
        try
        {
            var resp = await client.ListUsersAsync(0, 500);
            if (!resp.Success || !ReferenceEquals(_client, client)) return;
            Dispatcher.UIThread.Post(() =>
            {
                if (!ReferenceEquals(_client, client)) return;
                foreach (var info in resp.Users)
                {
                    UserPresence? presence = info.HasPresence
                        ? info.Presence switch
                        {
                            Aura.V1Alpha1.PresenceStatus.PresenceIdle => UserPresence.Idle,
                            Aura.V1Alpha1.PresenceStatus.PresenceDnd => UserPresence.Dnd,
                            _ => UserPresence.Online,
                        }
                        : null;
                    var customStatus = info.HasCustomStatus ? info.CustomStatus : null;
                    _userStatusCache[info.UserUuid] = (presence, customStatus, info.IsServerMuted);

                    if (info.UserUuid == client.UserUuid)
                    {
                        IsServerMuted = info.IsServerMuted;
                        UpdateSelfRow();
                        continue;
                    }

                    foreach (var u in Channels.SelectMany(c => c.Users).Where(u => !u.IsSelf && client.UuidForSession(u.Id) == info.UserUuid))
                    {
                        if (presence.HasValue) u.Presence = presence.Value;
                        if (customStatus != null) u.CustomStatus = customStatus;
                        u.IsServerMuted = info.IsServerMuted;
                    }
                }
            });
        }
        catch
        {
            // Non-critical enrichment for admin roster metadata.
        }
    }

    private void HandleUserJoined(string channelId, uint sessionId, string name)
    {
        if (_client != null && sessionId == _client.UserId) return;

        // Moving between channels: drop them from wherever they were.
        foreach (var other in Channels)
        {
            var stale = other.Users.FirstOrDefault(u => !u.IsSelf && u.Id == sessionId);
            if (stale != null && other.Id != channelId) other.Users.Remove(stale);
        }

        var channel = GetOrCreateChannel(channelId);
        if (channel.Users.Any(u => !u.IsSelf && u.Id == sessionId)) return;

        var joined = NewUser(sessionId, name, "");
        var uuid = _client?.UuidForSession(sessionId);
        joined.Avatar = _avatars.Get(uuid);
        if (uuid != null && _userStatusCache.TryGetValue(uuid, out var cached))
        {
            if (cached.Presence.HasValue) joined.Presence = cached.Presence.Value;
            if (cached.CustomStatus != null) joined.CustomStatus = cached.CustomStatus;
            joined.IsServerMuted = cached.IsServerMuted;
        }
        channel.Users.Add(joined);
        OnPropertyChanged(nameof(RosterLabel));
        _moderation?.RefreshPresence();
        AddNotice(channelId, $"{name} joined");
        if (channelId == SelectedChannel?.Id)
        {
            NotifyForPresence($"{name} joined {channel.Name}");
        }
    }

    private void HandleUserLeft(string channelId, uint sessionId)
    {
        var channel = Channels.FirstOrDefault(c => c.Id == channelId);
        var user = channel?.Users.FirstOrDefault(u => !u.IsSelf && u.Id == sessionId);
        if (channel == null || user == null) return;

        channel.Users.Remove(user);
        _talkStore.Forget(sessionId);
        OnPropertyChanged(nameof(RosterLabel));
        _moderation?.RefreshPresence();
        AddNotice(channelId, $"{user.Name} left");
        if (channelId == SelectedChannel?.Id)
        {
            NotifyForPresence($"{user.Name} left {channel.Name}");
        }
        if (ProfileCard.IsOpen && ProfileCard.SessionId == sessionId) ProfileCard.Close();
    }

    private void HandleServerSnapshot(ServerState snapshot)
    {
        // Profiles are keyed by account UUID, not session id.
        var profiles = snapshot.Profiles
            .GroupBy(p => p.UserId)
            .ToDictionary(g => g.Key, g => g.Last());
        // Snapshots don't carry avatars: the server sends them after login and
        // on change as profile updates. An empty one here says nothing, so it
        // must not clear an image we already hold. (An older server that still
        // includes them is handled by the non-empty case.)
        foreach (var (uuid, p) in profiles)
        {
            if (!p.AvatarData.IsEmpty) _avatars.Update(uuid, p.AvatarData.ToByteArray());
        }

        var previousSelectedId = SelectedChannel?.Id;
        var ownId = _client?.UserId;
        string? selfChannelId = null;

        Channels.Clear();
        foreach (var info in snapshot.Channels.OrderBy(c => c.Position))
        {
            var channel = NewChannel(info);
            foreach (var status in info.Users)
            {
                if (status.SessionId == ownId)
                {
                    selfChannelId = info.ChannelId;
                    continue;
                }

                profiles.TryGetValue(status.UserUuid, out var profile);
                var name = !string.IsNullOrWhiteSpace(status.DisplayName) ? status.DisplayName
                    : !string.IsNullOrWhiteSpace(profile?.DisplayName) ? profile!.DisplayName
                    : $"User {status.SessionId}";

                var user = NewUser(status.SessionId, name, profile?.Bio ?? "");
                user.Avatar = _avatars.Get(status.UserUuid);
                user.IsMuted = status.IsMuted;
                user.IsDeafened = status.IsDeafened;
                if (_userStatusCache.TryGetValue(status.UserUuid, out var cached))
                {
                    if (cached.Presence.HasValue) user.Presence = cached.Presence.Value;
                    if (cached.CustomStatus != null) user.CustomStatus = cached.CustomStatus;
                    user.IsServerMuted = cached.IsServerMuted;
                }
                channel.Users.Add(user);
            }
            Channels.Add(channel);
        }

        if (_client?.UserUuid is { } ownUuid && profiles.TryGetValue(ownUuid, out var own))
        {
            MyBio = own.Bio;
            // Keep our own avatar bytes unless the snapshot actually has some:
            // they are what a profile save sends back, so emptying them here
            // would delete the avatar on the next save.
            if (!own.AvatarData.IsEmpty) _myAvatar = own.AvatarData.ToByteArray();
            MyAvatarImage = _avatars.Get(ownUuid)
                ?? (_myAvatar is { Length: > 0 } ? _avatars.Update(ownUuid, _myAvatar) : null);
        }

        var current = selfChannelId ?? previousSelectedId;
        SelectedChannel = current == null ? null : Channels.FirstOrDefault(c => c.Id == current);
        if (SelectedChannel != null) PlaceSelf(SelectedChannel.Id);
        OnPropertyChanged(nameof(RosterLabel));
        RefreshTrust(force: true);
        _moderation?.RefreshPresence();

        if (_autoJoinPending)
        {
            _autoJoinPending = false;
            var target = Channels.FirstOrDefault(c => c.Id == _rejoinChannelId)
                         ?? Channels.FirstOrDefault(c => c.IsLobby)
                         ?? Channels.FirstOrDefault();
            _rejoinChannelId = null;
            if (target != null && selfChannelId != target.Id) _ = JoinChannelAsync(target);
        }
    }

    private void HandleProfileUpdated(Aura.V1Alpha1.UserProfile profile)
    {
        if (_client == null) return;
        var avatar = _avatars.Update(profile.UserId, profile.AvatarData.ToByteArray());
        if (profile.UserId == _client.UserUuid)
        {
            MyBio = profile.Bio;
            _myAvatar = profile.AvatarData.ToByteArray();
            MyAvatarImage = avatar;
            return;
        }

        foreach (var user in Channels.SelectMany(c => c.Users).Where(u => !u.IsSelf))
        {
            if (_client.UuidForSession(user.Id) != profile.UserId) continue;
            if (!string.IsNullOrWhiteSpace(profile.DisplayName)) user.Name = profile.DisplayName;
            user.Comment = profile.Bio;
            user.Avatar = avatar;
            if (ProfileCard.IsOpen && ProfileCard.SessionId == user.Id) ProfileCard.Refresh(user);
        }

        // Messages already on screen follow the change too, including ones
        // from someone who has since left the roster.
        foreach (var message in _history.All)
        {
            if (message.SenderUuid == profile.UserId) message.Avatar = avatar;
        }
    }

    private Channel GetOrCreateChannel(string channelId)
    {
        var channel = Channels.FirstOrDefault(c => c.Id == channelId);
        if (channel != null) return channel;

        channel = NewChannel(new ChannelInfo { ChannelId = channelId, Name = $"Channel {channelId}" });
        Channels.Add(channel);
        return channel;
    }
}
