import XCTest
@testable import Aura

/// Mention detection, keyword matching and `@` autocomplete. The rules are
/// shared with the desktop client.
final class MentionsTests: XCTestCase {

    private func mentions(_ text: String, name: String = "Alice", keywords: [String] = [], own: Bool = false) -> Bool {
        Mentions.mentionsYou(text, displayName: name, keywords: keywords, isOwnMessage: own)
    }

    // MARK: @name

    func testMentionAtEndOrBeforeWhitespace() {
        XCTAssertTrue(mentions("hey @Alice"))
        XCTAssertTrue(mentions("@Alice can you look"))
        XCTAssertTrue(mentions("line one\n@Alice\nline three"))
    }

    func testMentionIgnoresCase() {
        XCTAssertTrue(mentions("hey @ALICE"))
        XCTAssertTrue(mentions("hey @alice"))
    }

    func testMentionBeforePunctuation() {
        XCTAssertTrue(mentions("@alice, look"))
        XCTAssertTrue(mentions("thanks @alice!"))
        XCTAssertTrue(mentions("(@alice)"))
        XCTAssertTrue(mentions("@alice's turn"))
    }

    func testLongerNameIsNotAMention() {
        XCTAssertFalse(mentions("@alicex"))
        XCTAssertFalse(mentions("@alice2 hi"))
        XCTAssertFalse(mentions("@alicé"))
        XCTAssertFalse(mentions("alice without the at"))
    }

    func testLaterOccurrenceStillCounts() {
        XCTAssertTrue(mentions("@alicex and @alice"))
    }

    func testNameWithSpaces() {
        XCTAssertTrue(mentions("@bob smith: ping", name: "Bob Smith"))
        XCTAssertFalse(mentions("@bob smithers", name: "Bob Smith"))
    }

    func testOwnMessagesNeverMention() {
        XCTAssertFalse(mentions("@alice note to self", own: true))
        XCTAssertFalse(mentions("deploy now", keywords: ["deploy"], own: true))
    }

    func testEmptyNameMentionsNoOne() {
        XCTAssertFalse(mentions("@ hello", name: ""))
        XCTAssertFalse(mentions("@", name: "  "))
    }

    func testMentionRanges() {
        let text = "@Al, then @al and @alx"
        let ranges = Mentions.mentionRanges(in: text, displayName: "al")
        XCTAssertEqual(ranges.map { String(text[$0]) }, ["@Al", "@al"])
    }

    // MARK: Keywords

    func testKeywordAsWholeWord() {
        XCTAssertTrue(mentions("the deploy failed", keywords: ["deploy"]))
        XCTAssertTrue(mentions("DEPLOY!", keywords: ["deploy"]))
        XCTAssertTrue(mentions("standup in 5", keywords: ["deploy", "standup"]))
    }

    func testKeywordInsideAWordDoesNotCount() {
        XCTAssertFalse(mentions("redeploy it", keywords: ["deploy"]))
        XCTAssertFalse(mentions("deploys", keywords: ["deploy"]))
        XCTAssertFalse(mentions("deploy_bot", keywords: ["deploy"]))
    }

    func testKeywordLaterInTheText() {
        XCTAssertTrue(mentions("redeploy, then deploy", keywords: ["deploy"]))
    }

    func testKeywordSettingParsing() {
        XCTAssertEqual(Mentions.keywords(from: " deploy, ,Standup ,"), ["deploy", "Standup"])
        XCTAssertEqual(Mentions.keywords(from: ""), [])
        XCTAssertFalse(mentions("anything", keywords: Mentions.keywords(from: ", ,")))
    }

    // MARK: Autocomplete

    func testAutocompleteQuery() {
        XCTAssertEqual(Mentions.autocompleteQuery(in: "hey @al"), "al")
        XCTAssertEqual(Mentions.autocompleteQuery(in: "@"), "")
        XCTAssertEqual(Mentions.autocompleteQuery(in: "@Bob S"), "Bob S")
        XCTAssertNil(Mentions.autocompleteQuery(in: "mail me@al"))
        XCTAssertNil(Mentions.autocompleteQuery(in: "no at here"))
        XCTAssertNil(Mentions.autocompleteQuery(in: "@al\nnext"))
    }

