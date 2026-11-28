using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using Aura.V1Alpha1;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using uniffi.aura_core;

namespace Aura.Desktop.ViewModels;

/// <summary>Roster rows, the profile card (§6), channel admin, and trust state.</summary>
public partial class MainWindowViewModel
{
    /// <summary>Our own row, placed under the channel we're in.</summary>
    private User? SelfUser { get; set; }

    /// <summary>Our profile comment as the server last stored it.</summary>
    [ObservableProperty] private string _myBio = "";

    /// <summary>
    /// Our avatar as the server last stored it (possibly set from another
    /// client). Resent with every profile update, because the server replaces
    /// the whole profile and an empty avatar would wipe it.
    /// </summary>
    private byte[] _myAvatar = Array.Empty<byte>();

    public ProfileCardViewModel ProfileCard { get; } = new();
    public ChannelEditorViewModel ChannelEditor { get; } = new();

    private User NewUser(uint sessionId, string name, string comment)
    {
        var user = new User { Id = sessionId, Name = name, Comment = comment };
        user.IsLocallyMuted = _client?.IsLocallyMuted(sessionId) ?? false;
        user.CanModerate = IsAdmin;
        user.ModerateCommand = new RelayCommand(() => _ = OpenModerationFor(user));
        user.ServerMuteCommand = new RelayCommand(() => _ = ToggleServerMuteAsync(user));
        user.OpenProfileCommand = new RelayCommand(() => OpenProfile(user));
        user.ToggleLocalMuteCommand = new RelayCommand(() => ToggleLocalMute(user));
        user.SetVolumeCommand = new RelayCommand<string>(pct =>
        {
            if (_client != null && int.TryParse(pct, NumberStyles.Integer, CultureInfo.InvariantCulture, out var v))
                _client.SetLocalVolume(user.Id, v / 100f);
        });
        return user;
    }

    private Channel NewChannel(ChannelInfo info)
    {
        var channel = new Channel
        {
            Id = info.ChannelId,
            Name = info.Name,
            Comment = info.Comment,
            Icon = string.IsNullOrWhiteSpace(info.Icon?.Emoji) ? "🔊" : info.Icon!.Emoji,
            IsLobby = info.Type == Aura.V1Alpha1.ChannelType.Lobby,
            IsExpanded = true,
            CanManage = IsAdmin,
        };
        channel.EditCommand = new RelayCommand(() => ChannelEditor.OpenForEdit(channel));
        channel.DeleteCommand = new RelayCommand(() => ChannelEditor.OpenForEdit(channel, confirmDelete: true));
        return channel;
    }

    partial void OnIsAdminChanged(bool value)
    {
        foreach (var channel in Channels)
        {
            channel.CanManage = value;
            foreach (var user in channel.Users) user.CanModerate = value && !user.IsSelf;
        }
    }

    /// <summary>Show ourselves under <paramref name="channelId"/> and nowhere else.</summary>
    private void PlaceSelf(string channelId)
    {
        if (_client == null) return;
        foreach (var channel in Channels)
        {
            var existing = channel.Users.FirstOrDefault(u => u.IsSelf);
            if (existing != null && channel.Id != channelId) channel.Users.Remove(existing);
        }

        var target = Channels.FirstOrDefault(c => c.Id == channelId);
        if (target == null) return;

        SelfUser = target.Users.FirstOrDefault(u => u.IsSelf) ?? SelfUser;
        if (SelfUser == null || !target.Users.Contains(SelfUser))
        {
            SelfUser = new User
            {
                Id = _client.UserId,
                Name = string.IsNullOrWhiteSpace(DisplayName) ? "you" : DisplayName,
                IsSelf = true,
                Trust = TrustState.Verified,
            };
            SelfUser.OpenProfileCommand = new RelayCommand(() => OpenProfile(SelfUser));
            target.Users.Add(SelfUser);
        }
        UpdateSelfRow();
        OnPropertyChanged(nameof(RosterLabel));
    }

    /// <summary>
    /// Our row's glyphs. In push-to-talk the mic is closed between holds by
    /// design, so it does not read as "muted".
    /// </summary>
    private void UpdateSelfRow()
    {
        if (SelfUser == null) return;
        SelfUser.Name = string.IsNullOrWhiteSpace(DisplayName) ? "you" : DisplayName;
        SelfUser.Comment = MyBio;
        SelfUser.Avatar = MyAvatarImage;
        SelfUser.IsDeafened = IsDeafened;
        SelfUser.IsMuted = !IsMicEnabled && !IsPushToTalkMode;
        SelfUser.IsServerMuted = IsServerMuted;
        SelfUser.Presence = CurrentPresenceStatus;
        SelfUser.CustomStatus = CustomStatus;
    }

