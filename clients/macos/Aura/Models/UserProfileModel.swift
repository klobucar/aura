import Foundation

/// Represents a user profile metadata (separate from identity/keys)
public struct UserProfileModel: Identifiable, Codable, Hashable {
    public let id: UUID // Matches UserIdentity id
    public var displayName: String
    public var publicKeyHex: String
    public var createdAt: Date
    public var lastUsed: Date?
    public var linkedServerIds: [UUID] // Servers this profile is used with
    public var requiresBiometric: Bool // Whether this profile requires biometric auth
    
    public init(
        id: UUID = UUID(),
        displayName: String,
        publicKeyHex: String,
        createdAt: Date = Date(),
        lastUsed: Date? = nil,
        linkedServerIds: [UUID] = [],
        requiresBiometric: Bool = false
    ) {
        self.id = id
        self.displayName = displayName
        self.publicKeyHex = publicKeyHex
        self.createdAt = createdAt
        self.lastUsed = lastUsed
        self.linkedServerIds = linkedServerIds
        self.requiresBiometric = requiresBiometric
    }
}
