using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using Aura.Desktop.ViewModels;
using Aura.V1Alpha1;
using Google.Protobuf;
using Xunit;

namespace Aura.Desktop.Tests;

public class ModerationLogicTests
{
    private static readonly OnlinePresence Lobby = new("c-lobby", "Lobby", true);
    private static readonly OnlinePresence Games = new("c-games", "Games", false);

    private static ModMember M(string name, string uuid = "", bool verified = false, bool banned = false,
        bool admin = false, OnlinePresence? at = null, long created = 0) =>
        new(string.IsNullOrEmpty(uuid) ? $"U_{name}" : uuid, name, verified, banned, admin, created, at);

    private static readonly ModMember[] Sample =
    {
        M("alice", verified: true, admin: true, at: Lobby),
        M("bob", verified: true, at: Games),
        M("carol"),
        M("dave", at: Games),
        M("erin", banned: true),
        M("frank", verified: true, banned: true),
    };

    [Fact]
    public void StandingBanOutranksAdminOutranksVerified()
    {
        Assert.Equal(Standing.Banned, ModerationLogic.StandingOf(M("a", verified: true, admin: true, banned: true)));
        Assert.Equal(Standing.Admin, ModerationLogic.StandingOf(M("a", verified: true, admin: true)));
        Assert.Equal(Standing.Verified, ModerationLogic.StandingOf(M("a", verified: true)));
        Assert.Equal(Standing.Unverified, ModerationLogic.StandingOf(M("a")));
        Assert.Equal("unverified", ModerationLogic.StandingLabel(Standing.Unverified));
        Assert.Equal("banned", ModerationLogic.StandingLabel(Standing.Banned));
    }

    [Fact]
    public void CountsMatchTheChips()
    {
        var c = ModerationLogic.Count(Sample);
        Assert.Equal(new FilterCounts(All: 6, Online: 3, Awaiting: 2, Banned: 2), c);
    }

    [Fact]
    public void BannedAccountsAreNotAwaitingVerification()
    {
        Assert.False(ModerationLogic.AwaitingVerification(M("erin", banned: true)));
        Assert.True(ModerationLogic.AwaitingVerification(M("carol")));
    }

    [Theory]
    [InlineData(MemberFilter.All, new[] { "alice", "bob", "carol", "dave", "erin", "frank" })]
    [InlineData(MemberFilter.Online, new[] { "alice", "bob", "dave" })]
    [InlineData(MemberFilter.Awaiting, new[] { "carol", "dave" })]
    [InlineData(MemberFilter.Banned, new[] { "erin", "frank" })]
    public void FilterSelectsRows(MemberFilter filter, string[] expected) =>
        Assert.Equal(expected, ModerationLogic.Apply(Sample, filter, "").Select(m => m.Name));

    [Fact]
    public void SearchMatchesNameOrUserIdIgnoringCase()
    {
        Assert.Equal(new[] { "carol" }, ModerationLogic.Apply(Sample, MemberFilter.All, "  CAR ").Select(m => m.Name));
        Assert.Equal(new[] { "bob" }, ModerationLogic.Apply(Sample, MemberFilter.All, "u_bob").Select(m => m.Name));
        Assert.Empty(ModerationLogic.Apply(Sample, MemberFilter.All, "zzz"));
        Assert.Equal(6, ModerationLogic.Apply(Sample, MemberFilter.All, null).Count);
    }

    [Fact]
    public void SearchAndFilterCombine() =>
        Assert.Equal(new[] { "dave" }, ModerationLogic.Apply(Sample, MemberFilter.Awaiting, "d").Select(m => m.Name));

    [Fact]
    public void ShortIdKeepsEndsOfLongIdsOnly()
    {
        Assert.Equal("U_short", ModerationLogic.ShortId("U_short"));
        Assert.Equal("U_3f8a1c…5503", ModerationLogic.ShortId("U_3f8a1c77e0b14d2c9a5503"));
    }

    [Fact]
    public void StatusNamesTheChannelOrSaysOffline()
    {
        Assert.Equal("in Games", ModerationLogic.StatusText(M("bob", at: Games)));
        Assert.Equal("offline", ModerationLogic.StatusText(M("carol")));
    }

