import Foundation
import Security
@testable import Aura

/// Sizes fixed by `aura_identity`.
enum IdentitySizes {
    static let secret = 64
    static let identityKey = 1_984
    static let ed25519Key = 32
    static let signature = 3_373
}

/// In-memory `SecretKeychain`, so identity storage and migration can be tested
/// without a usable Keychain (CI runners have none).
final class InMemoryKeychain: SecretKeychain {
    private var items: [String: Data] = [:]

    /// When set, reads fail with this status (a locked or missing Keychain).
    var readFailure: OSStatus?
    /// When set, adds, updates and deletes fail with this status.
    var writeFailure: OSStatus?
    /// Number of successful adds and updates.
    private(set) var writes = 0

    private func key(_ service: String, _ account: String) -> String {
        service + "\u{0}" + account
    }

    var isEmpty: Bool { items.isEmpty }

    /// Seed an item directly, as an earlier app version would have left it.
    func put(_ data: Data, service: String, account: String) {
        items[key(service, account)] = data
    }

    func item(service: String, account: String) -> Data? {
        items[key(service, account)]
    }

    func read(service: String, account: String) -> (data: Data?, status: OSStatus) {
        if let readFailure { return (nil, readFailure) }
        guard let data = items[key(service, account)] else { return (nil, errSecItemNotFound) }
        return (data, errSecSuccess)
    }

    func add(_ data: Data, service: String, account: String) -> OSStatus {
        if let writeFailure { return writeFailure }
        guard items[key(service, account)] == nil else { return errSecDuplicateItem }
        items[key(service, account)] = data
        writes += 1
        return errSecSuccess
    }

    func update(_ data: Data, service: String, account: String) -> OSStatus {
        if let writeFailure { return writeFailure }
        guard items[key(service, account)] != nil else { return errSecItemNotFound }
        items[key(service, account)] = data
        writes += 1
        return errSecSuccess
    }

    func delete(service: String, account: String) -> OSStatus {
        if let writeFailure { return writeFailure }
        return items.removeValue(forKey: key(service, account)) == nil ? errSecItemNotFound : errSecSuccess
    }
}

/// The fields of a login frame, `[0x03][u32 LE length][AuthRequest]`, by
/// protobuf field number (1 identity key, 2 display name, 3 signature,
/// 4 server password, 5 challenge). Nil if the framing is wrong.
func authRequestFields(_ frame: Data) -> [Int: Data]? {
    let bytes = [UInt8](frame)
    guard bytes.count >= 5, bytes[0] == 0x03 else { return nil }
    let length = Int(bytes[1]) | Int(bytes[2]) << 8 | Int(bytes[3]) << 16 | Int(bytes[4]) << 24
    guard length == bytes.count - 5 else { return nil }
    return protobufLengthDelimitedFields(Array(bytes[5...]))
}

/// Length-delimited fields of a protobuf message, which is every field an
/// AuthRequest has. Nil on anything else or on truncation.
func protobufLengthDelimitedFields(_ bytes: [UInt8]) -> [Int: Data]? {
    var fields: [Int: Data] = [:]
    var index = 0

    func varint() -> Int? {
        var result = 0
        var shift = 0
        while index < bytes.count, shift < 64 {
            let byte = bytes[index]
            index += 1
            result |= Int(byte & 0x7F) << shift
            if byte & 0x80 == 0 { return result }
            shift += 7
        }
        return nil
    }

    while index < bytes.count {
        guard let tag = varint(), tag & 0x7 == 2, let length = varint(),
              length >= 0, index + length <= bytes.count else { return nil }
        fields[tag >> 3] = Data(bytes[index..<(index + length)])
        index += length
    }
    return fields
}
