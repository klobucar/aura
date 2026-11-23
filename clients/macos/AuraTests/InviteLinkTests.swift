import XCTest
@testable import Aura

/// `aura://` invite links. The format is shared byte-for-byte with the
/// desktop client, so these pin the exact text as well as the round trip.
final class InviteLinkTests: XCTestCase {

    private let fingerprint = String(repeating: "ab12", count: 16)

    // MARK: Formatting

    func testDefaultPortIsOmitted() {
        let link = AuraInviteLink(host: "chat.example.org")
        XCTAssertEqual(link.port, 8443)
        XCTAssertEqual(link.string, "aura://chat.example.org")
    }

    func testOtherPortIsWritten() {
        XCTAssertEqual(AuraInviteLink(host: "10.0.0.5", port: 9000).string, "aura://10.0.0.5:9000")
    }

    func testFullLink() {
        let link = AuraInviteLink(
            host: "Chat.Example.org",
            port: 9443,
            channelId: "C_42",
            fingerprint: fingerprint.uppercased(),
            password: "hunter2"
        )
        XCTAssertEqual(link.string, "aura://chat.example.org:9443/C_42?fp=\(fingerprint)&pw=hunter2")
    }

    func testPasswordWithoutFingerprint() {
        let link = AuraInviteLink(host: "example.org", password: "pw")
        XCTAssertEqual(link.string, "aura://example.org?pw=pw")
    }

    func testIPv6IsBracketed() {
        XCTAssertEqual(AuraInviteLink(host: "::1").string, "aura://[::1]")
        XCTAssertEqual(AuraInviteLink(host: "2001:db8::1", port: 9000, channelId: "lobby").string,
                       "aura://[2001:db8::1]:9000/lobby")
    }

    func testPercentEncoding() {
        let link = AuraInviteLink(host: "example.org", channelId: "general chat/ü", password: "p@ss w+rd&=?")
        XCTAssertEqual(link.string, "aura://example.org/general%20chat%2F%C3%BC?pw=p%40ss%20w%2Brd%26%3D%3F")
        XCTAssertEqual(AuraInviteLink.percentEncode("AZaz09-._~"), "AZaz09-._~")
    }

    func testEmptyOptionalPartsAreDropped() {
        let link = AuraInviteLink(host: "example.org", channelId: "", password: "")
        XCTAssertNil(link.channelId)
        XCTAssertNil(link.password)
        XCTAssertEqual(link.string, "aura://example.org")
    }

    // MARK: Parsing

    func testParseMinimal() throws {
        let link = try AuraInviteLink.parse("aura://chat.example.org")
        XCTAssertEqual(link, AuraInviteLink(host: "chat.example.org"))
        XCTAssertNil(link.channelId)
        XCTAssertNil(link.fingerprint)
        XCTAssertNil(link.password)
    }

    func testRoundTrip() throws {
        let original = AuraInviteLink(
            host: "2001:db8::7",
            port: 1234,
            channelId: "Ch 1/ü",
            fingerprint: fingerprint,
            password: "a+b c&d=e%f"
        )
        XCTAssertEqual(try AuraInviteLink.parse(original.string), original)
        XCTAssertEqual(try AuraInviteLink(url: try XCTUnwrap(original.url)), original)
    }

    func testParseIPv6() throws {
        let link = try AuraInviteLink.parse("aura://[2001:DB8::1]:9000/C_1")
        XCTAssertEqual(link.host, "2001:db8::1")
        XCTAssertEqual(link.port, 9000)
        XCTAssertEqual(link.channelId, "C_1")

        let defaultPort = try AuraInviteLink.parse("aura://[::1]")
        XCTAssertEqual(defaultPort.host, "::1")
        XCTAssertEqual(defaultPort.port, 8443)
    }

    func testUppercaseFingerprintIsAcceptedAndLowercased() throws {
        let link = try AuraInviteLink.parse("aura://example.org?fp=\(fingerprint.uppercased())")
        XCTAssertEqual(link.fingerprint, fingerprint)
        XCTAssertEqual(link.string, "aura://example.org?fp=\(fingerprint)")
    }