    [Fact]
    public void PresenceMapMatchesByUuidAndIgnoresUnknownSessions()
    {
        var map = ModerationLogic.PresenceMap(new (string?, OnlinePresence)[]
        {
            ("U_a", Lobby), (null, Games), ("", Games), ("U_b", Games),
        });
        Assert.Equal(2, map.Count);
        Assert.Equal(Lobby, map["U_a"]);

        var user = new UserInfo { UserUuid = "U_b", DisplayName = "bob" };
        Assert.Equal("in Games", ModerationLogic.StatusText(ModMember.From(user, map)));
        user.UserUuid = "U_z";
        Assert.Equal("offline", ModerationLogic.StatusText(ModMember.From(user, map)));
    }

    [Fact]
    public void JoinedTextIsRelative()
    {
        var now = new DateTimeOffset(2026, 11, 23, 12, 0, 0, TimeSpan.Zero);
        string At(TimeSpan ago) => ModerationLogic.JoinedText(now.Subtract(ago).ToUnixTimeSeconds(), now);

        Assert.Equal("joined just now", At(TimeSpan.FromSeconds(10)));
        Assert.Equal("joined 5 minutes ago", At(TimeSpan.FromMinutes(5)));
        Assert.Equal("joined 1 hour ago", At(TimeSpan.FromHours(1.5)));
        Assert.Equal("joined 3 days ago", At(TimeSpan.FromDays(3)));
        Assert.Equal("joined 2 months ago", At(TimeSpan.FromDays(65)));
        Assert.Equal("joined Jan 2025", ModerationLogic.JoinedText(
            new DateTimeOffset(2025, 1, 10, 0, 0, 0, TimeSpan.Zero).ToUnixTimeSeconds(), now));
        Assert.Equal("joined date unknown", ModerationLogic.JoinedText(0, now));
    }

    [Fact]
    public void MoveToLobbyNeedsAnOnlineMemberOutsideTheLobby()
    {
        Assert.True(ModerationLogic.CanMoveToLobby(M("bob", at: Games), isSelf: false));
        Assert.False(ModerationLogic.CanMoveToLobby(M("bob", at: Lobby), isSelf: false));
        Assert.False(ModerationLogic.CanMoveToLobby(M("bob"), isSelf: false));
        Assert.False(ModerationLogic.CanMoveToLobby(M("me", at: Games), isSelf: true));
    }

    [Fact]
    public void DestructiveActionsAreOffOnYourOwnAccount()
    {
        var banned = M("x", banned: true);
        Assert.False(ModerationLogic.CanBan(M("me"), isSelf: true));
        Assert.True(ModerationLogic.CanBan(M("x"), isSelf: false));
        Assert.False(ModerationLogic.CanBan(banned, isSelf: false));
        Assert.True(ModerationLogic.CanUnban(banned, isSelf: false));
        Assert.False(ModerationLogic.CanUnban(banned, isSelf: true));
        Assert.False(ModerationLogic.CanApplyGrant(true, default, isSelf: true));
    }

    [Fact]
    public void GrantNothingRevokesAdminAndIsLabelledSo()
    {
        Assert.Equal("Remove admin", ModerationLogic.GrantLabel(true, default));
        Assert.Equal("Update admin", ModerationLogic.GrantLabel(true, new AdminPermissions(true, false, false, false)));
        Assert.Equal("Make admin", ModerationLogic.GrantLabel(false, new AdminPermissions(false, false, false, true)));
        Assert.True(ModerationLogic.CanApplyGrant(true, default, isSelf: false));
        Assert.False(ModerationLogic.CanApplyGrant(false, default, isSelf: false));
        Assert.True(ModerationLogic.CanApplyGrant(false, new AdminPermissions(true, false, false, false), isSelf: false));
    }

    [Fact]
    public void KickNoticeAppendsTheAdminsReason()
    {
        Assert.Equal("An admin moved you to the lobby", ModerationLogic.KickNotice(""));
        Assert.Equal("An admin moved you to the lobby", ModerationLogic.KickNotice("   "));
        Assert.Equal("An admin moved you to the lobby: too loud", ModerationLogic.KickNotice(" too loud "));
    }

