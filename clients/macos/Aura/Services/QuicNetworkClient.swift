import Foundation
import Combine
import Network
import Observation

/// Native QUIC client for Aura server using Apple's Network framework.
/// Uses NWConnectionGroup to handle server-initiated streams for Apple/Quinn interop.
@Observable
@MainActor
public class QuicNetworkClient {
    
    // MARK: - Published State
    
    public var isConnected = false
    public var isAuthenticated = false
    public var connectionStatus = "Disconnected"
    public var userId: UInt32 = 0
    public var sessionToken: String?
    public var currentChannelId: UInt32?
    public var sessionId: UInt32?  // Our own session ID
    
    /// Users by channel ID (tracks all channels, not just current)
    public var usersByChannel: [UInt32: [ChannelUser]] = [:]
    
    // MARK: - Private Properties
    
    /// The QUIC connection group (manages the tunnel and accepts streams)
    private var connectionGroup: NWConnectionGroup?
    
    /// Control stream from server (for auth and signaling)
    private var controlStream: NWConnection?
    
    /// Continuation for waiting on server stream
    private var streamContinuation: CheckedContinuation<NWConnection, Error>?
    
    /// Audio pipeline for E2EE
    public let audioPipeline = AudioPipeline()
    
    private var sequenceNumber: UInt16 = 0
    
    // Protocol message types (matching server)
    private static let MSG_CHALLENGE_REQUEST: UInt8 = 0x01
    private static let MSG_CHALLENGE_RESPONSE: UInt8 = 0x02
    private static let MSG_AUTH_REQUEST: UInt8 = 0x03
    private static let MSG_AUTH_RESPONSE: UInt8 = 0x04
    private static let MSG_JOIN_CHANNEL: UInt8 = 0x10
    private static let MSG_USER_JOINED: UInt8 = 0x11
    private static let MSG_USER_LEFT: UInt8 = 0x12
    private static let MSG_CHANNEL_STATE: UInt8 = 0x13
    
    // ALPN protocol identifier
    private static let ALPN = "aura-dave"
    
    // Keepalive interval (must be < server timeout of 30s)
    private static let keepaliveInterval: TimeInterval = 10.0
    
    /// Timer for sending keepalive pings
    private var keepaliveTask: Task<Void, Never>?
    
    /// Task for listening to server messages
    private var listenerTask: Task<Void, Never>?
    
    public init() {}
    
    // MARK: - Connection
    
    /// Connect to the Aura server via QUIC using NWConnectionGroup.
    /// This allows accepting server-initiated streams.
    public func connect(host: String, port: UInt16 = 8443) async throws {
        connectionStatus = "Connecting..."
        print("[QuicClient] Connecting to \(host):\(port) with ALPN '\(Self.ALPN)'...")
        
        let endpoint = NWEndpoint.hostPort(
            host: NWEndpoint.Host(host),
            port: NWEndpoint.Port(rawValue: port)!
        )
        
        // Create QUIC options
        let quicOptions = NWProtocolQUIC.Options(alpn: [Self.ALPN])
        quicOptions.direction = .bidirectional
        quicOptions.idleTimeout = 30_000
        quicOptions.isDatagram = true
        quicOptions.maxDatagramFrameSize = 1200
        
        // Accept self-signed certificates
        sec_protocol_options_set_verify_block(quicOptions.securityProtocolOptions, { _, trust, completeHandler in
            print("[QuicClient] TLS verify - accepting self-signed cert")
            completeHandler(true)
        }, .global())
        
        sec_protocol_options_set_min_tls_protocol_version(quicOptions.securityProtocolOptions, .TLSv13)
        
        // Create parameters
        let parameters = NWParameters(quic: quicOptions)
        parameters.allowLocalEndpointReuse = true
        
        // Create NWMultiplexGroup descriptor
        let descriptor = NWMultiplexGroup(to: endpoint)
        
        // Create NWConnectionGroup
        let group = NWConnectionGroup(with: descriptor, using: parameters)
        connectionGroup = group
        
        print("[QuicClient] Creating QUIC connection group...")
        
        // Set up handler for incoming streams from server
        group.newConnectionHandler = { [weak self] newConnection in
            print("[QuicClient] Received new stream from server!")
            Task { @MainActor in
                self?.handleIncomingStream(newConnection)
            }
        }
        
        // Wait for group to be ready
        try await waitForGroupReady(group)
        
        isConnected = true
        connectionStatus = "Connected (waiting for server stream)"
        print("[QuicClient] QUIC tunnel ready, waiting for server stream...")
    }
    
