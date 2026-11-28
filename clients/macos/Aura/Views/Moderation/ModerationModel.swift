import AuraCore
import Foundation

// Pure logic behind the admin moderation console. Nothing here touches the
// network or SwiftUI, so it can be tested directly.

/// One account on the server, as ListUsers reports it.
struct ModerationMember: Identifiable, Equatable, Hashable {
    let id: String          // the account's `U_…` id
    let name: String
    let verified: Bool
    let banned: Bool
    let isAdmin: Bool
    let createdAt: Date
    let presence: AuraPresenceStatus?
    let customStatus: String?
    let isServerMuted: Bool

    init(
        id: String,
        name: String,
        verified: Bool,
        banned: Bool,
        isAdmin: Bool,
        createdAt: Date,
        presence: AuraPresenceStatus? = nil,
        customStatus: String? = nil,
        isServerMuted: Bool = false
    ) {
        self.id = id
        self.name = name
        self.verified = verified
        self.banned = banned
        self.isAdmin = isAdmin
        self.createdAt = createdAt
        self.presence = presence
        self.customStatus = customStatus
        self.isServerMuted = isServerMuted
    }

    init(_ record: UserInfoRecord) {
        let mappedPresence: AuraPresenceStatus? = record.presence.map {
            switch $0 {
            case .online: return .online
            case .idle: return .idle
            case .dnd: return .dnd
            }
        }
        self.init(
            id: record.userUuid,
            name: record.displayName,
            verified: record.verified,
            banned: record.banned,
            isAdmin: record.isAdmin,
            createdAt: Date(timeIntervalSince1970: TimeInterval(record.createdAt)),
            presence: mappedPresence,
            customStatus: record.customStatus,
            isServerMuted: record.isServerMuted
        )
    }

    var standing: ModerationStanding { ModerationStanding(self) }
    var shortId: String { ModerationLogic.shortId(id) }
}

/// The STANDING badge. A ban outranks everything, then admin, then whether the
/// server has verified the account.
enum ModerationStanding: Equatable {
    case banned, admin, verified, unverified

    init(_ member: ModerationMember) {
        if member.banned { self = .banned }
        else if member.isAdmin { self = .admin }
        else if member.verified { self = .verified }
        else { self = .unverified }
    }

    var label: String {
        switch self {
        case .banned: return "banned"
        case .admin: return "admin"
        case .verified: return "verified"
        case .unverified: return "unverified"
        }
    }
}

/// The filter chips above the table.
enum ModerationFilter: CaseIterable, Identifiable {
    case all, online, awaiting, banned

    var id: Self { self }

    var title: String {
        switch self {
        case .all: return "all"
        case .online: return "online"
        case .awaiting: return "awaiting verification"
        case .banned: return "banned"
        }
    }
}

/// Where an account is right now.
enum ModerationPresence: Equatable {
    case offline
    case online(channelName: String?, isLobby: Bool)

    var isOnline: Bool {
        if case .online = self { return true }
        return false
    }

    /// Whether a kick would be accepted: the account is in a channel that
    /// isn't the lobby. The server refuses anyone else.
    var canMoveToLobby: Bool {
        if case .online(let name?, let isLobby) = self { return !name.isEmpty && !isLobby }
        return false
    }

    /// `in Lounge` / `online` / `offline`.
    var label: String {
        switch self {
        case .offline: return "offline"
        case .online(let name?, _) where !name.isEmpty: return "in \(name)"
        case .online: return "online"
        }
    }
}

/// The two channel facts the console needs.
struct ChannelSummary: Equatable {
    let name: String
    let isLobby: Bool
}

/// What the server lets an admin do to a member, and the copy around it.
enum ModerationLogic {
    /// `U_ab12cd…` kept short enough to sit under a name.
    static func shortId(_ id: String) -> String {
        guard id.count > 10 else { return id }
        return String(id.prefix(10)) + "…"
    }

    /// Where `uuid` is: the channel of any session logged in as that account.
    /// `occupants` maps a channel id to the account ids in it; `selfUuid` and
    /// `selfChannelId` cover the local user, who is not in that map.
    static func presence(
        of uuid: String,
        occupants: [String: [String]],
        channels: [String: ChannelSummary],
        selfUuid: String?,
        selfChannelId: String?
    ) -> ModerationPresence {
        func online(in channelId: String?) -> ModerationPresence {
            let channel = channelId.flatMap { channels[$0] }
            return .online(channelName: channel?.name, isLobby: channel?.isLobby ?? false)
        }
        if uuid == selfUuid { return online(in: selfChannelId) }
        for (channelId, uuids) in occupants where uuids.contains(uuid) {
            return online(in: channelId)
        }
        return .offline
    }

