import XCTest
@testable import Aura

/// Copy and derivations behind the roster and chat column (handoff §1, §2).
/// The strings are shared byte-for-byte with the desktop client, so a drift
/// here is a cross-platform bug, not a cosmetic one.
@MainActor
final class ChatFormattingTests: XCTestCase {

    func testRosterLabel() {
        XCTAssertEqual(ChatFormatting.rosterLabel(online: 7), "ROSTER · 7 ONLINE")
        XCTAssertEqual(ChatFormatting.rosterLabel(online: 0), "ROSTER · 0 ONLINE")
    }

    func testMessagesTodayLabelPluralises() {
        XCTAssertEqual(ChatFormatting.messagesTodayLabel(count: 0), "0 messages today")
        XCTAssertEqual(ChatFormatting.messagesTodayLabel(count: 1), "1 message today")
        XCTAssertEqual(ChatFormatting.messagesTodayLabel(count: 12), "12 messages today")
    }

    func testComposerPlaceholder() {
        XCTAssertEqual(ChatFormatting.composerPlaceholder(channelName: "Lounge"), "Message #Lounge…")
        XCTAssertEqual(ChatFormatting.composerPlaceholder(channelName: nil), "Message…")
        XCTAssertEqual(ChatFormatting.composerPlaceholder(channelName: ""), "Message…")
    }

    func testEpochLabel() {
        XCTAssertEqual(ChatFormatting.epochLabel(epoch: 14), "E2EE · epoch 14")
        XCTAssertEqual(ChatFormatting.epochLabel(epoch: nil), "E2EE")
    }

    func testIdentityLine() {
        XCTAssertEqual(ChatFormatting.identityLine(shortFingerprint: "3f8a…5503"), "verified · 3f8a…5503")
        XCTAssertEqual(ChatFormatting.identityLine(shortFingerprint: nil), "no identity")
        XCTAssertEqual(ChatFormatting.identityLine(shortFingerprint: ""), "no identity")
    }

    func testCountTodayIgnoresOtherDays() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        // 2026-03-10T15:00:00Z
        let now = Date(timeIntervalSince1970: 1_773_154_800)
        let earlierToday = now.addingTimeInterval(-3 * 3600)
        let yesterday = now.addingTimeInterval(-24 * 3600)
        let tomorrow = now.addingTimeInterval(24 * 3600)

        XCTAssertEqual(
            ChatFormatting.countToday([now, earlierToday, yesterday, tomorrow], now: now, calendar: calendar),
            2
        )
        XCTAssertEqual(ChatFormatting.countToday([], now: now, calendar: calendar), 0)
    }

    func testCardFingerprintKeepsHeadAndTail() {
        let full = "3f8a 91c2 be04 77dd 5a1e 0c93 f6b8 220a e714 8db5 3c60 a9f1 04d2 6e8f b17c 5503"
        XCTAssertEqual(ChatFormatting.cardFingerprint(full), "3f8a 91c2 … b17c 5503")
        // Short enough already: passes through untouched.
        XCTAssertEqual(ChatFormatting.cardFingerprint("3f8a 91c2 b17c 5503"), "3f8a 91c2 b17c 5503")
    }
}

/// The markdown subset (handoff §6): bold, italic, code, link, list, quote —
/// no headings, no images, no remote fetch, no surprise link schemes.
@MainActor
final class AuraMarkdownTests: XCTestCase {

    func testParagraphBulletAndQuoteBlocks() {
        let source = """
        **Audio nerd.** Poke me

        > PTT is ⌥Space here
        > on this server
        - rebuilds Thursdays
        * [focus room](aura://focus)
        """
        XCTAssertEqual(AuraMarkdown.blocks(from: source), [
            .paragraph("**Audio nerd.** Poke me"),
            .quote("PTT is ⌥Space here\non this server"),
            .bullet("rebuilds Thursdays"),
            .bullet("[focus room](aura://focus)"),
        ])
    }

    func testEmphasisIsNotABullet() {
        XCTAssertEqual(AuraMarkdown.blocks(from: "*really* fine"), [.paragraph("*really* fine")])
    }

    func testHeadingsStayLiteral() {
        XCTAssertEqual(AuraMarkdown.blocks(from: "# Not a heading"), [.paragraph("# Not a heading")])
        let rendered = String(AuraMarkdown.inline("# Not a heading").characters)
        XCTAssertEqual(rendered, "# Not a heading")
    }

    func testInlineEmphasisIsParsed() {
        let rendered = AuraMarkdown.inline("**bold** and `code`")
        XCTAssertEqual(String(rendered.characters), "bold and code")
    }

    func testAllowedLinksSurvive() {
        let rendered = AuraMarkdown.inline("[site](https://example.com) [room](aura://focus)")
        let links = rendered.runs.compactMap(\.link)
        XCTAssertEqual(links.map(\.absoluteString), ["https://example.com", "aura://focus"])
    }

    func testDisallowedLinkSchemesAreStripped() {
        let rendered = AuraMarkdown.inline("[open me](file:///etc/passwd) [app](x-evil://run)")
        XCTAssertTrue(rendered.runs.allSatisfy { $0.link == nil })
        XCTAssertEqual(String(rendered.characters), "open me app")
    }

    func testImagesAreNeverReferenced() {
        let rendered = AuraMarkdown.inline("![tracker](https://example.com/pixel.png)")
        XCTAssertTrue(rendered.runs.allSatisfy { $0.imageURL == nil })
    }

    func testIsAllowed() {
        XCTAssertTrue(AuraMarkdown.isAllowed(URL(string: "HTTPS://example.com")!))
        XCTAssertTrue(AuraMarkdown.isAllowed(URL(string: "mailto:a@example.com")!))
        XCTAssertFalse(AuraMarkdown.isAllowed(URL(string: "javascript:alert(1)")!))
        XCTAssertFalse(AuraMarkdown.isAllowed(URL(string: "relative/path")!))
    }
}