    private func waitForGroupReady(_ group: NWConnectionGroup) async throws {
        return try await withCheckedThrowingContinuation { continuation in
            var resumed = false
            
            group.stateUpdateHandler = { [weak self] state in
                guard !resumed else { return }
                
                Task { @MainActor in
                    switch state {
                    case .setup:
                        print("[QuicClient] Group setup...")
                    case .waiting(let error):
                        print("[QuicClient] Group waiting: \(error)")
                    case .ready:
                        print("[QuicClient] Group ready!")
                        resumed = true
                        continuation.resume()
                    case .failed(let error):
                        print("[QuicClient] Group failed: \(error)")
                        self?.isConnected = false
                        resumed = true
                        continuation.resume(throwing: error)
                    case .cancelled:
                        print("[QuicClient] Group cancelled")
                        self?.isConnected = false
                        if !resumed {
                            resumed = true
                            continuation.resume(throwing: QuicClientError.connectionClosed)
                        }
                    @unknown default:
                        break
                    }
                }
            }
            
            group.start(queue: DispatchQueue.global(qos: .userInitiated))
        }
    }
    
    /// Handle incoming stream from server
    private func handleIncomingStream(_ connection: NWConnection) {
        print("[QuicClient] Handling incoming server stream...")
        
        // Start the connection
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready:
                print("[QuicClient] Server stream ready!")
                Task { @MainActor in
                    // Store as control stream
                    self?.controlStream = connection
                    // Resume any waiting continuation
                    self?.streamContinuation?.resume(returning: connection)
                    self?.streamContinuation = nil
                }
            case .failed(let error):
                print("[QuicClient] Server stream failed: \(error)")
                Task { @MainActor in
                    self?.streamContinuation?.resume(throwing: error)
                    self?.streamContinuation = nil
                }
            default:
                break
            }
        }
        
        connection.start(queue: DispatchQueue.global(qos: .userInitiated))
    }
    
    /// Wait for server to open a stream
    private func waitForServerStream() async throws -> NWConnection {
        // If we already have a control stream, return it
        if let stream = controlStream {
            return stream
        }
        
        // Wait for server to open stream
        return try await withCheckedThrowingContinuation { continuation in
            streamContinuation = continuation
            
            // Timeout after 10 seconds
            Task {
                try? await Task.sleep(nanoseconds: 10_000_000_000)
                if let cont = await MainActor.run(body: { self.streamContinuation }) {
                    await MainActor.run {
                        self.streamContinuation = nil
                    }
                    cont.resume(throwing: QuicClientError.protocolError("Timeout waiting for server stream"))
                }
            }
        }
    }
    
    // MARK: - Authentication
    
    /// Authenticate using TOFU with Ed25519 signature.
    /// Server-first protocol: wait for ServerHello with challenge, then send AuthRequest.
    public func authenticate(identity: UserIdentity, serverPassword: String? = nil) async throws {
        guard let publicKey = identity.publicKey else {
            throw QuicClientError.noIdentity
        }
        
        connectionStatus = "Waiting for server..."
        print("[QuicClient] Waiting for server to open auth stream...")
        
        // Wait for server to open a stream
        let stream = try await waitForServerStream()
        controlStream = stream
        
        connectionStatus = "Authenticating..."
        print("[QuicClient] Authenticating as '\(identity.displayName)'...")
        
        // Step 1: Receive ServerHello with challenge
        print("[QuicClient] Waiting for ServerHello...")
        let serverHello = try await receive(on: stream, minimumLength: 33, maximumLength: 33)
        
        guard serverHello.count >= 33, serverHello[0] == Self.MSG_CHALLENGE_RESPONSE else {
            throw QuicClientError.protocolError("Invalid ServerHello: got \(serverHello.count) bytes")
        }
        
        let challenge = serverHello.subdata(in: 1..<33)
        print("[QuicClient] Received challenge: \(challenge.prefix(8).hexString)...")
        
        // Step 2: Sign challenge
        guard let signature = identity.sign(challenge) else {
            throw QuicClientError.signingFailed
        }
        
        // Step 3: Build and send AuthRequest
        var authReq = Data()
        authReq.append(Self.MSG_AUTH_REQUEST)
        authReq.append(UInt8(publicKey.count))
        authReq.append(publicKey)
        
        let displayNameData = identity.displayName.data(using: .utf8) ?? Data()
        authReq.append(UInt8(displayNameData.count))
        authReq.append(displayNameData)
        
        authReq.append(UInt8(signature.count))
        authReq.append(signature)
        
        authReq.append(UInt8(challenge.count))
        authReq.append(challenge)
        
        let passwordData = serverPassword?.data(using: .utf8) ?? Data()
        authReq.append(UInt8(passwordData.count))
        if !passwordData.isEmpty {
            authReq.append(passwordData)
        }
        
        print("[QuicClient] Sending auth request (\(authReq.count) bytes)...")
        try await send(data: authReq, on: stream)
        
        // Step 4: Receive auth response
        print("[QuicClient] Waiting for auth response...")
        let authResp = try await receive(on: stream, minimumLength: 7, maximumLength: 256)
        
        guard authResp.count >= 7, authResp[0] == Self.MSG_AUTH_RESPONSE else {
            throw QuicClientError.protocolError("Invalid auth response")
        }
        
        let success = authResp[1] != 0
        let responseUserId = authResp.subdata(in: 2..<6).withUnsafeBytes { $0.load(as: UInt32.self) }
        
        var pos = 6
        let tokenLen = Int(authResp[pos])
        pos += 1
        let token = String(data: authResp.subdata(in: pos..<(pos + tokenLen)), encoding: .utf8) ?? ""
        pos += tokenLen
        
        let verified = authResp[pos] != 0
        pos += 1
        
        let errorLen = Int(authResp[pos])
        pos += 1
        let errorMsg = errorLen > 0 ? String(data: authResp.subdata(in: pos..<(pos + errorLen)), encoding: .utf8) : nil
        
        print("[QuicClient] Auth response: success=\(success), userId=\(responseUserId)")
        
        guard success else {
            throw QuicClientError.authenticationFailed(errorMsg ?? "Unknown error")
        }
        
        self.userId = responseUserId
        self.sessionToken = token
        self.isAuthenticated = true
        self.connectionStatus = "Authenticated as user \(userId)" + (verified ? " (verified)" : "")
        
        print("[QuicClient] ✓ Authentication SUCCESS!")
        
        // Initialize audio pipeline with session token as key (temporary POC)
        // In production, this would use MLS derived key
        if let tokenData = token.data(using: .utf8) {
            // Pad/truncate to 32 bytes for ChaCha20 key
            var keyData = Data(count: 32)
            let copyCount = min(tokenData.count, 32)
            keyData.replaceSubrange(0..<copyCount, with: tokenData[0..<copyCount])
            
            try? audioPipeline.initialize(sessionId: responseUserId, key: keyData)
            print("[QuicClient] Audio pipeline initialized")
            
            // Auto-join default channel for testing
            try await joinChannel(1)
            
            // Start keepalive to prevent session timeout
            startKeepalive()
            
            // Start listening for server messages (presence updates)
            startListening()
        }
    }
    
    // MARK: - Keepalive
    
    /// Start periodic keepalive pings to prevent session timeout
    private func startKeepalive() {
        stopKeepalive()
        
        keepaliveTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(Self.keepaliveInterval * 1_000_000_000))
                
                guard let self = self, self.isAuthenticated else { break }
                
                await self.sendKeepalivePing()
            }
        }
        
        print("[QuicClient] Keepalive timer started (every \(Self.keepaliveInterval)s)")
    }
    
    /// Stop the keepalive timer
    private func stopKeepalive() {
        keepaliveTask?.cancel()
        keepaliveTask = nil
    }
    
    /// Send a keepalive ping via control stream
    private func sendKeepalivePing() async {
        guard let stream = controlStream else { return }
        
        // Send keepalive ping (0x00 byte)
        let ping = Data([0x00])
        do {
            try await send(data: ping, on: stream)
        } catch {
            print("[QuicClient] Keepalive ping failed: \(error)")
        }
    }
    
    // MARK: - Server Message Listening
    
    /// Start listening for messages from the server
    private func startListening() {
        stopListening()
        
        listenerTask = Task { [weak self] in
            print("[QuicClient] Listener task started")
            while !Task.isCancelled {
                guard let self = self,
                      let stream = await MainActor.run(body: { self.controlStream }),
                      self.isAuthenticated else {
                    print("[QuicClient] Listener stopping - no stream or not authenticated")
                    break
                }
                
                do {
                    // Read message type byte
                    let typeData = try await self.receiveNonBlocking(on: stream, length: 1)
                    guard !typeData.isEmpty else {
                        print("[QuicClient] Received empty data, continuing...")
                        continue
                    }
                    
                    let msgType = typeData[0]
                    print(String(format: "[QuicClient] Received message type: 0x%02X", msgType))
                    
                    // Handle message synchronously to avoid race conditions
                    await self.handleServerMessage(type: msgType, stream: stream)
                } catch {
                    // Connection closed or error
                    print("[QuicClient] Listener error: \(error)")
                    break
                }
            }
            print("[QuicClient] Listener task ended")
        }
        
        print("[QuicClient] Started listening for server messages")
    }
    
    /// Stop listening for server messages
    private func stopListening() {
        listenerTask?.cancel()
        listenerTask = nil
    }
    
    /// Handle incoming server message based on type
    private func handleServerMessage(type: UInt8, stream: NWConnection) async {
        print(String(format: "[QuicClient] Handling message type: 0x%02X", type))
        switch type {
        case Self.MSG_USER_JOINED: // 0x11
            await handleUserJoined(stream: stream)
            
        case Self.MSG_USER_LEFT: // 0x12
            await handleUserLeft(stream: stream)
            
        case Self.MSG_CHANNEL_STATE: // 0x13
            await handleChannelState(stream: stream)
            
        case 0x21: // MSG_AUDIO_STREAM - ignore, handled elsewhere
            break
            
        default:
            print(String(format: "[QuicClient] Unknown message type: 0x%02X", type))
            break
        }
    }
    
    /// Handle UserJoined message
    private func handleUserJoined(stream: NWConnection) async {
        do {
            // Read channel_id (4 bytes) + session_id (4 bytes) + name_len (1 byte)
            let header = try await receive(on: stream, minimumLength: 9, maximumLength: 9)
            let channelId = header.prefix(4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let sessionId = header.subdata(in: 4..<8).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let nameLen = Int(header[8])
            
            // Read display name
            let nameData = try await receive(on: stream, minimumLength: nameLen, maximumLength: nameLen)
            let displayName = String(data: nameData, encoding: .utf8) ?? "Unknown"
            
            let user = ChannelUser(sessionId: sessionId, displayName: displayName)
            
            // Add to channel's user list (@Observable tracks this automatically)
            await MainActor.run {
                // Track our own session ID when we see ourselves join
                if sessionId != self.sessionId && displayName == UserDefaults.standard.string(forKey: "AuraDisplayName") {
                    self.sessionId = sessionId
                    print("[QuicClient] Detected own session ID: \(sessionId)")
                }
                
                // Don't add ourselves to the user list
                if sessionId != self.sessionId {
                    if usersByChannel[channelId] == nil {
                        usersByChannel[channelId] = []
                    }
                    usersByChannel[channelId]?.append(user)
                    print("[QuicClient] User joined channel \(channelId): \(displayName) (session \(sessionId))")
                    print("[QuicClient] Channel \(channelId) now has \(usersByChannel[channelId]?.count ?? 0) users")
                } else {
                    print("[QuicClient] Ignoring own UserJoined for channel \(channelId)")
                }
            }
        } catch {
            print("[QuicClient] Failed to parse UserJoined: \(error)")
        }
    }
    
    /// Handle UserLeft message
    private func handleUserLeft(stream: NWConnection) async {
        do {
            // Read channel_id (4 bytes) + session_id (4 bytes)
            let data = try await receive(on: stream, minimumLength: 8, maximumLength: 8)
            let channelId = data.prefix(4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let sessionId = data.subdata(in: 4..<8).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            
            // Remove from channel's user list (@Observable tracks this automatically)
            await MainActor.run {
                usersByChannel[channelId]?.removeAll { $0.id == sessionId }
                print("[QuicClient] User left channel \(channelId): session \(sessionId)")
                print("[QuicClient] Channel \(channelId) now has \(usersByChannel[channelId]?.count ?? 0) users")
            }
        } catch {
            print("[QuicClient] Failed to parse UserLeft: \(error)")
        }
    }
    
    /// Handle ChannelState message
    private func handleChannelState(stream: NWConnection) async {
        do {
            // Read channel_id (4 bytes) + user_count (1 byte)
            let header = try await receive(on: stream, minimumLength: 5, maximumLength: 5)
            let headerHex = header.map { String(format: "%02X", $0) }.joined(separator: " ")
            print("[QuicClient] ChannelState header bytes: \(headerHex)")
            
            let channelId = header.prefix(4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let userCount = Int(header[4])
            
            print("[QuicClient] ChannelState for channel \(channelId): \(userCount) users")
            
            var users: [ChannelUser] = []
            
            for i in 0..<userCount {
                // Read session_id (4 bytes) + name_len (1 byte)
                let userHeader = try await receive(on: stream, minimumLength: 5, maximumLength: 5)
                let userHeaderHex = userHeader.map { String(format: "%02X", $0) }.joined(separator: " ")
                print("[QuicClient]   User \(i+1) header: \(userHeaderHex)")
                
                let sessionId = userHeader.prefix(4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
                let nameLen = Int(userHeader[4])
                
                print("[QuicClient]   Session ID: \(sessionId), Name length: \(nameLen)")
                
                // Read display name
                let nameData = try await receive(on: stream, minimumLength: nameLen, maximumLength: nameLen)
                let nameHex = nameData.map { String(format: "%02X", $0) }.joined(separator: " ")
                print("[QuicClient]   Name bytes: \(nameHex)")
                
                let displayName = String(data: nameData, encoding: .utf8) ?? "Unknown"
                
                // Don't add ourselves to the list
                if sessionId != self.sessionId {
                    users.append(ChannelUser(sessionId: sessionId, displayName: displayName))
                    print("[QuicClient]   User \(i+1): \(displayName) (session \(sessionId))")
                } else {
                    print("[QuicClient]   Skipping self in ChannelState: \(displayName) (session \(sessionId))")
                }
            }
            
            // Replace channel's user list (@Observable tracks this automatically)
            await MainActor.run {
                usersByChannel[channelId] = users
                print("[QuicClient] Updated usersByChannel[\(channelId)] with \(users.count) users")
                print("[QuicClient] Total channels tracked: \(usersByChannel.keys.sorted())")
            }
        } catch {
            print("[QuicClient] Failed to parse ChannelState: \(error)")
        }
    }
    
    /// Non-blocking receive with timeout
    private func receiveNonBlocking(on connection: NWConnection, length: Int) async throws -> Data {
        return try await withCheckedThrowingContinuation { continuation in
            connection.receive(minimumIncompleteLength: length, maximumLength: length) { data, _, isComplete, error in
                if let error = error {
                    continuation.resume(throwing: error)
                } else if let data = data {
                    continuation.resume(returning: data)
                } else if isComplete {
                    continuation.resume(throwing: QuicClientError.connectionClosed)
                } else {
                    continuation.resume(returning: Data())
                }
            }
        }
    }
    
    // MARK: - Audio (via Control Stream for now)
    
    /// Send audio frame via control stream.
    /// TODO: Switch to proper QUIC datagrams once API is figured out.
    /// Send audio frame via control stream (encapsulated) or datagram (future).
    public func sendAudioDatagram(_ rawPcmBytes: Data) async throws {
        guard isAuthenticated else { return }
        guard let stream = controlStream else { return }
        
        // Convert Data -> [Int16]
        let pcmBuffer = rawPcmBytes.withUnsafeBytes {
            Array($0.bindMemory(to: Int16.self))
        }
        
        // Process through pipeline (Opus + Encrypt + Packet Header)
        // This returns the FULL FastAudioPacket bytes
        let packetData: Data
        do {
            packetData = try audioPipeline.process(pcm: pcmBuffer)
        } catch {
            print("[QuicClient] Audio processing error: \(error)")
            return
        }
        
        // Send directly if using Datagrams (TODO: enable real datagrams)
        // For now, wrap in control stream message 0x20
        
        var message = Data()
        message.append(0x20) // Audio Message Type
        
        // Append Length (u32 LE)
        let length = UInt32(packetData.count)
        message.append(contentsOf: withUnsafeBytes(of: length.littleEndian) { Data($0) })
        
        // Append the ENTIRE fast packet (header + ciphertext)
        message.append(packetData)
        
        // Send
        try await send(data: message, on: stream)
        
        if sequenceNumber % 100 == 0 {
            print("[QuicClient] Sent encapsulated audio packet #\(audioPipeline.sequence - 1) (\(packetData.count) bytes)")
        }
        sequenceNumber &+= 1
    }
    
    // MARK: - Channel
    
    public func joinChannel(_ channelId: UInt32) async throws {
        guard let stream = controlStream else {
            throw QuicClientError.notConnected
        }
        
        print("[QuicClient] Joining channel \(channelId)...")
        
        var data = Data([Self.MSG_JOIN_CHANNEL])
        data.append(contentsOf: withUnsafeBytes(of: channelId.littleEndian) { Data($0) })
        
        try await send(data: data, on: stream)
        currentChannelId = channelId
        connectionStatus = "In channel \(channelId)"
    }
    
    // MARK: - Disconnect
    
    public func disconnect() {
        stopKeepalive()
        stopListening()
        controlStream?.cancel()
        connectionGroup?.cancel()
        controlStream = nil
        connectionGroup = nil
        isConnected = false
        isAuthenticated = false
        currentChannelId = nil
        usersByChannel = [:]
        connectionStatus = "Disconnected"
        print("[QuicClient] Disconnected")
    }
    
    // MARK: - Network Helpers
    
    private func send(data: Data, on connection: NWConnection) async throws {
        return try await withCheckedThrowingContinuation { continuation in
            connection.send(content: data, completion: .contentProcessed { error in
                if let error = error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            })
        }
    }
    
    private func receive(on connection: NWConnection, minimumLength: Int, maximumLength: Int) async throws -> Data {
        return try await withCheckedThrowingContinuation { continuation in
            connection.receive(minimumIncompleteLength: minimumLength, maximumLength: maximumLength) { data, _, isComplete, error in
                if let error = error {
                    continuation.resume(throwing: error)
                } else if let data = data, !data.isEmpty {
                    print("[QuicClient] Received \(data.count) bytes")
                    continuation.resume(returning: data)
                } else if isComplete {
                    continuation.resume(throwing: QuicClientError.connectionClosed)
                } else {
                    continuation.resume(returning: Data())
                }
            }
        }
    }
}

// MARK: - Errors

public enum QuicClientError: Error, LocalizedError {
    case notConnected
    case noIdentity
    case signingFailed
    case protocolError(String)
    case authenticationFailed(String)
    case connectionClosed
    
    public var errorDescription: String? {
        switch self {
        case .notConnected: return "Not connected to server"
        case .noIdentity: return "No identity available"
        case .signingFailed: return "Failed to sign challenge"
        case .protocolError(let msg): return "Protocol error: \(msg)"
        case .authenticationFailed(let msg): return "Authentication failed: \(msg)"
        case .connectionClosed: return "Connection closed"
        }
    }
}

// MARK: - Channel User Model

/// Represents a user in a voice channel
public struct ChannelUser: Identifiable, Hashable {
    public let id: UInt32  // session_id
    public let displayName: String
    
    public init(sessionId: UInt32, displayName: String) {
        self.id = sessionId
        self.displayName = displayName
    }
}