    partial void OnMyBioChanged(string value)
    {
        UpdateSelfRow();
        if (ProfileCard.IsOpen && ProfileCard.IsSelf) ProfileCard.Bio = value;
    }

    private void ToggleLocalMute(User user)
    {
        if (_client == null || user.IsSelf) return;
        user.IsLocallyMuted = _client.ToggleLocalMute(user.Id);
        if (ProfileCard.IsOpen && ProfileCard.SessionId == user.Id) ProfileCard.IsLocallyMuted = user.IsLocallyMuted;
    }

    private async Task ToggleServerMuteAsync(User user)
    {
        if (_client == null || !IsAdmin || user.IsSelf) return;
        var nextMuted = !user.IsServerMuted;
        try
        {
            var resp = await _client.ServerMuteUserAsync(user.Id, nextMuted);
            if (resp.Success)
            {
                user.IsServerMuted = nextMuted;
            }
            else
            {
                ShowBanner($"Couldn't {(nextMuted ? "server-mute" : "server-unmute")} {user.Name}: {resp.ErrorMessage}", isError: true);
            }
        }
        catch (Exception ex)
        {
            ShowBanner($"Couldn't update server mute for {user.Name}: {ex.Message}", isError: true);
        }
    }

    // ==========================================================================
    // Profile card (§6): click any roster name
    // ==========================================================================

    private void OpenProfile(User user)
    {
        var card = ProfileCard;
        card.SessionId = user.Id;
        card.IsSelf = user.IsSelf;
        card.Uuid = user.IsSelf ? _client?.UserUuid : _client?.UuidForSession(user.Id);
        card.Refresh(user);
        card.Error = "";
        card.Status = "";
        card.IsLocallyMuted = user.IsLocallyMuted;
        card.LocalVolume = (int)Math.Round((_client?.LocalVolumeForSession(user.Id) ?? 1f) * 100);
        card.BioDraft = user.IsSelf ? MyBio : "";

        var (trust, fingerprint) = KeyFor(card.Uuid, user.IsSelf);
        card.SetTrust(user.IsSelf ? TrustState.Verified : trust, fingerprint);
        card.CanCompareKeys = !user.IsSelf && fingerprint.Length > 0;

        card.VolumeSetter = v => _client?.SetLocalVolume(user.Id, v / 100f);
        card.ToggleMute = () => ToggleLocalMute(user);
        card.CompareKeys = () =>
        {
            card.Close();
            _ = OpenTrustSheet(card.Uuid);
        };
        card.SaveBio = SaveBioAsync;
        card.ChangeAvatar = ChangeAvatarAsync;
        card.RemoveAvatar = RemoveAvatarAsync;
        card.IsOpen = true;
    }

    /// <summary>
    /// Trust and the full fingerprint for a uuid, from the MLS group of the
    /// channel we're in — the authenticated source, never the server's roster.
    /// Empty fingerprint when they're not in our group.
    /// </summary>
    private (TrustState Trust, string Fingerprint) KeyFor(string? uuid, bool isSelf)
    {
        var mls = _client?.Mls;
        var channelId = SelectedChannel?.Id;
        if (mls == null || channelId == null || string.IsNullOrEmpty(uuid)) return (TrustState.Tofu, "");

        try
        {
            var member = mls.GroupMembers(channelId, isVoice: true)
                .FirstOrDefault(m => isSelf ? m.IsSelf : m.Identity == uuid);
            if (member == null) return (TrustState.Tofu, "");
            var fingerprint = AuraCoreMethods.IdentityFingerprint(member.IdentityKey);
            if (isSelf) return (TrustState.Verified, fingerprint);
            Trust.Observe(member.Identity, member.IdentityKey);
            return (Trust.Evaluate(member.Identity, member.IdentityKey), fingerprint);
        }
        catch
        {
            return (TrustState.Tofu, "");
        }
    }

    private async Task SaveBioAsync(string bio)
    {
        if (_client == null) return;
        try
        {
            ProfileCard.Status = "saving…";
            await _client.UpdateProfileAsync(DisplayName, bio, _myAvatar);
        }
        catch (Exception ex)
        {
            ProfileCard.Status = "";
            ProfileCard.Error = ex.Message;
        }
    }

    // ==========================================================================
    // Channel admin
    // ==========================================================================

    [RelayCommand]
    private void NewChannel() => ChannelEditor.OpenForCreate();

    private void WireChannelEditor()
    {
        ChannelEditor.Save = async (id, name, comment, emoji) =>
        {
            if (_client == null) return;
            if (id == null) await _client.CreateChannelAsync(name, comment, emoji);
            else await _client.UpdateChannelAsync(id, name, comment, emoji);
        };
        ChannelEditor.Delete = async id =>
        {
            if (_client != null) await _client.DeleteChannelAsync(id);
        };
    }

