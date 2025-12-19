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
    
    /// Received chat messages (populated by incoming text packets)
    public var receivedMessages: [ReceivedTextMessage] = []
    
    /// System events (user leave/join) for UI
    public var systemEvents: [SystemEvent] = []
    
    // MARK: - Private Properties
    
    /// The QUIC connection group (manages the tunnel and accepts streams)
    private var quicGroup: NWConnectionGroup?
    
    /// Control stream from server (for auth and signaling)
    private var controlStream: NWConnection?
    
    /// Continuation for waiting on server stream
    private var streamContinuation: CheckedContinuation<NWConnection, Error>?
    
    /// Audio sender (Rust UniFFI wrapper for encoding + encryption)
    private var audioSender: AudioSenderWrapper?
    
    /// Audio receiver (Rust UniFFI wrapper for decoding + decryption)
    private var audioReceiver: AudioReceiverWrapper?
    
    /// Text crypto wrapper (Rust UniFFI wrapper for text encryption/decryption)
    private var textCrypto: TextCryptoWrapper?
    
    /// Audio playback engine
    private let audioPlayback = AudioPlayback()
    
    /// Track which users are currently speaking (for UI indicators)
    public var activeSpeakers: Set<UInt32> = []
    
    /// Track last activity time for each speaker (for debouncing)
    private var lastSpeakerActivity: [UInt32: Date] = [:]

    /// Timers for turning off speaking indicators after silence
    private var speakerTimers: [UInt32: Task<Void, Never>] = [:]
    
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
    private static let MSG_TEXT_PACKET: UInt8 = 0x30
    
    // ALPN protocol identifier
    private static let ALPN = "aura-dave"
    
    // Keepalive interval (must be < server timeout of 30s)
    private static let keepaliveInterval: TimeInterval = 10.0
    
    /// Timer for sending keepalive pings
    private var keepaliveTask: Task<Void, Never>?
    
    /// Task for listening to server messages
    private var listenerTask: Task<Void, Never>?
    
    /// Task for listening to QUIC datagrams (unreliable audio)
    private var datagramTask: Task<Void, Never>?
    
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
        quicGroup = group
        
        print("[QuicClient] Creating QUIC connection group...")
        
        // Set up handler for incoming streams from server
        group.newConnectionHandler = { [weak self] newConnection in
            print("[QuicClient] Received new stream from server!")
            Task { @MainActor in
                self?.handleIncomingStream(newConnection)
            }
        }
        
        // Set up handler for incoming datagrams (audio packets)
        group.setReceiveHandler(maximumMessageSize: 1220, rejectOversizedMessages: true) { [weak self] _, content, _ in
            guard let self = self, let data = content, !data.isEmpty else { return }
            
            print("[QuicClient] Received datagram: \(data.count) bytes - First byte: 0x\(String(format: "%02X", data[0]))")
            
            // Parse datagram type
            if data.count == 1 {
                print("[QuicClient] Ignoring 1-byte datagram (likely keepalive)")
                return
            }
            
            if data[0] == 0x01 {  // Audio datagram
                let audioData = data.subdata(in: 1..<data.count)
                print("[QuicClient] Processing audio datagram: \(audioData.count) bytes")
                Task { @MainActor in
                    self.processIncomingAudioPacket(audioData)
                }
            } else {
                print("[QuicClient] Unknown datagram type: 0x\(String(format: "%02X", data[0]))")
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
        
        print("[QuicClient] Auth response: success=\(success), userId=\(responseUserId), token=\(token.prefix(8))..., verified=\(verified)")
        
        guard success else {
            throw QuicClientError.authenticationFailed(errorMsg ?? "Unknown error")
        }
        
        self.userId = responseUserId
        self.sessionToken = token
        self.isAuthenticated = true
        
        // Use userId as sessionId immediately (server sends the real session ID now)
        self.sessionId = responseUserId
        print("[QuicClient] Session ID set to: \(responseUserId) from auth response")
        self.connectionStatus = "Authenticated as user \(userId)" + (verified ? " (verified)" : "")
        
        print("[QuicClient] ✓ Authentication SUCCESS!")
        
        // Initialize audio pipeline with session token as key (temporary POC)
        // In production, this would use MLS derived key
        if let tokenData = token.data(using: .utf8) {
            // Pad/truncate to 32 bytes for ChaCha20 key
            var keyData = Data(count: 32)
            let copyCount = min(tokenData.count, 32)
            keyData.replaceSubrange(0..<copyCount, with: tokenData[0..<copyCount])
            
            do {
                // Initialize audio sender/receiver with DAVE key
                let keyData = Data(repeating: 0x42, count: 32)  // TODO: Derive from MLS
                audioSender = try AudioSenderWrapper(sessionId: responseUserId, key: keyData)
                audioSender?.setEpoch(epoch: 0)
                
                // Enable DRED (10 frames = 100ms of redundancy)
                audioSender?.setDredDuration(duration: 10)
                print("[QuicClient] DRED enabled (100ms redundancy)")
                
                // Initialize audio receiver (for receiving others' voice)
                audioReceiver = try AudioReceiverWrapper()
                
                // Initialize text crypto with same DAVE key
                textCrypto = try TextCryptoWrapper(key: keyData)
                
                // Start audio playback engine
                audioPlayback.start()
                
                // Start audio capture automatically for testing
                Task { @MainActor in
                    await self.startAudioCapture()
                }
                
                print("[QuicClient] Audio sender/receiver/playback initialized")
            } catch {
                print("[QuicClient] Failed to initialize audio: \(error)")
            }
            
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
        
        // Start control stream listener for reliable messages
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
        datagramTask?.cancel()
        datagramTask = nil
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
            
        case 0x20: // MSG_AUDIO - audio packet from server
            await handleAudioPacket(stream: stream)
            
        case 0x21: // MSG_AUDIO_STREAM - ignore, handled elsewhere
            break
            
        case Self.MSG_TEXT_PACKET: // 0x30
            await handleTextPacket(stream: stream)
            
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
                // Get our saved display name
                let myDisplayName = UserDefaults.standard.string(forKey: "AuraDisplayName") ?? ""
                
                print("[QuicClient] UserJoined: session=\(sessionId), name=\(displayName), myName=\(myDisplayName), mySessionId=\(self.sessionId ?? 999)")
                
                // Detect our own session ID by matching display name
                if self.sessionId == nil && !myDisplayName.isEmpty && displayName == myDisplayName {
                    self.sessionId = sessionId
                    print("[QuicClient] Detected own session ID: \(sessionId) (matched name: \(displayName))")
                }
                
                // Don't add ourselves to the user list
                if sessionId != self.sessionId {
                    if usersByChannel[channelId] == nil {
                        usersByChannel[channelId] = []
                    }
                    usersByChannel[channelId]?.append(user)
                    
                    // Add system event
                    let event = SystemEvent(content: "\(displayName) joined", channelId: channelId)
                    systemEvents.append(event)
                    
                    print("[QuicClient] User joined channel \(channelId): \(displayName) (session \(sessionId))")
                    print("[QuicClient] Channel \(channelId) now has \(usersByChannel[channelId]?.count ?? 0) users")
                    
                    // Add sender to audio receiver for decryption (using same key as sender)
                    if let receiver = self.audioReceiver {
                        // Use same DAVE key as AudioSenderWrapper (line 345)
                        let keyData = Data(repeating: 0x42, count: 32)  // TODO: Derive from MLS
                        
                        do {
                            try receiver.addSender(sessionId: sessionId, key: keyData, epochHint: 0)
                            print("[QuicClient] Added audio sender \(sessionId) for decryption")
                        } catch {
                            print("[QuicClient] Failed to add sender: \(error)")
                        }
                    }
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
            
            // Remove from audio receiver
            audioReceiver?.removeSender(sessionId: sessionId)
            print("[QuicClient] Removed audio sender \(sessionId) from receiver")
            
            // Remove from channel's user list (@Observable tracks this automatically)
            await MainActor.run {
                if let index = usersByChannel[channelId]?.firstIndex(where: { $0.id == sessionId }) {
                    let user = usersByChannel[channelId]?[index]
                    let name = user?.displayName ?? "Unknown"
                    
                    // Add system event
                    let event = SystemEvent(content: "\(name) disconnected", channelId: channelId)
                    systemEvents.append(event)
                    
                    usersByChannel[channelId]?.remove(at: index)
                    print("[QuicClient] User left channel \(channelId): \(name) (session \(sessionId))")
                }
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
                    
                    // Add to audio receiver for decryption
                    if let receiver = self.audioReceiver {
                        let keyData = Data(repeating: 0x42, count: 32)  // TODO: Derive from MLS
                        do {
                            try receiver.addSender(sessionId: sessionId, key: keyData, epochHint: 0)
                            print("[QuicClient]   Added audio sender \(sessionId)")
                        } catch {
                            print("[QuicClient]   Failed to add audio sender: \(error)")
                        }
                    }
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
    
    /// Handle incoming TextPacket message
    private func handleTextPacket(stream: NWConnection) async {
        do {
            // Read length (4 bytes)
            let lenData = try await receive(on: stream, minimumLength: 4, maximumLength: 4)
            let packetLen = lenData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            
            print("[QuicClient] Text packet length: \(packetLen)")
            
            // Read the binary packet
            let packetData = try await receive(on: stream, minimumLength: Int(packetLen), maximumLength: Int(packetLen))
            
            // Parse encrypted packet
            // Format: sender_session_id(4) + channel_id(4) + epoch(8) + message_id_len(1) + message_id + content_len(4) + ciphertext + nonce(24) + tag(16) + reply_len(1) + reply_id
            let minPacketSize = 16 + 1 + 1 + 4 + 24 + 16  // 62 bytes minimum
            guard packetData.count >= minPacketSize else {
                print("[QuicClient] Text packet too short for encrypted format")
                return
            }
            
            let senderSessionId = packetData.subdata(in: 0..<4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let channelId = packetData.subdata(in: 4..<8).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let epoch = packetData.subdata(in: 8..<16).withUnsafeBytes { $0.load(as: UInt64.self).littleEndian }
            
            var offset = 16
            
            // Parse message ID
            let messageIdLen = Int(packetData[offset])
            offset += 1
            
            guard offset + messageIdLen <= packetData.count else {
                print("[QuicClient] Text packet too short for message ID")
                return
            }
            let messageIdData = packetData.subdata(in: offset..<offset+messageIdLen)
            let messageId = String(data: messageIdData, encoding: .utf8) ?? UUID().uuidString
            offset += messageIdLen
            
            // Parse ciphertext
            guard offset + 4 <= packetData.count else {
                print("[QuicClient] Text packet too short for ciphertext length")
                return
            }
            let ciphertextLen = Int(packetData.subdata(in: offset..<offset+4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian })
            offset += 4
            
            guard offset + ciphertextLen + 24 + 16 <= packetData.count else {
                print("[QuicClient] Text packet too short for ciphertext + nonce + tag")
                return
            }
            let ciphertext = Array(packetData.subdata(in: offset..<offset+ciphertextLen))
            offset += ciphertextLen
            
            // Parse nonce (24 bytes)
            let nonce = Array(packetData.subdata(in: offset..<offset+24))
            offset += 24
            
            // Parse tag (16 bytes)
            let tag = Array(packetData.subdata(in: offset..<offset+16))
            offset += 16
            
            // Parse reply ID if present
            var replyToId: String? = nil
            if offset < packetData.count {
                let replyLen = Int(packetData[offset])
                offset += 1
                if replyLen > 0 && offset + replyLen <= packetData.count {
                    let replyData = packetData.subdata(in: offset..<offset+replyLen)
                    replyToId = String(data: replyData, encoding: .utf8)
                }
            }
            
            // Decrypt the message
            guard let crypto = textCrypto else {
                print("[QuicClient] Text crypto not initialized, cannot decrypt")
                return
            }
            
            let encryptedPacket = EncryptedTextPacketRecord(
                senderSessionId: senderSessionId,
                channelId: channelId,
                epoch: epoch,
                messageId: messageId,
                ciphertext: Data(ciphertext),
                nonce: Data(nonce),
                tag: Data(tag),
                replyToId: replyToId ?? ""
            )
            
            let decryptedMessage: TextMessageRecord
            do {
                decryptedMessage = try crypto.decrypt(packet: encryptedPacket)
            } catch {
                print("[QuicClient] Failed to decrypt text message: \(error)")
                return
            }
            
            // Find sender name from usersByChannel
            var senderName = "User \(senderSessionId)"
            
            // Check if it's from us
            if senderSessionId == sessionId || senderSessionId == userId {
                senderName = UserDefaults.standard.string(forKey: "AuraDisplayName") ?? "You"
            } else {
                // Look up in usersByChannel
                if let users = usersByChannel[channelId] {
                    print("[QuicClient] Looking for sender \(senderSessionId) in channel \(channelId) with \(users.count) users: \(users.map { "\($0.id):\($0.displayName)" }.joined(separator: ", "))")
                    if let user = users.first(where: { $0.id == senderSessionId }) {
                        senderName = user.displayName
                    } else {
                        print("[QuicClient] Sender \(senderSessionId) NOT FOUND in usersByChannel[\(channelId)]")
                    }
                }
            }
            
            print("[QuicClient] Decrypted text from session \(senderSessionId) (msgId: \(messageId)), resolved to: \(senderName), replyTo: \(replyToId ?? "nil")")
            
            let message = ReceivedTextMessage(
                id: messageId,
                senderSessionId: senderSessionId,
                senderName: senderName,
                channelId: channelId,
                content: decryptedMessage.content,
                timestamp: Date(timeIntervalSince1970: TimeInterval(decryptedMessage.timestamp) / 1000),
                rawPacket: packetData,
                replyToId: replyToId
            )
            
            await MainActor.run {
                receivedMessages.append(message)
                print("[QuicClient] Received encrypted text message from \(senderName): \(decryptedMessage.content.prefix(30))")
            }
        } catch {
            print("[QuicClient] Failed to parse/decrypt TextPacket: \(error)")
        }
    }
    
    /// Handle incoming audio packet from server
    private func handleAudioPacket(stream: NWConnection) async {
        do {
            // Read length (4 bytes)
            let lenData = try await receive(on: stream, minimumLength: 4, maximumLength: 4)
            let packetLen = lenData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            
            // Read audio packet data
            let packetData = try await receive(on: stream, minimumLength: Int(packetLen), maximumLength: Int(packetLen))
            
            print("[QuicClient] Received audio packet: \(packetLen) bytes")
            
            // Process the audio packet (decrypt + decode + play)
            await MainActor.run {
                processIncomingAudioPacket(packetData)
            }
        } catch {
            print("[QuicClient] Failed to parse audio packet: \(error)")
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
    
    // MARK: - Audio
    
    /// Send audio frame via QUIC datagram (or control stream for now)
    /// - Parameter rawPcmBytes: Raw PCM data from AudioCapture (Int16 samples)
    public func sendAudioDatagram(_ floatPcm: [Float]) async throws {
        guard isAuthenticated, let sender = audioSender else { return }
        
        // Process through audio sender (Opus + Encrypt) using high-fidelity float path
        let packetData: Data
        do {
            packetData = try Data(sender.processFloat(pcm: floatPcm))
        } catch {
            print("[QuicClient] Audio encoding error: \(error)")
            return
        }
        
        // Send via QUIC datagram (unreliable, low-latency)
        guard let group = quicGroup else {
            print("[QuicClient] Cannot send audio: no connection group")
            return
        }
        
        var datagram = Data([0x01])  // Audio datagram type
        datagram.append(packetData)
        
        group.send(content: datagram) { [weak self] error in
            if let error = error {
                print("[QuicClient] ✗ Failed to send audio datagram: \(error)")
            } else if let self = self, self.sequenceNumber % 100 == 0 {
                print("[QuicClient] ✓ Sent audio packet #\(sender.sequence()) (67 bytes) via datagram")
            }
        }
        
        sequenceNumber &+= 1
    }
    
    /// Handle incoming audio packet and play it
    /// - Parameter packetData: Raw packet bytes from network
    private func processIncomingAudioPacket(_ packetData: Data) {
        guard let receiver = audioReceiver else { return }
        
        // Pass to receiver for decryption + decoding
        do {
            try receiver.onPacket(data: packetData)
            
            // Pop mixed audio from Rust core (handles PLC/DRED/talking detection internals)
            if let mixed = receiver.popMixed() {
                print("[QuicClient] ✓ Playing mixed audio buffer")
                audioPlayback.enqueue(pcm: mixed)
                
                // Update talking indicators
                // TODO: Get session IDs from the mixed frame metadata
                let now = Date()
                // For now, we'll mark all known senders as potentially active
                // This is a simplification - ideally we'd track which senders contributed to this mix
            }
        } catch {
            print("[QuicClient] Audio processing error: \(error)")
        }
    }
    
    /// Start audio capture and send frames to network
    private func startAudioCapture() async {
        guard let sender = audioSender else { return }
        
        print("[QuicClient] Starting audio capture...")
        
        // Import AudioCapture if needed
        let capture = AudioCapture()
        
        capture.start { [weak self] pcmData in
            guard let self = self else { return }
            Task {
                do {
                    try await self.sendAudioDatagram(pcmData)
                } catch {
                    // Silently ignore send errors
                }
            }
        }
        
        print("[QuicClient] Audio capture started and sending packets")
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
    
    // MARK: - Text Messaging
    
    /// Send a text message to the current channel
    public func sendTextMessage(_ content: String, messageId: String, replyToId: String? = nil) async throws {
        guard let stream = controlStream else {
            throw QuicClientError.notConnected
        }
        guard let channelId = currentChannelId else {
            throw QuicClientError.protocolError("Not in a channel")
        }
        guard let crypto = textCrypto else {
            throw QuicClientError.protocolError("Text crypto not initialized")
        }
        
        // Use sessionId if available, otherwise fall back to userId
        let senderSessionId = sessionId ?? userId
        
        print("[QuicClient] Sending encrypted text message to channel \(channelId): \(content.prefix(30))...")
        print("[QuicClient] ID: \(messageId), Session: \(senderSessionId) (replyTo: \(replyToId ?? "nil"))")
        
        // Create plaintext message record
        let textMsg = TextMessageRecord(
            senderUuid: "user-\(senderSessionId)",  // TODO: Use real UUID from identity
            timestamp: UInt64(Date().timeIntervalSince1970 * 1000),
            content: content,
            replyToId: replyToId ?? "",
            messageId: messageId
        )
        
        // Encrypt using DAVE
        let encryptedPacket = try crypto.encrypt(
            epoch: 0,  // TODO: Use actual text epoch from MLS
            channelId: channelId,
            senderSessionId: senderSessionId,
            message: textMsg
        )
        
        // Serialize encrypted packet to binary format
        // Format: sender_session_id(4) + channel_id(4) + epoch(8) + message_id_len(1) + message_id + content_len(4) + ciphertext + nonce(24) + tag(16) + reply_len(1) + reply_id
        var packet = Data()
        packet.append(contentsOf: withUnsafeBytes(of: encryptedPacket.senderSessionId.littleEndian) { Data($0) })
        packet.append(contentsOf: withUnsafeBytes(of: encryptedPacket.channelId.littleEndian) { Data($0) })
        packet.append(contentsOf: withUnsafeBytes(of: encryptedPacket.epoch.littleEndian) { Data($0) })
        
        // Message ID (length-prefixed)
        let messageIdData = messageId.data(using: .utf8) ?? Data()
        packet.append(UInt8(messageIdData.count))
        packet.append(messageIdData)
        
        // Ciphertext (encrypted content)
        packet.append(contentsOf: withUnsafeBytes(of: UInt32(encryptedPacket.ciphertext.count).littleEndian) { Data($0) })
        packet.append(Data(encryptedPacket.ciphertext))
        
        // Nonce (24 bytes from encryption)
        packet.append(Data(encryptedPacket.nonce))
        
        // Tag (16 bytes from encryption)
        packet.append(Data(encryptedPacket.tag))
        
        // Reply-to ID (length-prefixed)
        if let replyId = replyToId, let replyData = replyId.data(using: .utf8), replyData.count <= 255 {
            packet.append(UInt8(replyData.count))
            packet.append(replyData)
        } else {
            packet.append(UInt8(0))  // No reply
        }
        
        // Build message: 0x30 + length(4) + packet
        var message = Data([Self.MSG_TEXT_PACKET])
        message.append(contentsOf: withUnsafeBytes(of: UInt32(packet.count).littleEndian) { Data($0) })
        message.append(packet)

        
        try await send(data: message, on: stream)
        print("[QuicClient] Sent text message (\(message.count) bytes)")
    }
    
    // MARK: - Disconnect
    
    public func disconnect() {
        stopKeepalive()
        stopListening()
        controlStream?.cancel()
        quicGroup?.cancel()
        controlStream = nil
        quicGroup = nil
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

// MARK: - Received Text Message Model

/// Represents a received text message from the server
public struct ReceivedTextMessage: Identifiable, Equatable {
    public let id: String
    public let senderSessionId: UInt32
    public let senderName: String
    public let channelId: UInt32
    public let content: String
    public let timestamp: Date
    public let rawPacket: Data  // For future decryption
    public let replyToId: String?  // ID of message being replied to
    
    public var formattedTime: String {
        let formatter = DateFormatter()
        formatter.timeStyle = .short
        return formatter.string(from: timestamp)
    }
}

public struct SystemEvent: Identifiable, Equatable {
    public let id = UUID()
    public let content: String
    public let timestamp = Date()
    public let channelId: UInt32 // 0 for global
    
    public init(content: String, channelId: UInt32 = 0) {
        self.content = content
        self.channelId = channelId
    }
}
