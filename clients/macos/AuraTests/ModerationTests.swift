import AuraCore
import XCTest
@testable import Aura

/// The pure logic behind the moderation console and the involuntary-removal
/// notices (control_stream.md, "Moderation" and "Leaving a Channel
/// Involuntarily").
@MainActor
final class ModerationTests: XCTestCase {

    private func member(
        _ id: String, _ name: String,
        verified: Bool = false, banned: Bool = false, admin: Bool = false
    ) -> ModerationMember {
        ModerationMember(
            id: id, name: name, verified: verified, banned: banned, isAdmin: admin,
            createdAt: Date(timeIntervalSince1970: 0))
    }

    private var sample: [ModerationMember] {
        [
            member("U_aaaaaaaaaaaa", "Ada", verified: true, admin: true),
            member("U_bbbbbbbbbbbb", "Bob"),
            member("U_cccccccccccc", "Cleo", verified: true),
            member("U_dddddddddddd", "Dan", banned: true),
        ]
    }

    // MARK: Standing

    func testStandingClassification() {
        XCTAssertEqual(member("U_1", "a", verified: true, admin: true).standing, .admin)
        XCTAssertEqual(member("U_1", "a", verified: true).standing, .verified)
        XCTAssertEqual(member("U_1", "a").standing, .unverified)
        // A ban outranks everything else.
        XCTAssertEqual(member("U_1", "a", verified: true, banned: true, admin: true).standing, .banned)
        XCTAssertEqual(ModerationStanding.unverified.label, "unverified")
    }

    func testMemberFromRecord() {
        let record = UserInfoRecord(
            userUuid: "U_xyz", displayName: "Zed", verified: true, banned: false,
            isAdmin: false, createdAt: 86_400, presence: nil, customStatus: "",
            isServerMuted: false)
        let m = ModerationMember(record)
        XCTAssertEqual(m.id, "U_xyz")
        XCTAssertEqual(m.name, "Zed")
        XCTAssertEqual(m.createdAt, Date(timeIntervalSince1970: 86_400))
    }

    func testShortId() {
        XCTAssertEqual(ModerationLogic.shortId("U_abcdef0123456789"), "U_abcdef01…")
        XCTAssertEqual(ModerationLogic.shortId("U_abc"), "U_abc")
    }

    // MARK: Filter, search, counts

    func testCounts() {
        let counts = ModerationLogic.counts(of: sample, onlineIds: ["U_aaaaaaaaaaaa", "U_bbbbbbbbbbbb"])
        XCTAssertEqual(counts[.all], 4)
        XCTAssertEqual(counts[.online], 2)
        // Awaiting verification: not verified and not banned.
        XCTAssertEqual(counts[.awaiting], 1)
        XCTAssertEqual(counts[.banned], 1)
    }

    func testFilterChips() {
        let online: Set<String> = ["U_cccccccccccc"]
        func names(_ f: ModerationFilter) -> [String] {
            ModerationLogic.filter(sample, by: f, query: "", onlineIds: online).map(\.name)
        }
        XCTAssertEqual(names(.all), ["Ada", "Bob", "Cleo", "Dan"])
        XCTAssertEqual(names(.online), ["Cleo"])
        XCTAssertEqual(names(.awaiting), ["Bob"])
        XCTAssertEqual(names(.banned), ["Dan"])
    }

    func testSearchByNameOrId() {
        func names(_ q: String) -> [String] {
            ModerationLogic.filter(sample, by: .all, query: q, onlineIds: []).map(\.name)
        }
        XCTAssertEqual(names("bo"), ["Bob"])
        XCTAssertEqual(names("  CLEO "), ["Cleo"])
        XCTAssertEqual(names("U_dddd"), ["Dan"])
        XCTAssertEqual(names("nobody"), [])
        XCTAssertEqual(names(""), ["Ada", "Bob", "Cleo", "Dan"])
    }

    func testSearchCombinesWithChip() {
        let result = ModerationLogic.filter(sample, by: .banned, query: "ada", onlineIds: [])
        XCTAssertTrue(result.isEmpty)
    }

    // MARK: Presence