    /// <summary>The server's answer to an admin or profile request.</summary>
    private void HandleRequestResult(string action, bool ok, string error)
    {
        if (action == "delete user")
        {
            _ = HandleLeaveResultAsync(ok, error);
            return;
        }

        if (action == "update profile")
        {
            if (ProfileCard.IsOpen && ProfileCard.IsSelf)
            {
                ProfileCard.Status = ok ? "saved" : "";
                ProfileCard.Error = ok ? "" : error;
            }
            else if (!ok)
            {
                ShowBanner($"Couldn't update your profile: {error}", isError: true);
            }
            return;
        }

        if (ChannelEditor.IsOpen)
        {
            if (ok) ChannelEditor.Close();
            else ChannelEditor.Error = error;
        }
        else if (!ok)
        {
            ShowBanner($"Couldn't {action}: {error}", isError: true);
        }
    }

    // ==========================================================================
    // Trust state for roster glyphs and the rail
    // ==========================================================================

    private string _trustKey = "";

    /// <summary>
    /// Evaluate every member of the current channel's MLS group against the
    /// trust store. Cheap, but only redone when the group or roster changes.
    /// </summary>
    private void RefreshTrust(bool force)
    {
        var mls = _client?.Mls;
        var channel = SelectedChannel;
        if (mls == null || channel == null) return;

        List<MlsGroupMemberRecord> members;
        try
        {
            var epoch = mls.CurrentEpoch(channel.Id, isVoice: true);
            var key = $"{channel.Id}:{epoch}:{channel.Users.Count}";
            if (!force && key == _trustKey) return;
            _trustKey = key;
            members = mls.GroupMembers(channel.Id, isVoice: true).ToList();
        }
        catch
        {
            return; // Not in the group yet.
        }

        foreach (var user in channel.Users.Where(u => !u.IsSelf))
        {
            var uuid = _client!.UuidForSession(user.Id);
            var member = members.FirstOrDefault(m => !m.IsSelf && m.Identity == uuid);
            if (member == null) continue;
            Trust.Observe(member.Identity, member.IdentityKey);
            user.Trust = Trust.Evaluate(member.Identity, member.IdentityKey);
        }
    }
}

/// <summary>
/// The profile / comment card (§6): avatar initial, name + trust pill, full
/// fingerprint, comment rendered as markdown, Local volume and Compare keys.
/// On your own card the comment is editable instead.
/// </summary>
public partial class ProfileCardViewModel : ObservableObject
{
    /// <summary>Comment budget from the handoff (§6 "Body 280 characters").</summary>
    public const int BioLimit = 280;

    [ObservableProperty] private bool _isOpen;
    [ObservableProperty] private string _name = "";
    [ObservableProperty] private string _bio = "";
    [ObservableProperty] private string _fingerprint = "";
    [ObservableProperty] private string _trustLabel = "";
    [ObservableProperty] private bool _isVerified;
    [ObservableProperty] private bool _isKeyChanged;
    [ObservableProperty] private bool _isSelf;
    [ObservableProperty] private bool _canCompareKeys;
    [ObservableProperty] private bool _isLocallyMuted;
    [ObservableProperty] private int _localVolume = 100;
    [ObservableProperty] private string _bioDraft = "";
    [ObservableProperty] private string _error = "";
    [ObservableProperty] private string _status = "";

    public uint SessionId { get; set; }
    public string? Uuid { get; set; }

    public string Initial => string.IsNullOrWhiteSpace(Name) ? "?" : Name.Trim()[..1].ToUpperInvariant();
    public bool HasBio => !string.IsNullOrWhiteSpace(Bio);
    public bool HasFingerprint => Fingerprint.Length > 0;
    public bool CanAdjust => !IsSelf;
    public string FingerprintOrHint => HasFingerprint ? Fingerprint : "join their channel to see their key";
    public string LocalMuteLabel => IsLocallyMuted ? "Unmute locally" : "Mute locally";
    public string LocalVolumeLabel => $"{LocalVolume}%";
    public string BioCountLabel => $"{BioDraft.Length}/{BioLimit}";
    public bool BioTooLong => BioDraft.Length > BioLimit;

    public Action<int>? VolumeSetter { get; set; }
    public Action? ToggleMute { get; set; }
    public Action? CompareKeys { get; set; }
    public Func<string, Task>? SaveBio { get; set; }

    public void Refresh(User user)
    {
        Name = user.IsSelf ? $"{user.Name} (you)" : user.Name;
        Bio = user.Comment;
        Avatar = user.Avatar;
    }

