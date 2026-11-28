using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Net.Quic;
using System.Threading;
using System.Threading.Tasks;
using Aura.V1Alpha1;
using uniffi.aura_core;

namespace Aura.Desktop.Services;

/// <summary>The moderation requests (0x48-0x4C) the console needs. Implemented by the network client.</summary>
public interface IModerationApi
{
    Task<ListUsersResponse> ListUsersAsync(uint offset, uint limit, CancellationToken ct = default);
    Task<AdminResponse> VerifyUserAsync(string userUuid, CancellationToken ct = default);
    Task<AdminResponse> BanUserAsync(string userUuid, bool banned, string reason, CancellationToken ct = default);
    Task<AdminResponse> GrantAdminAsync(string userUuid, AdminPermissions permissions, CancellationToken ct = default);
    Task<AdminResponse> KickUserAsync(string userUuid, string reason, CancellationToken ct = default);
}

/// <summary>The four permissions GrantAdmin sets. All off revokes admin.</summary>
public readonly record struct AdminPermissions(bool VerifyUsers, bool BanUsers, bool GrantAdmin, bool ManageChannels)
{
    public bool None => !(VerifyUsers || BanUsers || GrantAdmin || ManageChannels);
}

/// <summary>Why the server took us out of a channel (ChannelRemovalReason).</summary>
public enum ChannelRemovalKind { Deleted, NotFound, Kicked }

/// <summary>A ChannelDeleted (0x47) notice, with the reason and the admin's detail text.</summary>
public sealed record ChannelRemoval(string ChannelId, string FallbackChannelId, ChannelRemovalKind Kind, string Detail);

/// <summary>Where an online account currently is.</summary>
public sealed record OnlinePresence(string ChannelId, string ChannelName, bool IsLobby);

/// <summary>One account as the console sees it: the server's UserInfo plus where they are online.</summary>
public sealed record ModMember(
    string Uuid, string Name, bool Verified, bool Banned, bool IsAdmin, long CreatedAt, OnlinePresence? Presence,
    UserPresence? PresenceStatus = null, string? CustomStatus = null, bool IsServerMuted = false)
{
    public bool IsOnline => Presence != null;

    public static ModMember From(UserInfo info, IReadOnlyDictionary<string, OnlinePresence> online) =>
        new(info.UserUuid, info.DisplayName, info.Verified, info.Banned, info.IsAdmin, info.CreatedAt,
            online.TryGetValue(info.UserUuid, out var p) ? p : null,
            info.HasPresence
                ? info.Presence switch
                {
                    Aura.V1Alpha1.PresenceStatus.PresenceIdle => UserPresence.Idle,
                    Aura.V1Alpha1.PresenceStatus.PresenceDnd => UserPresence.Dnd,
                    _ => UserPresence.Online,
                }
                : null,
            info.HasCustomStatus ? info.CustomStatus : null,
            info.IsServerMuted);
}

/// <summary>The STANDING badge. A ban outranks everything, then admin.</summary>
public enum Standing { Banned, Admin, Verified, Unverified }

public enum MemberFilter { All, Online, Awaiting, Banned }

public readonly record struct FilterCounts(int All, int Online, int Awaiting, int Banned);

/// <summary>Filtering, search, standing and wording for the moderation console, kept free of UI types.</summary>
public static class ModerationLogic
{
    /// <summary>QUIC application error code the server closes a banned account's connections with.</summary>
    public const long BanCloseCode = 0x01;

    /// <summary>The AuthResponse error for a banned account's login.</summary>
    public const string BannedLoginError = "User is banned";

    public const string BannedNotice = "You were banned from this server";

    public static Standing StandingOf(ModMember m) =>
        m.Banned ? Standing.Banned
        : m.IsAdmin ? Standing.Admin
        : m.Verified ? Standing.Verified
        : Standing.Unverified;

    public static string StandingLabel(Standing s) => s switch
    {
        Standing.Banned => "banned",
        Standing.Admin => "admin",
        Standing.Verified => "verified",
        _ => "unverified",
    };

    /// <summary>Not yet verified by an admin (a banned account is past waiting).</summary>
    public static bool AwaitingVerification(ModMember m) => !m.Verified && !m.Banned;

    public static FilterCounts Count(IEnumerable<ModMember> members)
    {
        int all = 0, online = 0, awaiting = 0, banned = 0;
        foreach (var m in members)
        {
            all++;
            if (m.IsOnline) online++;
            if (AwaitingVerification(m)) awaiting++;
            if (m.Banned) banned++;
        }
        return new FilterCounts(all, online, awaiting, banned);
    }

    public static bool InFilter(ModMember m, MemberFilter filter) => filter switch
    {
        MemberFilter.Online => m.IsOnline,
        MemberFilter.Awaiting => AwaitingVerification(m),
        MemberFilter.Banned => m.Banned,
        _ => true,
    };

    /// <summary>Search by display name or user id (`U_…`), case-insensitive substring.</summary>
    public static bool Matches(ModMember m, string? query)
    {
        var q = query?.Trim();
        if (string.IsNullOrEmpty(q)) return true;
        return m.Name.Contains(q, StringComparison.OrdinalIgnoreCase)
            || m.Uuid.Contains(q, StringComparison.OrdinalIgnoreCase);
    }

    public static List<ModMember> Apply(IEnumerable<ModMember> members, MemberFilter filter, string? query) =>
        members.Where(m => InFilter(m, filter) && Matches(m, query)).ToList();