    func testPercentDecoding() throws {
        let link = try AuraInviteLink.parse("aura://example.org/general%20chat?pw=p%40ss%20w%2Brd")
        XCTAssertEqual(link.channelId, "general chat")
        XCTAssertEqual(link.password, "p@ss w+rd")
    }

    func testPlusIsALiteralPlus() throws {
        XCTAssertEqual(try AuraInviteLink.parse("aura://example.org?pw=a+b").password, "a+b")
    }

    func testUnknownQueryItemsAreIgnored() throws {
        let link = try AuraInviteLink.parse("aura://example.org?v=2&fp=\(fingerprint)&utm=x&pw=pw&flag")
        XCTAssertEqual(link.fingerprint, fingerprint)
        XCTAssertEqual(link.password, "pw")
    }

    func testSchemeAndHostAreCaseInsensitive() throws {
        let link = try AuraInviteLink.parse("  AURA://Chat.Example.ORG:8443/  ")
        XCTAssertEqual(link.host, "chat.example.org")
        XCTAssertEqual(link.port, 8443)
        XCTAssertNil(link.channelId)
    }

    func testFragmentIsIgnored() throws {
        XCTAssertEqual(try AuraInviteLink.parse("aura://example.org/lobby#top").channelId, "lobby")
    }

    // MARK: Malformed

    func testRejectsOtherSchemes() {
        assertRejected("https://example.org", .wrongScheme)
        assertRejected("example.org", .wrongScheme)
        assertRejected("aurax://example.org", .wrongScheme)
    }

    func testRejectsMissingHost() {
        assertRejected("aura://", .missingHost)
        assertRejected("aura://:9000", .missingHost)
        assertRejected("aura:///lobby", .missingHost)
        assertRejected("aura://[]:9000", .missingHost)
    }

    func testRejectsBadHosts() {
        assertRejected("aura://::1", .invalidHost)
        assertRejected("aura://[::1", .invalidHost)
        assertRejected("aura://[::1]x", .invalidHost)
        assertRejected("aura://[example.org]", .invalidHost)
        assertRejected("aura://user@example.org", .invalidHost)
        assertRejected("aura://exa mple.org", .invalidHost)
    }

    func testRejectsBadPorts() {
        assertRejected("aura://example.org:", .invalidPort)
        assertRejected("aura://example.org:0", .invalidPort)
        assertRejected("aura://example.org:65536", .invalidPort)
        assertRejected("aura://example.org:80a", .invalidPort)
        assertRejected("aura://example.org:-1", .invalidPort)
        assertRejected("aura://[::1]:", .invalidPort)
    }

    func testRejectsMalformedFingerprints() {
        assertRejected("aura://example.org?fp=abc", .invalidFingerprint)
        assertRejected("aura://example.org?fp=", .invalidFingerprint)
        assertRejected("aura://example.org?fp=\(String(repeating: "g", count: 64))", .invalidFingerprint)
        assertRejected("aura://example.org?fp=\(fingerprint)00", .invalidFingerprint)
    }

    func testRejectsBadChannels() {
        assertRejected("aura://example.org/a/b", .invalidChannel)
        assertRejected("aura://example.org/%zz", .invalidChannel)
    }

    func testRejectsBadPasswordEncoding() {
        assertRejected("aura://example.org?pw=%E0%A4%A", .invalidPassword)
    }

    // MARK: Matching

    func testMatchesIgnoresHostCaseAndBrackets() {
        let link = AuraInviteLink(host: "Example.org", port: 9000)
        XCTAssertTrue(link.matches(host: "EXAMPLE.ORG", port: 9000))
        XCTAssertFalse(link.matches(host: "example.org", port: 8443))
        XCTAssertTrue(AuraInviteLink(host: "::1").matches(host: "[::1]", port: 8443))
    }

    private func assertRejected(
        _ text: String,
        _ expected: AuraInviteLink.ParseError,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertThrowsError(try AuraInviteLink.parse(text), text, file: file, line: line) { error in
            XCTAssertEqual(error as? AuraInviteLink.ParseError, expected, text, file: file, line: line)
        }
    }
}