    func testSuggestionsMatchPrefixIgnoringCaseUpToSix() {
        let names = ["Alice", "alfred", "Bob", "Alice", "Al", "Alba", "Alma", "Alvin", "Alys"]
        XCTAssertEqual(Mentions.suggestions(for: "al", among: names), ["Alice", "alfred", "Al", "Alba", "Alma", "Alvin"])
        XCTAssertEqual(Mentions.suggestions(for: "B", among: names), ["Bob"])
        XCTAssertEqual(Mentions.suggestions(for: "z", among: names), [])
        XCTAssertEqual(Mentions.suggestions(for: "", among: ["A", "B"]), ["A", "B"])
    }

    func testCompletionReplacesThePartialName() {
        XCTAssertEqual(Mentions.complete("hey @al", with: "Alice"), "hey @Alice ")
        XCTAssertEqual(Mentions.complete("@", with: "Bob Smith"), "@Bob Smith ")
    }

    // MARK: Notification rules

    func testNotificationLevels() {
        XCTAssertTrue(NotificationPrefs.shouldNotify(level: .all, isMention: false))
        XCTAssertTrue(NotificationPrefs.shouldNotify(level: .mentions, isMention: true))
        XCTAssertFalse(NotificationPrefs.shouldNotify(level: .mentions, isMention: false))
        XCTAssertFalse(NotificationPrefs.shouldNotify(level: .nothing, isMention: true))
    }

    func testNotificationDefaultsAndKeys() throws {
        let suite = "AuraTests.notifications.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }

        XCTAssertEqual(NotificationPrefs.level(defaults), .mentions)
        XCTAssertEqual(NotificationPrefs.keywords(defaults), [])
        XCTAssertTrue(NotificationPrefs.showsText(defaults))
        XCTAssertTrue(NotificationPrefs.countsBadge(defaults))
        XCTAssertFalse(NotificationPrefs.joinLeaveBanners(defaults))

        defaults.set("all", forKey: "AuraNotifyLevel")
        defaults.set("deploy, standup", forKey: "AuraNotifyKeywords")
        defaults.set(false, forKey: "AuraNotifyShowText")
        defaults.set(false, forKey: "AuraNotifyBadge")
        defaults.set(true, forKey: "AuraNotifyJoinLeave")
        XCTAssertEqual(NotificationPrefs.level(defaults), .all)
        XCTAssertEqual(NotificationPrefs.keywords(defaults), ["deploy", "standup"])
        XCTAssertFalse(NotificationPrefs.showsText(defaults))
        XCTAssertFalse(NotificationPrefs.countsBadge(defaults))
        XCTAssertTrue(NotificationPrefs.joinLeaveBanners(defaults))

        defaults.set("none", forKey: "AuraNotifyLevel")
        XCTAssertEqual(NotificationPrefs.level(defaults), .nothing)
        defaults.set("bogus", forKey: "AuraNotifyLevel")
        XCTAssertEqual(NotificationPrefs.level(defaults), .mentions)
    }

    // MARK: Copy

    func testCopy() {
        XCTAssertEqual(ChatFormatting.mentionsBelowLabel(count: 1), "1 mention below ⌄")
        XCTAssertEqual(ChatFormatting.mentionsBelowLabel(count: 3), "3 mentions below ⌄")
        XCTAssertEqual(ChatFormatting.notificationTitle(sender: "bob", channel: "Lounge"), "bob · Lounge")
        XCTAssertEqual(ChatFormatting.hiddenNotificationBody(channel: "Lounge", isMention: true), "New mention in Lounge")
        XCTAssertEqual(ChatFormatting.inviteHeader(destination: "chat.example.org"),
                       "You've been invited · Join chat.example.org")
        XCTAssertEqual(ChatFormatting.leaveAndJoinTitle(currentHost: "a.org", newHost: "b.org"),
                       "Leave a.org and join b.org?")
        XCTAssertEqual(ChatFormatting.deleteAccountTitle(host: "a.org"), "Delete your account on a.org?")
        XCTAssertEqual(ChatFormatting.deleteAccountMessage(name: "Alice"),
                       "The server releases the name \"Alice\" and removes your profile. Your key stays on this device, and you can join again later as someone new.")
        XCTAssertEqual(ChatFormatting.accountDeletedNotice(host: "a.org"), "Your account on a.org was deleted.")
    }
}
