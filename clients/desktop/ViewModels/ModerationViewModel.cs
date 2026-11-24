using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using Aura.V1Alpha1;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>One row of the console's member table.</summary>
public partial class ModerationRowViewModel : ObservableObject
{
    public ModerationRowViewModel(ModMember member, bool isSelf)
    {
        Member = member;
        IsSelf = isSelf;
    }

    public ModMember Member { get; }
    public bool IsSelf { get; }

    [ObservableProperty] private bool _isSelected;

    public string Name => IsSelf ? $"{Member.Name} (you)" : Member.Name;
    public string Initial => ModerationLogic.Initial(Member.Name);
    public string ShortId => ModerationLogic.ShortId(Member.Uuid);
    public Standing Standing => ModerationLogic.StandingOf(Member);
    public string StandingLabel => ModerationLogic.StandingLabel(Standing);
    public string Status => ModerationLogic.StatusText(Member);

    public bool IsBanned => Standing == Standing.Banned;
    public bool IsAdminStanding => Standing == Standing.Admin;
    public bool IsVerifiedStanding => Standing == Standing.Verified;
    public bool IsUnverifiedStanding => Standing == Standing.Unverified;
    public bool IsOnline => Member.IsOnline;
}

/// <summary>
/// The admin moderation console: a searchable, filterable table of every
/// account on the server and a panel of actions for the selected one. Shown as
/// a sheet over the window, like the trust sheet.
///
/// The client doesn't learn which permissions its own admin account holds, so
/// every action is offered and the server's refusal is shown inline. All
/// requests go through <see cref="IModerationApi"/>, and the list is simply
/// re-fetched after each change so the table is always what the server holds.
/// </summary>
public partial class ModerationViewModel : ObservableObject
{
    private readonly Func<IModerationApi?> _api;
    private readonly Func<string?> _selfUuid;
    private readonly Func<IReadOnlyDictionary<string, OnlinePresence>> _presence;
    private readonly Func<DateTimeOffset> _now;

    private List<ModMember> _members = new();
    private CancellationTokenSource? _loadCts;

    public ModerationViewModel(
        Func<IModerationApi?> api,
        Func<string?> selfUuid,
        Func<IReadOnlyDictionary<string, OnlinePresence>> presence,
        Func<DateTimeOffset>? now = null)
    {
        _api = api;
        _selfUuid = selfUuid;
        _presence = presence;
        _now = now ?? (() => DateTimeOffset.UtcNow);
    }

    [ObservableProperty] private bool _isOpen;
    [ObservableProperty] private bool _isLoading;

    /// <summary>"AURA.EXAMPLE · ADMIN".</summary>
    [ObservableProperty] private string _hostLabel = "ADMIN";

    /// <summary>A failure of the list itself (not of an action on a member).</summary>
    [ObservableProperty] private string _listError = "";
    public bool HasListError => !string.IsNullOrEmpty(ListError);
    partial void OnListErrorChanged(string value) => OnPropertyChanged(nameof(HasListError));

    public ObservableCollection<ModerationRowViewModel> Rows { get; } = new();

    [ObservableProperty] private string _search = "";
    [ObservableProperty] private MemberFilter _filter = MemberFilter.All;

    [ObservableProperty] private string _allLabel = "all · 0";
    [ObservableProperty] private string _onlineLabel = "online · 0";
    [ObservableProperty] private string _awaitingLabel = "awaiting verification · 0";
    [ObservableProperty] private string _bannedLabel = "banned · 0";

    public bool FilterIsAll => Filter == MemberFilter.All;
    public bool FilterIsOnline => Filter == MemberFilter.Online;
    public bool FilterIsAwaiting => Filter == MemberFilter.Awaiting;
    public bool FilterIsBanned => Filter == MemberFilter.Banned;

    public bool ShowEmpty => !IsLoading && Rows.Count == 0 && !HasListError;
    public string EmptyText => _members.Count == 0 ? "No accounts yet." : "No members match.";

    partial void OnSearchChanged(string value) => Rebuild();

