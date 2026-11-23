import Foundation

/// Copy and small derivations shared by the roster and chat column.
///
/// The strings here are shared byte-for-byte with the desktop client (handoff:
/// "Byte-for-byte identical across platforms: palette, copy …"), so they live
/// in one place rather than inline in the views.
nonisolated enum ChatFormatting {

    /// `ROSTER · 7 ONLINE`
    static func rosterLabel(online: Int) -> String {
        "ROSTER · \(online) ONLINE"
    }

    /// `12 messages today`
    static func messagesTodayLabel(count: Int) -> String {
        count == 1 ? "1 message today" : "\(count) messages today"
    }

    /// `Message #Lounge…`, or `Message…` before a channel is known.
    static func composerPlaceholder(channelName: String?) -> String {
        guard let name = channelName, !name.isEmpty else { return "Message…" }
        return "Message #\(name)…"
    }

    /// `E2EE · epoch 14`. Plain `E2EE` until the text group exists — the pill
    /// is still true (nothing is sent without it), it just has no epoch yet.
    static func epochLabel(epoch: UInt64?) -> String {
        guard let epoch else { return "E2EE" }
        return "E2EE · epoch \(epoch)"
    }

    /// The identity card's second line: `verified · 3f8a…5503`. Your own key
    /// is the one key you never have to compare.
    static func identityLine(shortFingerprint: String?) -> String {
        guard let short = shortFingerprint, !short.isEmpty else { return "no identity" }
        return "verified · \(short)"
    }

    /// How many of `timestamps` fall on the same calendar day as `now`.
    static func countToday(_ timestamps: [Date], now: Date = Date(), calendar: Calendar = .current) -> Int {
        timestamps.reduce(0) { $0 + (calendar.isDate($1, inSameDayAs: now) ? 1 : 0) }
    }

    /// The profile card's fingerprint: head and tail groups of the full
    /// grouped-hex form, `3f8a 91c2 … b17c 5503`. Short inputs pass through.
    static func cardFingerprint(_ grouped: String) -> String {
        let groups = grouped.split(separator: " ")
        guard groups.count > 4 else { return grouped }
        return "\(groups[0]) \(groups[1]) … \(groups[groups.count - 2]) \(groups[groups.count - 1])"
    }

    // MARK: Invites

    /// Login screen header while following an invite. Before connecting only
    /// the host is known — the link carries the channel's id, not its name.
    static func inviteHeader(destination: String) -> String {
        "You've been invited · Join \(destination)"
    }

    static func inviteChannelToggle(channelName: String) -> String {
        "Open straight into \(channelName)"
    }

    static func leaveAndJoinTitle(currentHost: String, newHost: String) -> String {
        "Leave \(currentHost) and join \(newHost)?"
    }

    static let inviteCertificateMismatch = "Server certificate doesn't match the invite"

    // MARK: Mentions and notifications

    /// `3 mentions below ⌄` — the pill over a chat scrolled up.
    static func mentionsBelowLabel(count: Int) -> String {
        count == 1 ? "1 mention below ⌄" : "\(count) mentions below ⌄"
    }

    /// Banner title: `alice · Lounge`.
    static func notificationTitle(sender: String, channel: String) -> String {
        "\(sender) · \(channel)"
    }

    /// Banner body when message text is kept out of banners.
    static func hiddenNotificationBody(channel: String, isMention: Bool) -> String {
        isMention ? "New mention in \(channel)" : "New message in \(channel)"
    }

    // MARK: Identity and account

    static func lastBackupLabel(_ date: Date) -> String {
        "Last backup \(date.formatted(date: .abbreviated, time: .shortened))"
    }

    static func deleteAccountTitle(host: String) -> String {
        "Delete your account on \(host)?"
    }

    static func deleteAccountMessage(name: String) -> String {
        "The server releases the name \"\(name)\" and removes your profile. Your key stays on this device, and you can join again later as someone new."
    }

    static func accountDeletedNotice(host: String) -> String {
        "Your account on \(host) was deleted."
    }

    /// `20:14` — mono timestamp on the message's name row. Hour cycle follows
    /// the user's locale.
    static func timeLabel(_ date: Date) -> String {
        timeFormatter.string(from: date)
    }

    private static let timeFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = .current
        formatter.setLocalizedDateFormatFromTemplate("jjmm")
        return formatter
    }()
}
