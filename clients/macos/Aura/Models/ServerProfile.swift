import Foundation

/// Represents a saved server configuration
public struct ServerProfile: Identifiable, Codable, Hashable {
    public let id: UUID
    public var name: String
    public var host: String
    public var port: UInt16
    public var password: String? // Optional server password
    public var lastUsed: Date?
    public var isFavorite: Bool
    
    public init(
        id: UUID = UUID(),
        name: String,
        host: String,
        port: UInt16 = 8443,
        password: String? = nil,
        lastUsed: Date? = nil,
        isFavorite: Bool = false
    ) {
        self.id = id
        self.name = name
        self.host = host
        self.port = port
        self.password = password
        self.lastUsed = lastUsed
        self.isFavorite = isFavorite
    }
}