    partial void OnFilterChanged(MemberFilter value)
    {
        OnPropertyChanged(nameof(FilterIsAll));
        OnPropertyChanged(nameof(FilterIsOnline));
        OnPropertyChanged(nameof(FilterIsAwaiting));
        OnPropertyChanged(nameof(FilterIsBanned));
        Rebuild();
    }

    partial void OnIsLoadingChanged(bool value) => OnPropertyChanged(nameof(ShowEmpty));

    [RelayCommand]
    private void SetFilter(MemberFilter filter) => Filter = filter;

    // ------------------------------------------------------------------------
    // Opening, loading
    // ------------------------------------------------------------------------

    /// <summary>
    /// Open the console, optionally with one account selected. A uuid selects
    /// it once the list arrives; a name (the roster knew no uuid yet) is put
    /// in the search box instead.
    /// </summary>
    public async Task OpenAsync(string? focusUuid = null, string? focusName = null)
    {
        _selectedUuid = focusUuid;
        Search = string.IsNullOrEmpty(focusUuid) && !string.IsNullOrWhiteSpace(focusName) ? focusName : "";
        Filter = MemberFilter.All;
        ResetPanel();
        IsOpen = true;
        await ReloadAsync();
    }

    [RelayCommand]
    private void Close()
    {
        _loadCts?.Cancel();
        IsOpen = false;
        ResetPanel();
    }

    [RelayCommand]
    public async Task ReloadAsync()
    {
        var api = _api();
        if (api == null)
        {
            ListError = "Not connected.";
            return;
        }

        _loadCts?.Cancel();
        var cts = _loadCts = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        IsLoading = true;
        ListError = "";
        try
        {
            var (users, error) = await ModerationLogic.FetchAllAsync(api, cts.Token);
            if (cts.IsCancellationRequested) return;
            if (error != null)
            {
                ListError = error == "Admin required" ? "Only admins can see the member list." : error;
                return;
            }
            var online = _presence();
            _members = users.Select(u => ModMember.From(u, online)).ToList();
            Rebuild();
        }
        catch (OperationCanceledException)
        {
            if (ReferenceEquals(_loadCts, cts)) ListError = "The server didn't answer in time.";
        }
        catch (Exception ex)
        {
            ListError = ex.Message;
        }
        finally
        {
            if (ReferenceEquals(_loadCts, cts)) IsLoading = false;
        }
    }

    /// <summary>Re-evaluate who is online against the current roster, without asking the server.</summary>
    public void RefreshPresence()
    {
        if (!IsOpen || _members.Count == 0) return;
        var online = _presence();
        _members = _members.Select(m => m with { Presence = online.TryGetValue(m.Uuid, out var p) ? p : null }).ToList();
        Rebuild();
    }

    private string? _selectedUuid;

    /// <summary>Rebuild the visible rows from the loaded accounts, the filter and the search box.</summary>
    private void Rebuild()
    {
        var counts = ModerationLogic.Count(_members);
        AllLabel = $"all · {counts.All}";
        OnlineLabel = $"online · {counts.Online}";
        AwaitingLabel = $"awaiting verification · {counts.Awaiting}";
        BannedLabel = $"banned · {counts.Banned}";

        var self = _selfUuid();
        Rows.Clear();
        foreach (var m in ModerationLogic.Apply(_members, Filter, Search))
        {
            Rows.Add(new ModerationRowViewModel(m, m.Uuid == self) { IsSelected = m.Uuid == _selectedUuid });
        }

        // Keep the panel on the same account across reloads; drop it if the
        // account is gone or filtered out of view.
        var row = Rows.FirstOrDefault(r => r.Member.Uuid == _selectedUuid);
        if (!ReferenceEquals(row, Selected)) SetSelected(row, keepPanelState: row != null && Selected?.Member.Uuid == row.Member.Uuid);
        OnPropertyChanged(nameof(ShowEmpty));
        OnPropertyChanged(nameof(EmptyText));
    }

    // ------------------------------------------------------------------------
    // Selection and the detail panel
    // ------------------------------------------------------------------------

    [ObservableProperty] private ModerationRowViewModel? _selected;

