using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>The admin moderation console, opened over the window from the roster.</summary>
public partial class MainWindowViewModel
{
    public ModerationViewModel Moderation => _moderation ??= new ModerationViewModel(
        api: () => _client,
        selfUuid: () => _client?.UserUuid,
        presence: CurrentPresence)
    {
        HostLabel = HostLabelFor(_sessionHost),
    };

    private ModerationViewModel? _moderation;

    private static string HostLabelFor(string host) =>
        string.IsNullOrWhiteSpace(host) ? "ADMIN" : $"{host.Trim().ToUpperInvariant()} · ADMIN";

    /// <summary>
    /// Where each account we can see is online, from the roster the server
    /// keeps us updated on. Accounts are matched by uuid; the session ids the
    /// roster is keyed on change with every connection.
    /// </summary>
    private IReadOnlyDictionary<string, OnlinePresence> CurrentPresence()
    {
        if (_client == null) return new Dictionary<string, OnlinePresence>();
        return ModerationLogic.PresenceMap(Channels.SelectMany(channel =>
        {
            var where = new OnlinePresence(channel.Id, channel.Name, channel.IsLobby);
            return channel.Users.Select(user =>
                (user.IsSelf ? _client.UserUuid : _client.UuidForSession(user.Id), where));
        }));
    }

    /// <summary>The roster-header button: open the console with nobody selected.</summary>
    [RelayCommand]
    private Task OpenModeration()
    {
        if (!IsAdmin || _client == null) return Task.CompletedTask;
        Moderation.HostLabel = HostLabelFor(_sessionHost);
        return Moderation.OpenAsync();
    }

    /// <summary>The roster context menu: open the console on one member.</summary>
    private Task OpenModerationFor(User user)
    {
        if (!IsAdmin || _client == null || user.IsSelf) return Task.CompletedTask;
        Moderation.HostLabel = HostLabelFor(_sessionHost);
        return Moderation.OpenAsync(_client.UuidForSession(user.Id), user.Name);
    }
}