    /// Case-insensitive match on display name or account id. An empty query
    /// matches everyone.
    static func matches(_ member: ModerationMember, query: String) -> Bool {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return true }
        return member.name.localizedCaseInsensitiveContains(q)
            || member.id.localizedCaseInsensitiveContains(q)
    }

    static func matches(_ member: ModerationMember, filter: ModerationFilter, onlineIds: Set<String>) -> Bool {
        switch filter {
        case .all: return true
        case .online: return onlineIds.contains(member.id)
        case .awaiting: return !member.verified && !member.banned
        case .banned: return member.banned
        }
    }

    /// Members passing both the chip and the search box.
    static func filter(
        _ members: [ModerationMember],
        by filter: ModerationFilter,
        query: String,
        onlineIds: Set<String>
    ) -> [ModerationMember] {
        members.filter {
            matches($0, filter: filter, onlineIds: onlineIds) && matches($0, query: query)
        }
    }

    /// The number on each chip. Counts ignore the search box.
    static func counts(of members: [ModerationMember], onlineIds: Set<String>) -> [ModerationFilter: Int] {
        var result: [ModerationFilter: Int] = [:]
        for chip in ModerationFilter.allCases {
            result[chip] = members.filter { matches($0, filter: chip, onlineIds: onlineIds) }.count
        }
        return result
    }

    /// `Make admin…`, or `Remove admin` for an admin whose permissions would
    /// all be switched off (granting none revokes).
    static func grantButtonTitle(isAdmin: Bool, permissions: AdminPermissionsRecord) -> String {
        if isAdmin && !hasAny(permissions) { return "Remove admin" }
        return isAdmin ? "Update permissions" : "Make admin"
    }

    static func hasAny(_ p: AdminPermissionsRecord) -> Bool {
        p.verifyUsers || p.banUsers || p.grantAdmin || p.manageChannels
    }

    /// What the user sees after a ban.
    static let bannedMessage = "You were banned from this server"

    /// The AuthResponse error for a banned account.
    static func isBannedRefusal(_ message: String) -> Bool {
        message == "User is banned"
    }

    /// QUIC application error 0x01 / reason `banned`: the account was banned
    /// while connected.
    static func isBannedClose(code: UInt64, reason: String?) -> Bool {
        reason == "banned" || (code == 0x01 && (reason ?? "").isEmpty)
    }

    /// `joined 3 days ago`.
    static func joinedLabel(_ date: Date, now: Date = Date()) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return "joined " + formatter.localizedString(for: date, relativeTo: now)
    }
}

/// The system line for a ChannelDeleted (0x47) notice.
enum ChannelRemovalNotice {
    static func text(reason: ChannelRemovalReason, detail: String) -> String {
        switch reason {
        case .deleted:
            return "This channel was deleted"
        case .notFound:
            return "That channel no longer exists"
        case .kicked:
            let trimmed = detail.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty
                ? "An admin moved you to the lobby"
                : "An admin moved you to the lobby: \(trimmed)"
        }
    }

    /// A kick leaves the channel standing; the other two mean it is gone.
    static func channelStillExists(_ reason: ChannelRemovalReason) -> Bool {
        reason == .kicked
    }
}

/// A request awaiting its reply on the control stream. Replies arrive in
/// order, each on its request's own type.
enum ModerationAction: Equatable {
    case list
    case verify
    case ban(banned: Bool)
    case grant
    case kick
    case serverMute(muted: Bool)

    /// The control message type this action's request and reply use.
    var messageType: UInt8 {
        switch self {
        case .list: return 0x48
        case .verify: return 0x49
        case .ban: return 0x4A
        case .grant: return 0x4B
        case .kick: return 0x4C
        case .serverMute: return 0x4D
        }
    }

    var successNotice: String {
        switch self {
        case .list: return ""
        case .verify: return "Account verified."
        case .ban(let banned): return banned ? "Account banned." : "Ban lifted."
        case .grant: return "Admin permissions updated."
        case .kick: return "Moved to the lobby."
        case .serverMute(let muted): return muted ? "Server-muted." : "Server-unmuted."
        }
    }
}