    public bool HasSelection => Selected != null;
    public string SelectedName => Selected?.Member.Name ?? "";
    public string SelectedShortId => Selected?.ShortId ?? "";
    public string SelectedJoined => Selected == null ? "" : ModerationLogic.JoinedText(Selected.Member.CreatedAt, _now());
    public string SelectedStatus => Selected?.Status ?? "";
    public string SelectedStandingLabel => Selected?.StandingLabel ?? "";

    public bool SelectedIsSelf => Selected?.IsSelf == true;
    public bool SelectedIsBanned => Selected?.Member.Banned == true;
    public bool SelectedIsAdmin => Selected?.Member.IsAdmin == true;

    public bool CanVerify => Selected != null && !Busy && ModerationLogic.CanVerify(Selected.Member);
    public bool CanMoveToLobby => Selected != null && !Busy && ModerationLogic.CanMoveToLobby(Selected.Member, Selected.IsSelf);
    public bool CanBan => Selected != null && !Busy && ModerationLogic.CanBan(Selected.Member, Selected.IsSelf);
    public bool CanUnban => Selected != null && !Busy && ModerationLogic.CanUnban(Selected.Member, Selected.IsSelf);
    public bool CanOpenGrant => Selected != null && !Busy && !Selected.IsSelf;
    public bool CanApplyGrant => Selected != null && !Busy && ModerationLogic.CanApplyGrant(Selected.Member.IsAdmin, Permissions, Selected.IsSelf);

    public string MoveToLobbyHint => Selected == null ? "" :
        Selected.IsSelf ? "That's you."
        : !Selected.Member.IsOnline ? "Offline: nothing to move."
        : Selected.Member.Presence!.IsLobby ? "Already in the lobby."
        : "";

    public string BanButtonLabel => $"Ban {SelectedName}";
    public string GrantButtonLabel => SelectedIsAdmin ? "Admin permissions…" : "Make admin…";
    public string GrantApplyLabel => ModerationLogic.GrantLabel(SelectedIsAdmin, Permissions);

    partial void OnSelectedChanged(ModerationRowViewModel? value)
    {
        foreach (var name in new[]
        {
            nameof(HasSelection), nameof(SelectedName), nameof(SelectedShortId), nameof(SelectedJoined),
            nameof(SelectedStatus), nameof(SelectedStandingLabel), nameof(SelectedIsSelf), nameof(SelectedIsBanned),
            nameof(SelectedIsAdmin), nameof(MoveToLobbyHint), nameof(BanButtonLabel), nameof(GrantButtonLabel),
        })
        {
            OnPropertyChanged(name);
        }
        NotifyActions();
    }

    private void NotifyActions()
    {
        foreach (var name in new[]
        {
            nameof(CanVerify), nameof(CanMoveToLobby), nameof(CanBan), nameof(CanUnban),
            nameof(CanOpenGrant), nameof(CanApplyGrant), nameof(GrantApplyLabel),
        })
        {
            OnPropertyChanged(name);
        }
    }

    private void SetSelected(ModerationRowViewModel? row, bool keepPanelState)
    {
        if (Selected != null) Selected.IsSelected = false;
        if (row != null) row.IsSelected = true;
        _selectedUuid = row?.Member.Uuid;
        Selected = row;
        if (!keepPanelState) ResetPanel();
    }

    [RelayCommand]
    private void Select(ModerationRowViewModel? row)
    {
        if (row == null || ReferenceEquals(row, Selected)) return;
        SetSelected(row, keepPanelState: false);
    }

    // ------------------------------------------------------------------------
    // Actions
    // ------------------------------------------------------------------------

    [ObservableProperty] private bool _busy;

    /// <summary>The server's refusal (or a transport failure) for the last action, shown in the panel.</summary>
    [ObservableProperty] private string _actionError = "";
    public bool HasActionError => !string.IsNullOrEmpty(ActionError);
    partial void OnActionErrorChanged(string value) => OnPropertyChanged(nameof(HasActionError));

    /// <summary>A short confirmation of the last action ("verified", "banned").</summary>
    [ObservableProperty] private string _actionStatus = "";
    public bool HasActionStatus => !string.IsNullOrEmpty(ActionStatus);
    partial void OnActionStatusChanged(string value) => OnPropertyChanged(nameof(HasActionStatus));

