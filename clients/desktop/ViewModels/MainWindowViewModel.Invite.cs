using System;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>Invite links: making one for the current channel, and opening one.</summary>
public partial class MainWindowViewModel
{
    /// <summary>A link loaded but not yet connected with; carries the pin and channel.</summary>
    private AuraInviteLink? _pendingInvite;

    /// <summary>Set by the view; the VM has no clipboard of its own.</summary>
    public Func<Task<string?>>? ClipboardGetter { get; set; }

    // ── Sender: the "Invite" flyout in the channel header ──

    [ObservableProperty] private bool _inviteIncludeChannel = true;
    [ObservableProperty] private bool _inviteIncludePassword;
    [ObservableProperty] private string _inviteLink = "";
    [ObservableProperty] private bool _inviteHasFingerprint;
    [ObservableProperty] private string _inviteStatus = "";

    public string InviteChannelLabel => $"Open straight into {SelectedChannel?.Name ?? "this channel"}";

    /// <summary>Offered only when this session actually used a password.</summary>
    public bool CanIncludeInvitePassword => !string.IsNullOrEmpty(_sessionPassword);

    partial void OnInviteIncludeChannelChanged(bool value) => RefreshInvite();
    partial void OnInviteIncludePasswordChanged(bool value) => RefreshInvite();

    /// <summary>Rebuild the link from the current session.</summary>
    private void RefreshInvite()
    {
        OnPropertyChanged(nameof(InviteChannelLabel));
        InviteStatus = "";
        if (!IsAuthenticated || string.IsNullOrEmpty(_sessionHost))
        {
            InviteLink = "";
            InviteHasFingerprint = false;
            return;
        }

        // The fingerprint goes in only for a pinned (self-signed) server. A
        // CA-issued certificate renews every few months and would break every
        // link already shared.
        var pin = KnownServers.PinFor(_sessionHost);
        var fingerprint = pin is { Length: 64 } ? pin.ToLowerInvariant() : null;

        var link = new AuraInviteLink(
            _sessionHost,
            _sessionPort,
            InviteIncludeChannel ? SelectedChannel?.Id : null,
            fingerprint,
            InviteIncludePassword && CanIncludeInvitePassword ? _sessionPassword : null);

        InviteLink = link.ToString();
        InviteHasFingerprint = fingerprint != null;
    }

    [RelayCommand]
    private void OpenInvite() => RefreshInvite();

    [RelayCommand]
    private async Task CopyInviteAsync()
    {
        RefreshInvite();
        if (ClipboardSetter == null || string.IsNullOrEmpty(InviteLink)) return;
        await ClipboardSetter(InviteLink);
        InviteStatus = "copied";
    }

    // ── Receiver ──

    /// <summary>
    /// Load an invite: prefill the connect form, remember the channel and the
    /// certificate pin. Connecting stays an explicit step.
    /// Returns false (with a banner) for something that isn't a valid link.
    /// </summary>
    public bool ApplyInviteLink(string? text)
    {
        if (!AuraInviteLink.TryParse(text, out var link, out var error) || link == null)
        {
            ShowBanner($"That isn't a usable invite link: {error}.", isError: true);
            return false;
        }

        _pendingInvite = link;
        _applyingInvite = true;
        ServerAddress = link.Host;
        _applyingInvite = false;
        ServerPort = link.Port;
        ServerPassword = link.Password ?? "";
        _rejoinChannelId = link.ChannelId;

        var where = link.Port == AuraInviteLink.DefaultPort ? link.Host : $"{link.Host}:{link.Port}";
        ShowBanner(IsConnected
            ? $"Invite to {where} loaded. Disconnect, then Connect to use it."
            : $"Invite to {where} loaded. Press Connect to join.", isError: false);
        return true;
    }

    [RelayCommand]
    private async Task PasteInviteAsync()
    {
        var text = ClipboardGetter == null ? null : await ClipboardGetter();
        if (string.IsNullOrWhiteSpace(text))
        {
            ShowBanner("The clipboard is empty. Copy the invite link first.", isError: true);
            return;
        }
        ApplyInviteLink(text);
    }

    private bool _applyingInvite;

    partial void OnServerAddressChanged(string value)
    {
        if (_applyingInvite) return;

        // A pasted aura:// link in the server field is an invite.
        if (AuraInviteLink.LooksLikeInvite(value))
        {
            ApplyInviteLink(value);
            return;
        }

        // Typing a different host drops a loaded invite and its pin.
        if (_pendingInvite != null && !SameHost(_pendingInvite.Host, value))
        {
            _pendingInvite = null;
        }
    }
}