    [Fact]
    public void RemovalNoticesPerReason()
    {
        string Notice(ChannelRemovalKind k, string d = "") =>
            ModerationLogic.RemovalNotice(new ChannelRemoval("c", "lobby", k, d));
        Assert.Equal("The channel you were in was deleted", Notice(ChannelRemovalKind.Deleted));
        Assert.Equal("That channel no longer exists", Notice(ChannelRemovalKind.NotFound));
        Assert.Equal("An admin moved you to the lobby: spam", Notice(ChannelRemovalKind.Kicked, "spam"));
    }

    [Fact]
    public void BannedLoginIsRecognised()
    {
        Assert.True(ModerationLogic.IsBannedLogin("User is banned"));
        Assert.True(ModerationLogic.IsBannedLogin(" user is banned "));
        Assert.False(ModerationLogic.IsBannedLogin("Invalid signature"));
        Assert.False(ModerationLogic.IsBannedLogin(null));
    }

    [Fact]
    public void OrdinaryFailuresAreNotABanClose()
    {
        Assert.False(ModerationLogic.IsBanClose(null));
        Assert.False(ModerationLogic.IsBanClose(new System.IO.EndOfStreamException()));
        Assert.False(ModerationLogic.IsBanClose(new InvalidOperationException("x", new TimeoutException())));
    }
}

public class ModerationFramingTests
{
    [Fact]
    public void RequestsEncodeAndFrameWithTheirOwnType()
    {
        var req = new BanUserRequest { TargetUserUuid = "U_x", Banned = true, Reason = "spam" };
        var frame = ControlFrames.Frame(0x4A, req.ToByteArray());

        Assert.Equal(0x4A, frame[0]);
        Assert.Equal((uint)(frame.Length - 5), BitConverter.ToUInt32(frame, 1)); // little-endian host assumed by tests
        var back = BanUserRequest.Parser.ParseFrom(frame.AsSpan(5).ToArray());
        Assert.Equal("U_x", back.TargetUserUuid);
        Assert.True(back.Banned);
        Assert.Equal("spam", back.Reason);
    }

    [Fact]
    public void GrantAdminCarriesAllFourFlags()
    {
        var req = new GrantAdminRequest
        {
            TargetUserUuid = "U_x", VerifyUsers = true, BanUsers = false, GrantAdmin = true, ManageChannels = false,
        };
        var back = GrantAdminRequest.Parser.ParseFrom(req.ToByteArray());
        Assert.True(back.VerifyUsers);
        Assert.False(back.BanUsers);
        Assert.True(back.GrantAdmin);
        Assert.False(back.ManageChannels);
    }

    [Fact]
    public async Task ListUsersReplyReadsBackThroughTheFrameReader()
    {
        var reply = new ListUsersResponse { Success = true, TotalCount = 2 };
        reply.Users.Add(new UserInfo { UserUuid = "U_a", DisplayName = "a", IsAdmin = true, CreatedAt = 1700000000 });
        reply.Users.Add(new UserInfo { UserUuid = "U_b", DisplayName = "b", Banned = true });
        using var stream = new System.IO.MemoryStream(ControlFrames.Frame(0x48, reply.ToByteArray()));

        var type = new byte[1];
        await ControlFrames.ReadExactAsync(stream, type, CancellationToken.None);
        Assert.Equal(0x48, type[0]);
        var payload = await ControlFrames.ReadPayloadAsync(stream, 1 << 20, CancellationToken.None);
        var back = ListUsersResponse.Parser.ParseFrom(payload);
        Assert.Equal(2u, back.TotalCount);
        Assert.True(back.Users[0].IsAdmin);
        Assert.True(back.Users[1].Banned);
    }

    [Fact]
    public void ChannelDeletedNoticeKeepsReasonAndDetail()
    {
        var notice = new ChannelDeleted
        {
            ChannelId = "c1", FallbackChannelId = "lobby",
            Reason = ChannelRemovalReason.Kicked, Detail = "spam",
        };
        var back = ChannelDeleted.Parser.ParseFrom(notice.ToByteArray());
        Assert.Equal(ChannelRemovalReason.Kicked, back.Reason);
        Assert.Equal("spam", back.Detail);
        // An older server's notice has no reason: it reads as a deletion.
        Assert.Equal(ChannelRemovalReason.Deleted, ChannelDeleted.Parser.ParseFrom(new ChannelDeleted { ChannelId = "c" }.ToByteArray()).Reason);
    }
}