    [ObservableProperty] private string _banReason = "";
    [ObservableProperty] private string _kickReason = "";
    [ObservableProperty] private bool _confirmingBan;

    partial void OnBusyChanged(bool value) => NotifyActions();

    // The admin-permissions panel ("Make admin…").
    [ObservableProperty] private bool _showGrant;
    [ObservableProperty] private bool _grantVerifyUsers;
    [ObservableProperty] private bool _grantBanUsers;
    [ObservableProperty] private bool _grantGrantAdmin;
    [ObservableProperty] private bool _grantManageChannels;

    private AdminPermissions Permissions =>
        new(GrantVerifyUsers, GrantBanUsers, GrantGrantAdmin, GrantManageChannels);

    partial void OnGrantVerifyUsersChanged(bool value) => NotifyActions();
    partial void OnGrantBanUsersChanged(bool value) => NotifyActions();
    partial void OnGrantGrantAdminChanged(bool value) => NotifyActions();
    partial void OnGrantManageChannelsChanged(bool value) => NotifyActions();

    /// <summary>Clear everything that belongs to one account's panel.</summary>
    private void ResetPanel()
    {
        ActionError = "";
        ActionStatus = "";
        BanReason = "";
        KickReason = "";
        ConfirmingBan = false;
        ShowGrant = false;
        // The client doesn't learn a target's existing permissions, so the
        // checkboxes always start all-off.
        GrantVerifyUsers = GrantBanUsers = GrantGrantAdmin = GrantManageChannels = false;
    }

    private async Task<bool> RunAsync(string done, Func<IModerationApi, string, Task<AdminResponse>> call)
    {
        var api = _api();
        var target = Selected;
        if (api == null || target == null || Busy) return false;

        Busy = true;
        ActionError = "";
        ActionStatus = "";
        try
        {
            var reply = await call(api, target.Member.Uuid);
            if (!reply.Success)
            {
                ActionError = string.IsNullOrWhiteSpace(reply.ErrorMessage) ? "The server refused the request." : reply.ErrorMessage;
                return false;
            }
        }
        catch (OperationCanceledException)
        {
            ActionError = "The server didn't answer in time.";
            return false;
        }
        catch (Exception ex)
        {
            ActionError = ex.Message;
            return false;
        }
        finally
        {
            Busy = false;
        }

        await ReloadAsync();
        ActionStatus = done;
        return true;
    }

    [RelayCommand]
    private Task VerifyAsync() => RunAsync("verified", (api, uuid) => api.VerifyUserAsync(uuid));

    [RelayCommand]
    private async Task MoveToLobbyAsync()
    {
        if (!CanMoveToLobby) return;
        if (await RunAsync("moved to the lobby", (api, uuid) => api.KickUserAsync(uuid, KickReason.Trim()))) KickReason = "";
    }

    /// <summary>First press arms the confirmation, second sends the ban.</summary>
    [RelayCommand]
    private async Task BanAsync()
    {
        if (!CanBan) return;
        if (!ConfirmingBan)
        {
            ConfirmingBan = true;
            return;
        }

        if (await RunAsync("banned", (api, uuid) => api.BanUserAsync(uuid, true, BanReason.Trim()))) BanReason = "";
        ConfirmingBan = false;
    }

    [RelayCommand]
    private void CancelBan() => ConfirmingBan = false;

    [RelayCommand]
    private async Task UnbanAsync()
    {
        if (!CanUnban) return;
        if (await RunAsync("unbanned", (api, uuid) => api.BanUserAsync(uuid, false, BanReason.Trim()))) BanReason = "";
    }

    [RelayCommand]
    private void ToggleGrant()
    {
        if (!CanOpenGrant) return;
        ShowGrant = !ShowGrant;
    }

    [RelayCommand]
    private async Task ApplyGrantAsync()
    {
        if (!CanApplyGrant) return;
        var permissions = Permissions;
        var removing = permissions.None;
        if (await RunAsync(removing ? "admin removed" : "admin permissions set",
                (api, uuid) => api.GrantAdminAsync(uuid, permissions)))
        {
            ShowGrant = false;
        }
    }

    partial void OnShowGrantChanged(bool value) => NotifyActions();
}
