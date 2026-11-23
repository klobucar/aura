import AppKit
import UserNotifications

/// How much chat traffic reaches Notification Center.
nonisolated enum NotificationLevel: String, CaseIterable, Identifiable {
    case all
    case mentions
    case nothing = "none"

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: return "Everything"
        case .mentions: return "Mentions"
        case .nothing: return "Nothing"
        }
    }
}

/// Notification preferences. The keys and defaults are shared with the
/// desktop client.
nonisolated enum NotificationPrefs {
    static let levelKey = "AuraNotifyLevel"
    static let keywordsKey = "AuraNotifyKeywords"
    static let showTextKey = "AuraNotifyShowText"
    static let badgeKey = "AuraNotifyBadge"
    static let joinLeaveKey = "AuraNotifyJoinLeave"

    static func level(_ defaults: UserDefaults = .standard) -> NotificationLevel {
        defaults.string(forKey: levelKey).flatMap(NotificationLevel.init(rawValue:)) ?? .mentions
    }

    static func keywords(_ defaults: UserDefaults = .standard) -> [String] {
        Mentions.keywords(from: defaults.string(forKey: keywordsKey) ?? "")
    }

    static func showsText(_ defaults: UserDefaults = .standard) -> Bool {
        defaults.object(forKey: showTextKey) as? Bool ?? true
    }

    static func countsBadge(_ defaults: UserDefaults = .standard) -> Bool {
        defaults.object(forKey: badgeKey) as? Bool ?? true
    }

    static func joinLeaveBanners(_ defaults: UserDefaults = .standard) -> Bool {
        defaults.object(forKey: joinLeaveKey) as? Bool ?? false
    }

    /// Whether a message reaches Notification Center at `level`.
    static func shouldNotify(level: NotificationLevel, isMention: Bool) -> Bool {
        switch level {
        case .all: return true
        case .mentions: return isMention
        case .nothing: return false
        }
    }
}

/// Banners and the Dock badge for chat that arrives while Aura is in the
/// background. Nothing is posted while the app is active: the window already
/// shows it.
///
/// Permission is asked for the first time a banner is actually due, not at
/// launch; if the user declines, banners are dropped without a fuss.
@MainActor
final class MessageNotifier {
    static let shared = MessageNotifier()

    /// Mentions that arrived while Aura was in the background.
    private(set) var unseenMentions = 0

    private enum Authorization {
        case unknown, requesting, granted, denied
    }

    private var authorization = Authorization.unknown
    private var queued: [UNNotificationRequest] = []
    private var activationObserver: NSObjectProtocol?

    private init() {
        activationObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.clearBadge() }
        }
    }

    /// A chat message from someone else.
    func didReceive(sender: String, channel: String, text: String, isMention: Bool) {
        guard !NSApp.isActive else { return }

        if isMention && NotificationPrefs.countsBadge() {
            unseenMentions += 1
            NSApp.dockTile.badgeLabel = "\(unseenMentions)"
        }

        guard NotificationPrefs.shouldNotify(level: NotificationPrefs.level(), isMention: isMention) else { return }

        let content = UNMutableNotificationContent()
        content.title = ChatFormatting.notificationTitle(sender: sender, channel: channel)
        content.body = NotificationPrefs.showsText()
            ? text
            : ChatFormatting.hiddenNotificationBody(channel: channel, isMention: isMention)
        content.sound = .default
        deliver(content, identifier: "aura_message_\(UUID().uuidString)")
    }

    /// Someone joined or disconnected. Off unless the user asked for these.
    func presenceChanged(_ text: String, identifier: String) {
        guard !NSApp.isActive, NotificationPrefs.joinLeaveBanners() else { return }
        let content = UNMutableNotificationContent()
        content.title = "Aura"
        content.body = text
        content.sound = .default
        deliver(content, identifier: identifier)
    }

    func clearBadge() {
        unseenMentions = 0
        NSApp.dockTile.badgeLabel = nil
    }

    // MARK: Delivery

    private func deliver(_ content: UNMutableNotificationContent, identifier: String) {
        let request = UNNotificationRequest(identifier: identifier, content: content, trigger: nil)
        switch authorization {
        case .granted:
            UNUserNotificationCenter.current().add(request)
        case .denied:
            return
        case .requesting:
            queued.append(request)
        case .unknown:
            queued.append(request)
            authorization = .requesting
            // The completion handlers run on a background queue; @Sendable
            // keeps them from inheriting this type's main-actor isolation.
            UNUserNotificationCenter.current().getNotificationSettings { @Sendable settings in
                let status = settings.authorizationStatus
                Task { @MainActor in self.settled(status) }
            }
        }
    }

    private func settled(_ status: UNAuthorizationStatus) {
        switch status {
        case .authorized, .provisional:
            finish(granted: true)
        case .notDetermined:
            UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { @Sendable granted, _ in
                Task { @MainActor in self.finish(granted: granted) }
            }
        default:
            finish(granted: false)
        }
    }

    private func finish(granted: Bool) {
        authorization = granted ? .granted : .denied
        let pending = queued
        queued = []
        guard granted else { return }
        for request in pending {
            UNUserNotificationCenter.current().add(request)
        }
    }
}