    public void SetTrust(TrustState trust, string fingerprint)
    {
        Fingerprint = fingerprint;
        IsVerified = trust == TrustState.Verified;
        IsKeyChanged = trust == TrustState.KeyChanged;
        TrustLabel = trust switch
        {
            TrustState.Verified => "verified",
            TrustState.KeyChanged => "key changed",
            TrustState.Blocked => "blocked",
            _ => "TOFU · not compared",
        };
    }

    partial void OnNameChanged(string value) => OnPropertyChanged(nameof(Initial));
    partial void OnBioChanged(string value) => OnPropertyChanged(nameof(HasBio));
    partial void OnFingerprintChanged(string value)
    {
        OnPropertyChanged(nameof(HasFingerprint));
        OnPropertyChanged(nameof(FingerprintOrHint));
    }
    partial void OnIsSelfChanged(bool value)
    {
        OnPropertyChanged(nameof(CanAdjust));
        OnPropertyChanged(nameof(CanRemoveAvatar));
    }
    partial void OnIsLocallyMutedChanged(bool value) => OnPropertyChanged(nameof(LocalMuteLabel));
    partial void OnBioDraftChanged(string value)
    {
        OnPropertyChanged(nameof(BioCountLabel));
        OnPropertyChanged(nameof(BioTooLong));
        Status = "";
    }

    partial void OnLocalVolumeChanged(int value)
    {
        OnPropertyChanged(nameof(LocalVolumeLabel));
        if (IsOpen) VolumeSetter?.Invoke(value);
    }

    [RelayCommand]
    public void Close() => IsOpen = false;

    [RelayCommand]
    private void ToggleLocalMute() => ToggleMute?.Invoke();

    [RelayCommand]
    private void Compare() => CompareKeys?.Invoke();

    [RelayCommand]
    private async Task SaveBioDraft()
    {
        if (BioTooLong)
        {
            Error = $"Keep it under {BioLimit} characters.";
            return;
        }
        Error = "";
        if (SaveBio != null) await SaveBio(BioDraft.Trim());
    }
}

/// <summary>Create / edit / delete a channel (admin).</summary>
public partial class ChannelEditorViewModel : ObservableObject
{
    [ObservableProperty] private bool _isOpen;
    [ObservableProperty] private string _name = "";
    [ObservableProperty] private string _comment = "";
    [ObservableProperty] private string _emoji = "";
    [ObservableProperty] private string _error = "";
    [ObservableProperty] private bool _confirmingDelete;
    [ObservableProperty] private bool _isBusy;

    private string? _channelId;

    public bool IsNew => _channelId == null;
    public string Title => IsNew ? "Create channel" : "Edit channel";
    public string DeleteLabel => ConfirmingDelete ? "Delete — this can't be undone" : "Delete channel";

    public Func<string?, string, string, string, Task>? Save { get; set; }
    public Func<string, Task>? Delete { get; set; }

    public void OpenForCreate()
    {
        _channelId = null;
        Name = "";
        Comment = "";
        Emoji = "";
        Open();
    }

    public void OpenForEdit(Channel channel, bool confirmDelete = false)
    {
        _channelId = channel.Id;
        Name = channel.Name;
        Comment = channel.Comment;
        Emoji = channel.Icon == "🔊" ? "" : channel.Icon;
        Open();
        ConfirmingDelete = confirmDelete;
    }

    private void Open()
    {
        Error = "";
        ConfirmingDelete = false;
        IsBusy = false;
        OnPropertyChanged(nameof(IsNew));
        OnPropertyChanged(nameof(Title));
        IsOpen = true;
    }

    partial void OnConfirmingDeleteChanged(bool value) => OnPropertyChanged(nameof(DeleteLabel));

    public void Close()
    {
        IsOpen = false;
        IsBusy = false;
        ConfirmingDelete = false;
    }

    [RelayCommand]
    private void Cancel() => Close();

    [RelayCommand]
    private async Task Submit()
    {
        if (string.IsNullOrWhiteSpace(Name))
        {
            Error = "A channel needs a name.";
            return;
        }
        if (Save == null) return;
        Error = "";
        IsBusy = true;
        try
        {
            // The editor closes when the server confirms (see HandleRequestResult).
            await Save(_channelId, Name.Trim(), Comment.Trim(), Emoji.Trim());
        }
        catch (Exception ex)
        {
            Error = ex.Message;
            IsBusy = false;
        }
    }

    [RelayCommand]
    private async Task DeleteChannel()
    {
        if (_channelId == null || Delete == null) return;
        if (!ConfirmingDelete)
        {
            ConfirmingDelete = true;
            return;
        }
        IsBusy = true;
        try
        {
            await Delete(_channelId);
        }
        catch (Exception ex)
        {
            Error = ex.Message;
            IsBusy = false;
        }
    }
}