    func testPresenceMapping() {
        let channels = [
            "1": ChannelSummary(name: "Lobby", isLobby: true),
            "2": ChannelSummary(name: "Lounge", isLobby: false),
        ]
        let occupants = ["1": ["U_lobby"], "2": ["U_lounge"]]

        func presence(_ uuid: String) -> ModerationPresence {
            ModerationLogic.presence(
                of: uuid, occupants: occupants, channels: channels,
                selfUuid: "U_me", selfChannelId: "2")
        }

        XCTAssertEqual(presence("U_lounge"), .online(channelName: "Lounge", isLobby: false))
        XCTAssertEqual(presence("U_lounge").label, "in Lounge")
        XCTAssertTrue(presence("U_lounge").canMoveToLobby)

        XCTAssertEqual(presence("U_lobby").label, "in Lobby")
        XCTAssertFalse(presence("U_lobby").canMoveToLobby)

        XCTAssertEqual(presence("U_me"), .online(channelName: "Lounge", isLobby: false))

        XCTAssertEqual(presence("U_gone"), .offline)
        XCTAssertEqual(presence("U_gone").label, "offline")
        XCTAssertFalse(presence("U_gone").isOnline)
        XCTAssertFalse(presence("U_gone").canMoveToLobby)
    }

    func testOnlineInNoChannelCannotBeMoved() {
        let p = ModerationLogic.presence(
            of: "U_me", occupants: [:], channels: [:], selfUuid: "U_me", selfChannelId: nil)
        XCTAssertTrue(p.isOnline)
        XCTAssertEqual(p.label, "online")
        XCTAssertFalse(p.canMoveToLobby)
    }

    // MARK: Grant admin

    func testGrantButtonTitle() {
        let none = AdminPermissionsRecord(verifyUsers: false, banUsers: false, grantAdmin: false, manageChannels: false)
        let some = AdminPermissionsRecord(verifyUsers: true, banUsers: false, grantAdmin: false, manageChannels: false)
        XCTAssertEqual(ModerationLogic.grantButtonTitle(isAdmin: true, permissions: none), "Remove admin")
        XCTAssertEqual(ModerationLogic.grantButtonTitle(isAdmin: true, permissions: some), "Update permissions")
        XCTAssertEqual(ModerationLogic.grantButtonTitle(isAdmin: false, permissions: some), "Make admin")
        XCTAssertFalse(ModerationLogic.hasAny(none))
        XCTAssertTrue(ModerationLogic.hasAny(some))
    }

    // MARK: Actions

    func testActionMessageTypes() {
        XCTAssertEqual(ModerationAction.list.messageType, 0x48)
        XCTAssertEqual(ModerationAction.verify.messageType, 0x49)
        XCTAssertEqual(ModerationAction.ban(banned: true).messageType, 0x4A)
        XCTAssertEqual(ModerationAction.ban(banned: false).messageType, 0x4A)
        XCTAssertEqual(ModerationAction.grant.messageType, 0x4B)
        XCTAssertEqual(ModerationAction.kick.messageType, 0x4C)
        XCTAssertEqual(ModerationAction.ban(banned: false).successNotice, "Ban lifted.")
    }

    // MARK: Removal notices

    func testRemovalNoticeText() {
        XCTAssertEqual(ChannelRemovalNotice.text(reason: .deleted, detail: ""), "This channel was deleted")
        XCTAssertEqual(ChannelRemovalNotice.text(reason: .notFound, detail: ""), "That channel no longer exists")
        XCTAssertEqual(ChannelRemovalNotice.text(reason: .kicked, detail: ""), "An admin moved you to the lobby")
        XCTAssertEqual(ChannelRemovalNotice.text(reason: .kicked, detail: "  "), "An admin moved you to the lobby")
        XCTAssertEqual(
            ChannelRemovalNotice.text(reason: .kicked, detail: "too loud"),
            "An admin moved you to the lobby: too loud")
        // Detail is only for kicks.
        XCTAssertEqual(ChannelRemovalNotice.text(reason: .deleted, detail: "x"), "This channel was deleted")
    }

    func testOnlyAKickKeepsTheChannel() {
        XCTAssertTrue(ChannelRemovalNotice.channelStillExists(.kicked))
        XCTAssertFalse(ChannelRemovalNotice.channelStillExists(.deleted))
        XCTAssertFalse(ChannelRemovalNotice.channelStillExists(.notFound))
    }

    // MARK: Bans

    func testBannedDetection() {
        XCTAssertTrue(ModerationLogic.isBannedRefusal("User is banned"))
        XCTAssertFalse(ModerationLogic.isBannedRefusal("Invalid signature"))
        XCTAssertTrue(ModerationLogic.isBannedClose(code: 1, reason: "banned"))
        XCTAssertTrue(ModerationLogic.isBannedClose(code: 1, reason: nil))
        XCTAssertFalse(ModerationLogic.isBannedClose(code: 0, reason: nil))
        XCTAssertFalse(ModerationLogic.isBannedClose(code: 1, reason: "other"))
    }
}
