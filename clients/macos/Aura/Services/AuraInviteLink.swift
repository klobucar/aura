import Foundation

/// An invite link, shared byte-for-byte with the desktop client:
///
///     aura://<host>[:<port>][/<channel-id>][?fp=<sha256-hex>][&pw=<password>]
///
/// - `host`: a DNS name or IPv4 address; an IPv6 address goes in brackets.
///   The port defaults to 8443 and is left out when it is 8443.
/// - `channel-id`: one percent-encoded path segment, the channel's id (not
///   its name).
/// - `fp`: 64 hex characters, the SHA-256 of the server's leaf certificate
///   (DER) — the value `QuicNetworkClient` computes for trust-on-first-use.
///   Written lowercase, read in either case. Only present when the sender
///   trusts the server through a pinned fingerprint: a server with a publicly
///   valid certificate gets none, since its fingerprint changes on every
///   renewal and would break the link.
/// - `pw`: the server password, percent-encoded, only when the sender chose
///   to include it.
///
/// Unknown query items are ignored. Percent-encoding leaves the RFC 3986
/// unreserved characters (`A–Z a–z 0–9 - . _ ~`) alone and encodes every
/// other UTF-8 byte as `%XX`; `+` is a literal plus, never a space.
nonisolated struct AuraInviteLink: Equatable, Hashable, Sendable {
    static let scheme = "aura"
    static let defaultPort: UInt16 = 8443

    /// DNS name, IPv4 address, or IPv6 address without brackets. Lowercase.
    var host: String
    var port: UInt16
    var channelId: String?
    /// Lowercase hex SHA-256 of the server's leaf certificate.
    var fingerprint: String?
    var password: String?

    init(
        host: String,
        port: UInt16 = AuraInviteLink.defaultPort,
        channelId: String? = nil,
        fingerprint: String? = nil,
        password: String? = nil
    ) {
        self.host = Self.normalizedHost(host)
        self.port = port
        self.channelId = channelId.flatMap { $0.isEmpty ? nil : $0 }
        self.fingerprint = fingerprint.map { $0.lowercased() }
        self.password = password.flatMap { $0.isEmpty ? nil : $0 }
    }

    enum ParseError: Error, Equatable, LocalizedError {
        /// Not an `aura://` link.
        case wrongScheme
        case missingHost
        case invalidHost
        case invalidPort
        case invalidChannel
        case invalidFingerprint
        case invalidPassword

        var errorDescription: String? {
            switch self {
            case .wrongScheme: return "This isn't an Aura invite link."
            case .missingHost: return "The invite link has no server address."
            case .invalidHost: return "The invite link's server address isn't valid."
            case .invalidPort: return "The invite link's port isn't valid."
            case .invalidChannel: return "The invite link's channel isn't valid."
            case .invalidFingerprint: return "The invite link's certificate fingerprint isn't valid."
            case .invalidPassword: return "The invite link's password isn't encoded properly."
            }
        }
    }

    // MARK: Formatting

    /// The link as text.
    var string: String {
        var result = "\(Self.scheme)://"
        result += host.contains(":") ? "[\(host)]" : host
        if port != Self.defaultPort {
            result += ":\(port)"
        }
        if let channelId, !channelId.isEmpty {
            result += "/" + Self.percentEncode(channelId)
        }
        var items: [String] = []
        if let fingerprint {
            items.append("fp=\(fingerprint.lowercased())")
        }
        if let password, !password.isEmpty {
            items.append("pw=\(Self.percentEncode(password))")
        }
        if !items.isEmpty {
            result += "?" + items.joined(separator: "&")
        }
        return result
    }

    var url: URL? { URL(string: string) }

    /// True when this link points at the server at `host`:`port`.
    func matches(host: String, port: UInt16) -> Bool {
        self.host == Self.normalizedHost(host) && self.port == port
    }

    /// Host as compared and stored: trimmed, lowercase, IPv6 without brackets.
    static func normalizedHost(_ host: String) -> String {
        var trimmed = host.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if trimmed.hasPrefix("["), trimmed.hasSuffix("]"), trimmed.count >= 2 {
            trimmed = String(trimmed.dropFirst().dropLast())
        }
        return trimmed
    }

    /// Unreserved characters stay; every other UTF-8 byte becomes `%XX`.
    static func percentEncode(_ text: String) -> String {
        var result = ""
        for byte in text.utf8 {
            switch byte {
            case 0x41...0x5A, 0x61...0x7A, 0x30...0x39, 0x2D, 0x2E, 0x5F, 0x7E:
                result.unicodeScalars.append(Unicode.Scalar(byte))
            default:
                result += String(format: "%%%02X", byte)
            }
        }
        return result
    }

    // MARK: Parsing

    init(url: URL) throws {
        self = try Self.parse(url.absoluteString)
    }

    static func parse(_ text: String) throws -> AuraInviteLink {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let separator = trimmed.range(of: "://"),
              trimmed[..<separator.lowerBound].lowercased() == scheme
        else { throw ParseError.wrongScheme }

        var rest = trimmed[separator.upperBound...]
        if let hash = rest.firstIndex(of: "#") {
            rest = rest[..<hash]
        }
        var query: Substring = ""
        if let mark = rest.firstIndex(of: "?") {
            query = rest[rest.index(after: mark)...]
            rest = rest[..<mark]
        }
        let authority: Substring
        let path: Substring
        if let slash = rest.firstIndex(of: "/") {
            authority = rest[..<slash]
            path = rest[rest.index(after: slash)...]
        } else {
            authority = rest
            path = ""
        }

        let (host, port) = try parseAuthority(authority)
        let channelId = try parseChannel(path)

        var fingerprint: String?
        var password: String?
        for item in query.split(separator: "&") {
            let key: Substring
            let value: Substring
            if let equals = item.firstIndex(of: "=") {
                key = item[..<equals]
                value = item[item.index(after: equals)...]
            } else {
                key = item
                value = ""
            }
            switch key {
            case "fp" where fingerprint == nil:
                guard isFingerprint(value) else { throw ParseError.invalidFingerprint }
                fingerprint = value.lowercased()
            case "pw" where password == nil:
                guard let decoded = String(value).removingPercentEncoding else {
                    throw ParseError.invalidPassword
                }
                password = decoded
            default:
                continue
            }
        }

        return AuraInviteLink(
            host: host,
            port: port,
            channelId: channelId,
            fingerprint: fingerprint,
            password: password
        )
    }

    private static func parseAuthority(_ authority: Substring) throws -> (String, UInt16) {
        guard !authority.isEmpty else { throw ParseError.missingHost }
        guard !authority.contains("@") else { throw ParseError.invalidHost }

        let host: Substring
        var portText: Substring?
        if authority.hasPrefix("[") {
            guard let close = authority.firstIndex(of: "]") else { throw ParseError.invalidHost }
            host = authority[authority.index(after: authority.startIndex)..<close]
            let after = authority[authority.index(after: close)...]
            if !after.isEmpty {
                guard after.hasPrefix(":") else { throw ParseError.invalidHost }
                portText = after.dropFirst()
            }
            guard !host.isEmpty else { throw ParseError.missingHost }
            guard isIPv6Literal(host) else { throw ParseError.invalidHost }
        } else {
            // An unbracketed IPv6 address cannot be told apart from host:port.
            let parts = authority.split(separator: ":", omittingEmptySubsequences: false)
            guard parts.count <= 2 else { throw ParseError.invalidHost }
            host = parts[0]
            if parts.count == 2 { portText = parts[1] }
            guard !host.isEmpty else { throw ParseError.missingHost }
            guard isHostName(host) else { throw ParseError.invalidHost }
        }

        var port = defaultPort
        if let portText {
            guard !portText.isEmpty, portText.count <= 5,
                  portText.allSatisfy({ ("0"..."9").contains($0) }),
                  let value = UInt16(portText), value > 0
            else { throw ParseError.invalidPort }
            port = value
        }
        return (String(host).lowercased(), port)
    }

    private static func parseChannel(_ path: Substring) throws -> String? {
        var segment = path
        if segment.hasSuffix("/") { segment = segment.dropLast() }
        guard !segment.isEmpty else { return nil }
        guard !segment.contains("/"),
              let decoded = String(segment).removingPercentEncoding,
              !decoded.isEmpty
        else { throw ParseError.invalidChannel }
        return decoded
    }

    /// Exactly 64 hex digits, either case.
    static func isFingerprint<S: StringProtocol>(_ text: S) -> Bool {
        text.count == 64 && text.allSatisfy { $0.isASCII && $0.isHexDigit }
    }

    /// A DNS name or IPv4 address: ASCII letters, digits, `-`, `.`, `_`.
    private static func isHostName(_ host: Substring) -> Bool {
        host.allSatisfy { character in
            character.isASCII && (character.isLetter || character.isNumber || "-._".contains(character))
        }
    }

    /// Hex digits, colons and (for an embedded IPv4 tail) dots, with at least
    /// one colon.
    private static func isIPv6Literal(_ host: Substring) -> Bool {
        host.contains(":") && host.allSatisfy { character in
            character.isASCII && (character.isHexDigit || character == ":" || character == ".")
        }
    }
}