public class ModerationFetchAndViewModelTests
{
    private sealed class FakeApi : IModerationApi
    {
        public List<UserInfo> Accounts = new();
        public List<(uint Offset, uint Limit)> Pages = new();
        public string? Refuse;
        public List<string> Calls = new();
        public AdminResponse Reply = new() { Success = true };

        public Task<ListUsersResponse> ListUsersAsync(uint offset, uint limit, CancellationToken ct = default)
        {
            Pages.Add((offset, limit));
            if (Refuse != null) return Task.FromResult(new ListUsersResponse { Success = false, ErrorMessage = Refuse });
            var r = new ListUsersResponse { Success = true, TotalCount = (uint)Accounts.Count };
            r.Users.AddRange(Accounts.Skip((int)offset).Take((int)Math.Min(limit, 2))); // server pages smaller than asked
            return Task.FromResult(r);
        }

        public Task<AdminResponse> VerifyUserAsync(string u, CancellationToken ct = default) { Calls.Add($"verify {u}"); return Task.FromResult(Reply); }
        public Task<AdminResponse> BanUserAsync(string u, bool b, string r, CancellationToken ct = default) { Calls.Add($"ban {u} {b} '{r}'"); return Task.FromResult(Reply); }
        public Task<AdminResponse> GrantAdminAsync(string u, AdminPermissions p, CancellationToken ct = default) { Calls.Add($"grant {u} {p}"); return Task.FromResult(Reply); }
        public Task<AdminResponse> KickUserAsync(string u, string r, CancellationToken ct = default) { Calls.Add($"kick {u} '{r}'"); return Task.FromResult(Reply); }
    }

    private static List<UserInfo> Accounts(int n) =>
        Enumerable.Range(0, n).Select(i => new UserInfo { UserUuid = $"U_{i:D4}", DisplayName = $"user{i}" }).ToList();

    [Fact]
    public async Task FetchAllPagesUntilTotalCountIsReached()
    {
        var api = new FakeApi { Accounts = Accounts(5) };
        var (users, error) = await ModerationLogic.FetchAllAsync(api, CancellationToken.None);

        Assert.Null(error);
        Assert.Equal(5, users.Count);
        Assert.Equal(new uint[] { 0, 2, 4 }, api.Pages.Select(p => p.Offset));
        Assert.All(api.Pages, p => Assert.Equal(500u, p.Limit));
    }

    [Fact]
    public async Task FetchAllSurfacesARefusal()
    {
        var api = new FakeApi { Accounts = Accounts(3), Refuse = "Admin required" };
        var (users, error) = await ModerationLogic.FetchAllAsync(api, CancellationToken.None);
        Assert.Empty(users);
        Assert.Equal("Admin required", error);
    }

    private static ModerationViewModel Console(FakeApi api, string self = "U_0000",
        Dictionary<string, OnlinePresence>? online = null) =>
        new(() => api, () => self, () => online ?? new Dictionary<string, OnlinePresence>());

    [Fact]
    public async Task OpenLoadsCountsAndSelectsTheRequestedAccount()
    {
        var api = new FakeApi { Accounts = Accounts(3) };
        api.Accounts[1].Verified = true;
        api.Accounts[2].Banned = true;
        var vm = Console(api);

        await vm.OpenAsync("U_0001");

        Assert.True(vm.IsOpen);
        Assert.Equal(3, vm.Rows.Count);
        Assert.Equal("all · 3", vm.AllLabel);
        Assert.Equal("awaiting verification · 1", vm.AwaitingLabel);
        Assert.Equal("banned · 1", vm.BannedLabel);
        Assert.Equal("user1", vm.SelectedName);
        Assert.True(vm.Rows.Single(r => r.IsSelf).Member.Uuid == "U_0000");
    }