    /// <summary>`U_3f8a…5503`: enough of an id to tell two accounts apart.</summary>
    public static string ShortId(string uuid) =>
        uuid.Length <= 14 ? uuid : $"{uuid[..8]}…{uuid[^4..]}";

    public static string Initial(string name) =>
        string.IsNullOrWhiteSpace(name) ? "?" : name.Trim()[..1].ToUpperInvariant();

    /// <summary>The server has no last-seen; all it can say is where they are right now.</summary>
    public static string StatusText(ModMember m) =>
        m.Presence is { } p ? $"in {p.ChannelName}" : "offline";

    /// <summary>`joined 3 days ago`, from the account's creation time (Unix seconds).</summary>
    public static string JoinedText(long createdAtUnix, DateTimeOffset now)
    {
        if (createdAtUnix <= 0) return "joined date unknown";
        DateTimeOffset created;
        try { created = DateTimeOffset.FromUnixTimeSeconds(createdAtUnix); }
        catch (ArgumentOutOfRangeException) { return "joined date unknown"; }

        var age = now - created;
        if (age < TimeSpan.Zero || age < TimeSpan.FromMinutes(1)) return "joined just now";
        if (age < TimeSpan.FromHours(1)) return $"joined {Plural((int)age.TotalMinutes, "minute")} ago";
        if (age < TimeSpan.FromDays(1)) return $"joined {Plural((int)age.TotalHours, "hour")} ago";
        if (age < TimeSpan.FromDays(30)) return $"joined {Plural((int)age.TotalDays, "day")} ago";
        if (age < TimeSpan.FromDays(365)) return $"joined {Plural((int)(age.TotalDays / 30), "month")} ago";
        return $"joined {created.ToString("MMM yyyy", CultureInfo.InvariantCulture)}";
    }

    private static string Plural(int n, string unit) => n == 1 ? $"1 {unit}" : $"{n} {unit}s";

    /// <summary>The server refuses a kick unless the target is in a channel other than the lobby.</summary>
    public static bool CanMoveToLobby(ModMember m, bool isSelf) =>
        !isSelf && m.Presence is { IsLobby: false };

    public static bool CanBan(ModMember m, bool isSelf) => !isSelf && !m.Banned;
    public static bool CanUnban(ModMember m, bool isSelf) => !isSelf && m.Banned;
    public static bool CanVerify(ModMember m) => !m.Verified && !m.Banned;

    /// <summary>Label for the apply button of the admin-permissions panel.</summary>
    public static string GrantLabel(bool targetIsAdmin, AdminPermissions p) =>
        p.None ? (targetIsAdmin ? "Remove admin" : "Choose permissions") : (targetIsAdmin ? "Update admin" : "Make admin");

    /// <summary>Granting nothing to a non-admin does nothing, so it isn't offered.</summary>
    public static bool CanApplyGrant(bool targetIsAdmin, AdminPermissions p, bool isSelf) =>
        !isSelf && (targetIsAdmin || !p.None);

    /// <summary>The notice shown when the server takes us out of a channel.</summary>
    public static string RemovalNotice(ChannelRemoval removal) => removal.Kind switch
    {
        ChannelRemovalKind.Kicked => KickNotice(removal.Detail),
        ChannelRemovalKind.NotFound => "That channel no longer exists",
        _ => "The channel you were in was deleted",
    };

    public static string KickNotice(string? detail)
    {
        const string head = "An admin moved you to the lobby";
        return string.IsNullOrWhiteSpace(detail) ? head : $"{head}: {detail.Trim()}";
    }

    /// <summary>A login refused because the account is banned.</summary>
    public static bool IsBannedLogin(string? errorMessage) =>
        string.Equals(errorMessage?.Trim(), BannedLoginError, StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// True when a stream or connection failure is the server's ban close:
    /// QUIC application error 0x01. System.Net.Quic reports it as a
    /// <see cref="QuicException"/> whose ApplicationErrorCode is set, either
    /// directly or wrapped.
    /// </summary>
    public static bool IsBanClose(Exception? ex)
    {
        for (var e = ex; e != null; e = e.InnerException)
        {
            if (e is QuicException { ApplicationErrorCode: BanCloseCode }) return true;
        }
        return false;
    }

    /// <summary>
    /// Fetch every account, a page at a time (the server caps a page at 500),
    /// until <c>total_count</c> are in hand. An empty page ends the loop so a
    /// server that miscounts cannot spin it forever.
    /// </summary>
    public static async Task<(List<UserInfo> Users, string? Error)> FetchAllAsync(
        IModerationApi api, CancellationToken ct, uint pageSize = 500)
    {
        var users = new List<UserInfo>();
        uint total;
        do
        {
            var page = await api.ListUsersAsync((uint)users.Count, pageSize, ct);
            if (!page.Success)
            {
                return (users, string.IsNullOrEmpty(page.ErrorMessage) ? "The server refused the request" : page.ErrorMessage);
            }
            total = page.TotalCount;
            if (page.Users.Count == 0) break;
            users.AddRange(page.Users);
        }
        while (users.Count < total);
        return (users, null);
    }

    /// <summary>Where each online account is, keyed by user uuid.</summary>
    public static Dictionary<string, OnlinePresence> PresenceMap(
        IEnumerable<(string? Uuid, OnlinePresence Where)> occupants)
    {
        var map = new Dictionary<string, OnlinePresence>(StringComparer.Ordinal);
        foreach (var (uuid, where) in occupants)
        {
            if (!string.IsNullOrEmpty(uuid)) map[uuid] = where;
        }
        return map;
    }
}