    [Fact]
    public async Task OwnAccountOffersNoDestructiveActions()
    {
        var api = new FakeApi { Accounts = Accounts(2) };
        var vm = Console(api, online: new() { ["U_0000"] = new OnlinePresence("g", "Games", false) });
        await vm.OpenAsync("U_0000");

        Assert.False(vm.CanBan);
        Assert.False(vm.CanMoveToLobby);
        Assert.False(vm.CanOpenGrant);
        Assert.Equal("That's you.", vm.MoveToLobbyHint);
    }

    [Fact]
    public async Task BanNeedsASecondPressAndThenSendsTheReason()
    {
        var api = new FakeApi { Accounts = Accounts(2) };
        var vm = Console(api);
        await vm.OpenAsync("U_0001");
        vm.BanReason = " harassment ";

        await vm.BanCommand.ExecuteAsync(null);
        Assert.True(vm.ConfirmingBan);
        Assert.Empty(api.Calls);

        await vm.BanCommand.ExecuteAsync(null);
        Assert.False(vm.ConfirmingBan);
        Assert.Equal(new[] { "ban U_0001 True 'harassment'" }, api.Calls);
        Assert.Equal("banned", vm.ActionStatus);
    }

    [Fact]
    public async Task CancellingTheConfirmationSendsNothing()
    {
        var api = new FakeApi { Accounts = Accounts(2) };
        var vm = Console(api);
        await vm.OpenAsync("U_0001");
        await vm.BanCommand.ExecuteAsync(null);
        vm.CancelBanCommand.Execute(null);
        Assert.False(vm.ConfirmingBan);
        Assert.Empty(api.Calls);
    }

    [Fact]
    public async Task ServerRefusalIsShownInlineAndNothingIsReported()
    {
        var api = new FakeApi { Accounts = Accounts(2), Reply = new AdminResponse { Success = false, ErrorMessage = "Permission denied" } };
        var vm = Console(api);
        await vm.OpenAsync("U_0001");

        await vm.VerifyCommand.ExecuteAsync(null);

        Assert.Equal("Permission denied", vm.ActionError);
        Assert.True(vm.HasActionError);
        Assert.Equal("", vm.ActionStatus);
    }

    [Fact]
    public async Task GrantStartsAllOffAndNoneSelectedMeansRemoveAdmin()
    {
        var api = new FakeApi { Accounts = Accounts(2) };
        api.Accounts[1].IsAdmin = true;
        var vm = Console(api);
        await vm.OpenAsync("U_0001");

        vm.ToggleGrantCommand.Execute(null);
        Assert.True(vm.ShowGrant);
        Assert.False(vm.GrantBanUsers || vm.GrantVerifyUsers || vm.GrantGrantAdmin || vm.GrantManageChannels);
        Assert.Equal("Remove admin", vm.GrantApplyLabel);

        vm.GrantBanUsers = true;
        Assert.Equal("Update admin", vm.GrantApplyLabel);
        await vm.ApplyGrantCommand.ExecuteAsync(null);
        Assert.Contains("AdminPermissions { VerifyUsers = False, BanUsers = True", api.Calls.Single());
    }

    [Fact]
    public async Task SearchAndFilterNarrowTheRows()
    {
        var api = new FakeApi { Accounts = Accounts(4) };
        api.Accounts[3].Banned = true;
        var vm = Console(api);
        await vm.OpenAsync();

        vm.Search = "user2";
        Assert.Single(vm.Rows);
        vm.Search = "";
        vm.SetFilterCommand.Execute(MemberFilter.Banned);
        Assert.Equal(new[] { "user3" }, vm.Rows.Select(r => r.Member.Name));
        Assert.True(vm.FilterIsBanned);
    }

    [Fact]
    public async Task PresenceRefreshUpdatesStatusWithoutAskingTheServer()
    {
        var api = new FakeApi { Accounts = Accounts(2) };
        var online = new Dictionary<string, OnlinePresence>();
        var vm = new ModerationViewModel(() => api, () => "U_0000", () => online);
        await vm.OpenAsync("U_0001");
        Assert.Equal("offline", vm.SelectedStatus);
        var pages = api.Pages.Count;

        online["U_0001"] = new OnlinePresence("g", "Games", false);
        vm.RefreshPresence();

        Assert.Equal("in Games", vm.SelectedStatus);
        Assert.True(vm.CanMoveToLobby);
        Assert.Equal(pages, api.Pages.Count);
    }
}
