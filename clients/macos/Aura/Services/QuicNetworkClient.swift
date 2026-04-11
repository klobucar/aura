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
    public var isAdmin = false
    public var connectionStatus = "Disconnected"
    public var userId: UInt32 = 0
    public var sessionToken: String?
    public var currentChannelId: UInt32?
    public var sessionId: UInt32?  // Our own session ID
    
    public var isMuted = false
    public var isDeafened = false
    
    /// Users by channel ID (tracks all channels, not just current)
    public var usersByChannel: [UInt32: [ChannelUser]] = [:]
    
    /// All channels on the server
    public var channels: [ChannelModel] = []
    
    /// All user profiles by session ID
    public var profiles: [UInt32: UserProfileRecord] = [:]
    
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
    
    // Text crypto now uses MLS-derived per-sender keys (no shared instance)
    
    /// MLS wrapper (Rust UniFFI wrapper for MLS group management and key derivation)
    private var mlsWrapper: MlsWrapper?
    
    /// Track the current voice channel for MLS group ID generation
    private var currentVoiceChannelId: UInt32?
    
    /// Audio playback engine
    private let audioPlayback = AudioPlayback()
    
    /// Track which users are currently speaking (for UI indicators)
    public var activeSpeakers: Set<UInt32> = []
    
    /// Timeout tasks for clearing speakers who stop sending audio
    private var speakerTimeouts: [UInt32: Task<Void, Never>] = [:]
    
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
    private static let MSG_AUDIO_STREAM: UInt8 = 0x20
    private static let MSG_TEXT_PACKET: UInt8 = 0x30
    private static let MSG_CREATE_CHANNEL: UInt8 = 0x40
    private static let MSG_UPDATE_CHANNEL: UInt8 = 0x41
    private static let MSG_UPDATE_PROFILE: UInt8 = 0x42
    private static let MSG_UPDATE_STATUS: UInt8 = 0x45
    
    // MLS Protocol message types
    private static let MSG_MLS_JOIN: UInt8 = 0x50           // Client sends key package
    private static let MSG_MLS_COMMIT_WELCOME: UInt8 = 0x51 // Client sends commit + welcome
    private static let MSG_MLS_CREATE_GROUP: UInt8 = 0x52   // Server tells client to create group
    private static let MSG_MLS_ADD_MEMBER_REQ: UInt8 = 0x53 // Server forwards key package
    private static let MSG_MLS_COMMIT: UInt8 = 0x54         // Server broadcasts commit
    private static let MSG_MLS_WELCOME: UInt8 = 0x55        // Server sends welcome to new member
    
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
    
    // MARK: - Retry State
    
    /// Current retry attempt count
    public var retryCount: Int = 0
    
    /// Maximum number of retry attempts
    public var maxRetries: Int = 5
    
    /// Whether auto-reconnect is enabled
    public var autoReconnectEnabled: Bool = true
    
    /// Whether we are currently retrying connection
    public var isRetrying: Bool = false
    
    /// Task for scheduled reconnection
    private var reconnectTask: Task<Void, Never>?
    
    /// Saved connection parameters for retry
    private var savedHost: String?
    private var savedPort: UInt16?
    private var savedIdentity: UserIdentity?
    private var savedPassword: String?
    
    public init() {
        // Listen for audio settings changes
        NotificationCenter.default.addObserver(
            forName: .audioSettingsChanged,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            self?.applyAudioSettings(notification.object as? [String: Any])
        }
    }
    
    deinit {
        NotificationCenter.default.removeObserver(self)
    }
    
    // MARK: - Audio Settings
    
    private func applyAudioSettings(_ settings: [String: Any]?) {
        guard let settings = settings else { return }
        
        if let enabled = settings["noiseSuppression"] as? Bool {
            audioSender?.setNoiseSuppressionEnabled(enabled: enabled)
            print("[QuicClient] Noise suppression: \(enabled ? "enabled" : "disabled")")
        }
        
        if let enabled = settings["aecEnabled"] as? Bool {
            audioSender?.setWebrtcAecEnabled(enabled: enabled)
            print("[QuicClient] AEC: \(enabled ? "enabled" : "disabled")")
        }
        
        if let enabled = settings["webrtcNsEnabled"] as? Bool {
            audioSender?.setWebrtcNsEnabled(enabled: enabled)
            print("[QuicClient] WebRTC NS: \(enabled ? "enabled" : "disabled")")
        }
        
        if let enabled = settings["webrtcAgcEnabled"] as? Bool {
            audioSender?.setWebrtcAgcEnabled(enabled: enabled)
            print("[QuicClient] AGC: \(enabled ? "enabled" : "disabled")")
        }
        
        if let ms = settings["jitterBuffer"] as? Int {
            audioReceiver?.setJitterBufferMs(latencyMs: UInt32(ms))
            print("[QuicClient] Jitter buffer set to \(ms)ms")
        }
    }
    
    // MARK: - Connection
    
    /// Connect to the Aura server via QUIC using NWConnectionGroup.
    /// This allows accepting server-initiated streams.
    public func connect(host: String, port: UInt16 = 8443) async throws {
        // Save connection parameters for retry
        savedHost = host
        savedPort = port
        
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
                        
                        // Trigger reconnection if this was an unexpected disconnect
                        if self?.isAuthenticated == true {
                            self?.handleConnectionLoss()
                        }
                        
                        continuation.resume(throwing: error)
                    case .cancelled:
                        print("[QuicClient] Group cancelled")
                        self?.isConnected = false
                        if !resumed {
                            resumed = true
                            
                            // Trigger reconnection if this was an unexpected disconnect
                            if self?.isAuthenticated == true {
                                self?.handleConnectionLoss()
                            }
                            
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
                    
                    // Trigger reconnection if we were authenticated
                    if self?.isAuthenticated == true {
                        self?.handleConnectionLoss()
                    }
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
        
        // Save authentication parameters for retry
        savedIdentity = identity
        savedPassword = serverPassword
        
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
        
        // Step 4: Receive auth response header (7 bytes)
        print("[QuicClient] Waiting for auth response header...")
        let header = try await receive(on: stream, minimumLength: 7, maximumLength: 7)
        
        guard header[0] == Self.MSG_AUTH_RESPONSE else {
            throw QuicClientError.protocolError("Invalid auth response header: expected 0x04, got 0x\(String(format: "%02X", header[0]))")
        }
        
        let success = header[1] != 0
        let responseUserId = header.subdata(in: 2..<6).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
        let tokenLen = Int(header[6])
        
        // Read token
        var finalToken = ""
        if tokenLen > 0 {
            let tokenData = try await receive(on: stream, minimumLength: tokenLen, maximumLength: tokenLen)
            finalToken = String(data: tokenData, encoding: .utf8) ?? ""
        }
        
        // Read the rest of fixed fields: verified (1) + isAdmin (1) + errorLen (1)
        let restFixed = try await receive(on: stream, minimumLength: 3, maximumLength: 3)
        let verified = restFixed[0] != 0
        let isAdmin = restFixed[1] != 0
        let errorLen = Int(restFixed[2])
        
        // Read error message
        var errorMsg: String? = nil
        if errorLen > 0 {
            let errorData = try await receive(on: stream, minimumLength: errorLen, maximumLength: errorLen)
            errorMsg = String(data: errorData, encoding: .utf8)
        }
        
        print("[QuicClient] Auth response: success=\(success), userId=\(responseUserId), token=\(finalToken.prefix(8))..., verified=\(verified), isAdmin=\(isAdmin)")
        
        guard success else {
            throw QuicClientError.authenticationFailed(errorMsg ?? "Unknown error")
        }
        
        self.userId = responseUserId
        self.sessionToken = finalToken
        self.isAuthenticated = true
        self.isAdmin = isAdmin
        
        // Use userId as sessionId immediately (server sends the real session ID now)
        self.sessionId = responseUserId
        print("[QuicClient] Session ID set to: \(responseUserId) from auth response")
        self.connectionStatus = "Authenticated as user \(userId)" + (verified ? " (verified)" : "")
        
        print("[QuicClient] ✓ Authentication SUCCESS!")
        
        // Initialize MLS wrapper with user identity
        do {
            mlsWrapper = try MlsWrapper(identityName: finalToken)
            print("[QuicClient] MLS wrapper initialized for E2EE")
        } catch {
            print("[QuicClient] Failed to initialize MLS: \(error) - E2EE will not be available")
        }
        
        // Initialize audio pipeline with temporary key (will be updated with MLS-derived key on channel join)
        if let tokenData = finalToken.data(using: .utf8) {
            // Pad/truncate to 32 bytes for ChaCha20 key
            var keyData = Data(count: 32)
            let copyCount = min(tokenData.count, 32)
            keyData.replaceSubrange(0..<copyCount, with: tokenData[0..<copyCount])
            
            do {
                // Initialize audio sender/receiver with temporary DAVE key
                // This will be replaced with MLS-derived key when joining a channel
                let tempKeyData = Data(repeating: 0x42, count: 32)
                audioSender = try AudioSenderWrapper(sessionId: responseUserId, key: tempKeyData)
                audioSender?.setEpoch(epoch: 0)
                
                // Enable DRED (10 frames = 100ms of redundancy)
                audioSender?.setDredDuration(duration: 10)
                print("[QuicClient] DRED enabled (100ms redundancy)")
                
                // Initialize audio receiver (for receiving others' voice)
                audioReceiver = try AudioReceiverWrapper()
                
                // Apply saved audio settings
                let noiseSuppressionEnabled = UserDefaults.standard.object(forKey: "noiseSuppressionEnabled") as? Bool ?? true
                let aecEnabled = UserDefaults.standard.bool(forKey: "aecEnabled")
                let webrtcNsEnabled = UserDefaults.standard.bool(forKey: "webrtcNsEnabled")
                let webrtcAgcEnabled = UserDefaults.standard.object(forKey: "webrtcAgcEnabled") as? Bool ?? true
                let jitterBufferMs = UserDefaults.standard.object(forKey: "jitterBufferMs") as? Int ?? 20
                
                audioSender?.setNoiseSuppressionEnabled(enabled: noiseSuppressionEnabled)
                audioSender?.setWebrtcAecEnabled(enabled: aecEnabled)
                audioSender?.setWebrtcNsEnabled(enabled: webrtcNsEnabled)
                audioSender?.setWebrtcAgcEnabled(enabled: webrtcAgcEnabled)
                
                audioReceiver?.setJitterBufferMs(latencyMs: UInt32(jitterBufferMs))
                
                print("[QuicClient] Applied settings: RNNoise=\(noiseSuppressionEnabled), AEC=\(aecEnabled), WebRTC-NS=\(webrtcNsEnabled), AGC=\(webrtcAgcEnabled), Jitter=\(jitterBufferMs)ms")
                
                // Text crypto will use MLS-derived keys per-sender (no initialization needed)
                
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
    
    /// Update user profile
    public func updateProfile(bio: String, avatarData: Data) async {
        guard let sessionId = self.sessionId else { return }
        
        let record = UserProfileRecord(
            userId: sessionId,
            displayName: profiles[sessionId]?.displayName ?? "Unknown",
            bio: bio,
            avatarData: avatarData,
            signature: Data(), // Server handles signature for now or we do it in Rust
            signingKey: Data()
        )
        
        let payload = encodeUpdateProfile(profile: record)
        
        // Send MSG_UPDATE_PROFILE (0x42)
        let mutStream: NWConnection? = await MainActor.run { self.controlStream }
        guard let stream = mutStream else { return }
        
        var msg = Data([Self.MSG_UPDATE_PROFILE])
        let len = UInt32(payload.count).littleEndian
        msg.append(Data(withUnsafeBytes(of: len) { Array($0) }))
        msg.append(Data(payload))
        
        do {
            try await send(data: msg, on: stream)
            print("[QuicClient] Profile update sent")
        } catch {
            print("[QuicClient] Failed to send profile update: \(error)")
        }
    }
    
    /// Create a new channel (Admin only)
    public func createChannel(name: String, comment: String, emoji: String? = nil, presetId: String? = nil) async {
        guard isAdmin else { return }
        
        let icon = ChannelIconRecord(emoji: emoji, presetId: presetId, customData: nil)
        let payload = encodeCreateChannel(name: name, comment: comment, icon: icon)
        
        let mutStream: NWConnection? = await MainActor.run { self.controlStream }
        guard let stream = mutStream else { return }
        
        var msg = Data([Self.MSG_CREATE_CHANNEL]) // MSG_CREATE_CHANNEL
        let len = UInt32(payload.count).littleEndian
        msg.append(Data(withUnsafeBytes(of: len) { Array($0) }))
        msg.append(Data(payload))
        
        do {
            try await send(data: msg, on: stream)
            print("[QuicClient] Create channel request sent")
        } catch {
            print("[QuicClient] Failed to send create channel request: \(error)")
        }
    }
    
    /// Update channel metadata (Admin only)
    public func updateChannel(id: UInt32, name: String? = nil, comment: String? = nil, emoji: String? = nil, presetId: String? = nil, position: Int32? = nil) async {
        guard isAdmin else { return }
        
        let icon = (emoji != nil || presetId != nil) ? ChannelIconRecord(emoji: emoji, presetId: presetId, customData: nil) : nil
        let payload = encodeUpdateChannel(channelId: id, name: name, comment: comment, icon: icon, position: position)
        
        let mutStream: NWConnection? = await MainActor.run { self.controlStream }
        guard let stream = mutStream else { return }
        
        var msg = Data([Self.MSG_UPDATE_CHANNEL]) // MSG_UPDATE_CHANNEL
        let len = UInt32(payload.count).littleEndian
        msg.append(Data(withUnsafeBytes(of: len) { Array($0) }))
        msg.append(Data(payload))
        
        do {
            try await send(data: msg, on: stream)
            print("[QuicClient] Update channel request sent")
        } catch {
            print("[QuicClient] Failed to send update channel request: \(error)")
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
            await handleServerState(stream: stream)
            
        case 0x20: // MSG_AUDIO - audio packet from server
            await handleAudioPacket(stream: stream)
            
        case 0x21: // MSG_AUDIO_STREAM - ignore, handled elsewhere
            break
            
        case Self.MSG_TEXT_PACKET: // 0x30
            await handleTextPacket(stream: stream)
            
        // MLS Protocol handlers
        case Self.MSG_MLS_CREATE_GROUP: // 0x52 - Server tells us to create group
            await handleMlsCreateGroup(stream: stream)
            
        case Self.MSG_MLS_ADD_MEMBER_REQ: // 0x53 - Server forwards key package for us to add
            await handleMlsAddMemberRequest(stream: stream)
            
        case Self.MSG_MLS_COMMIT: // 0x54 - Commit from another member
            await handleMlsCommit(stream: stream)
            
        case Self.MSG_MLS_WELCOME: // 0x55 - Welcome message from founder
            await handleMlsWelcome(stream: stream)
            
        case Self.MSG_UPDATE_STATUS: // 0x45
            await handleUserStatusUpdate(stream: stream)
            
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
                    
                    // Add sender to audio receiver for decryption
                    if let receiver = self.audioReceiver, let mls = self.mlsWrapper {
                        // Derive actual DAVE key from MLS if we are in the group
                        let keyData: Data
                        if mls.isMember(channelId: channelId, isVoice: true) {
                            do {
                                let keyBytes = try mls.exportAudioKey(channelId: channelId, senderSessionId: sessionId)
                                let epoch = try mls.currentEpoch(channelId: channelId, isVoice: true)
                                keyData = Data(keyBytes)
                                try receiver.addSender(sessionId: sessionId, key: keyData, epochHint: UInt16(epoch & 0xFFFF))
                                print("[QuicClient] Added audio sender \(sessionId) with MLS key")
                            } catch {
                                print("[QuicClient] Failed to derive MLS key for new user \(sessionId): \(error)")
                            }
                        } else {
                            print("[QuicClient] Not an MLS member for channel \(channelId), waiting for epoch advance to add sender \(sessionId)")
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
    
    /// Handle ServerState snapshot (Protobuf via UniFFI)
    private func handleServerState(stream: NWConnection) async {
        do {
            // Read length (4 bytes)
            let lengthData = try await receive(on: stream, minimumLength: 4, maximumLength: 4)
            let length = lengthData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            
            print("[QuicClient] Receiving ServerState: \(length) bytes")
            
            // Read payload
            let payload = try await receive(on: stream, minimumLength: Int(length), maximumLength: Int(length))
            
            // Decode via Rust core
            let snapshot = try decodeServerState(data: payload)
            
            // Update models
            await MainActor.run {
                self.channels = snapshot.channels.map { ChannelModel(record: $0) }
                
                var newProfiles: [UInt32: UserProfileRecord] = [:]
                for p in snapshot.profiles {
                    newProfiles[p.userId] = p
                }
                self.profiles = newProfiles
                
                // Re-map usersByChannel (exclude ourselves)
                var newUserMapping: [UInt32: [ChannelUser]] = [:]
                for c in snapshot.channels {
                    let users = c.userIds.compactMap { sid -> ChannelUser? in
                        // Don't include ourselves in the user list
                        guard sid != self.sessionId else { return nil }
                        guard let p = self.profiles[sid] else { return nil }
                        return ChannelUser(sessionId: sid, displayName: p.displayName, bio: p.bio, avatarData: Data(p.avatarData))
                    }
                    newUserMapping[c.channelId] = users
                    
                    // Add listeners for decryption
                    if let receiver = self.audioReceiver, let mls = self.mlsWrapper {
                        for sid in c.userIds where sid != self.sessionId {
                            if mls.isMember(channelId: c.channelId, isVoice: true) {
                                do {
                                    let keyBytes = try mls.exportAudioKey(channelId: c.channelId, senderSessionId: sid)
                                    let epoch = try mls.currentEpoch(channelId: c.channelId, isVoice: true)
                                    try receiver.addSender(sessionId: sid, key: Data(keyBytes), epochHint: UInt16(epoch & 0xFFFF))
                                    print("[QuicClient] Added receiver key for user \(sid) from snapshot")
                                } catch {
                                    print("[QuicClient] Failed to derive snapshot key for \(sid): \(error)")
                                }
                            }
                        }
                    }
                }
                self.usersByChannel = newUserMapping
                
                print("[QuicClient] ServerState sync complete: \(self.channels.count) channels, \(self.profiles.count) profiles")
            }
        } catch {
            print("[QuicClient] Failed to parse ServerState: \(error)")
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
            
            // Decrypt the message using MLS-derived key for the sender
            guard let mls = mlsWrapper else {
                print("[QuicClient] MLS not initialized, cannot decrypt text")
                return
            }
            
            // Derive decryption key from MLS text group for this sender
            let senderKey: Data
            do {
                let keyBytes = try mls.exportTextKey(channelId: channelId, senderSessionId: senderSessionId)
                senderKey = Data(keyBytes)
            } catch {
                print("[QuicClient] Failed to derive text key for sender \(senderSessionId): \(error)")
                return
            }
            
            // Create crypto wrapper with sender's key
            let crypto = try TextCryptoWrapper(key: senderKey)
            
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
    
    // MARK: - MLS Protocol Handlers
    
    /// Send MLS join with key package when joining a channel
    private func sendMlsJoin(channelId: UInt32, isVoice: Bool) async {
        guard let stream = controlStream else { return }
        guard let mls = mlsWrapper else {
            print("[QuicClient] MLS not initialized, cannot join with E2EE")
            return
        }
        
        do {
            let keyPackage = try mls.createKeyPackage()
            
            // [0x50] [channel_id: u32] [is_voice: u8] [kp_len: u32] [key_package]
            var msg = Data([Self.MSG_MLS_JOIN])
            msg.append(withUnsafeBytes(of: channelId.littleEndian) { Data($0) })
            msg.append(isVoice ? 1 : 0)
            msg.append(withUnsafeBytes(of: UInt32(keyPackage.count).littleEndian) { Data($0) })
            msg.append(Data(keyPackage))
            
            try await send(data: msg, on: stream)
            print("[QuicClient] Sent MLS join for \(isVoice ? "voice" : "text") channel \(channelId) (\(keyPackage.count) bytes)")
        } catch {
            print("[QuicClient] Failed to send MLS join: \(error)")
        }
    }
    
    /// Handle server telling us to create a new MLS group (we're the first joiner)
    private func handleMlsCreateGroup(stream: NWConnection) async {
        do {
            // [channel_id: u32] [is_voice: u8]
            let data = try await receive(on: stream, minimumLength: 5, maximumLength: 5)
            let channelId = data.prefix(4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let isVoice = data[4] != 0
            
            guard let mls = mlsWrapper else {
                print("[QuicClient] MLS not initialized")
                return
            }
            
            try mls.createGroup(channelId: channelId, isVoice: isVoice)
            print("[QuicClient] Created MLS \(isVoice ? "voice" : "text") group for channel \(channelId)")
            
            // Update our own audio sender key if we're the founder
            if isVoice, let session = sessionId {
                try updateAudioKeysFromMls(channelId: channelId)
                print("[QuicClient] Updated audio keys from MLS as founder")
            }
        } catch {
            print("[QuicClient] Failed to create MLS group: \(error)")
        }
    }
    
    /// Handle server forwarding a key package for us to add (we're a founder or authorized member)
    private func handleMlsAddMemberRequest(stream: NWConnection) async {
        do {
            // [channel_id: u32] [is_voice: u8] [joiner_session_id: u32] [uuid_len: u8] [uuid] [kp_len: u32] [key_package]
            var header = try await receive(on: stream, minimumLength: 9, maximumLength: 9)
            let channelId = header.prefix(4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let isVoice = header[4] != 0
            let joinerSessionId = header.subdata(in: 5..<9).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            
            let uuidLen = try await receive(on: stream, minimumLength: 1, maximumLength: 1)[0]
            let uuidData = try await receive(on: stream, minimumLength: Int(uuidLen), maximumLength: Int(uuidLen))
            let joinerUuid = String(data: uuidData, encoding: .utf8) ?? ""
            
            var kpLenData = try await receive(on: stream, minimumLength: 4, maximumLength: 4)
            let kpLen = kpLenData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let keyPackage = try await receive(on: stream, minimumLength: Int(kpLen), maximumLength: Int(kpLen))
            
            guard let mls = mlsWrapper else {
                print("[QuicClient] MLS not initialized")
                return
            }
            
            // Add the member - returns commit and welcome
            let result = try mls.addMember(channelId: channelId, isVoice: isVoice, keyPackageBytes: Data(keyPackage))
            
            print("[QuicClient] Added member \(joinerSessionId) to MLS group, sending commit/welcome")
            
            // Send commit + welcome back to server
            guard let stream = controlStream else { return }
            
            // [0x51] [channel_id: u32] [is_voice: u8] [new_member_session_id: u32]
            //        [commit_len: u32] [commit] [welcome_len: u32] [welcome]
            var msg = Data([Self.MSG_MLS_COMMIT_WELCOME])
            msg.append(withUnsafeBytes(of: channelId.littleEndian) { Data($0) })
            msg.append(isVoice ? 1 : 0)
            msg.append(withUnsafeBytes(of: joinerSessionId.littleEndian) { Data($0) })
            msg.append(withUnsafeBytes(of: UInt32(result.commit.count).littleEndian) { Data($0) })
            msg.append(Data(result.commit))
            msg.append(withUnsafeBytes(of: UInt32(result.welcome.count).littleEndian) { Data($0) })
            msg.append(Data(result.welcome))
            
            try await send(data: msg, on: stream)
            print("[QuicClient] Sent commit/welcome for new member \(joinerSessionId)")
            
            // Update audio keys after epoch advance
            if isVoice {
                try updateAudioKeysFromMls(channelId: channelId)
            }
        } catch {
            print("[QuicClient] Failed to handle MLS add member: \(error)")
        }
    }
    
    /// Handle commit message from another member
    private func handleMlsCommit(stream: NWConnection) async {
        do {
            // [channel_id: u32] [is_voice: u8] [commit_len: u32] [commit]
            let header = try await receive(on: stream, minimumLength: 5, maximumLength: 5)
            let channelId = header.prefix(4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let isVoice = header[4] != 0
            
            var lenData = try await receive(on: stream, minimumLength: 4, maximumLength: 4)
            let commitLen = lenData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let commit = try await receive(on: stream, minimumLength: Int(commitLen), maximumLength: Int(commitLen))
            
            guard let mls = mlsWrapper else { return }
            
            let newEpoch = try mls.processCommit(channelId: channelId, isVoice: isVoice, commitBytes: Data(commit))
            print("[QuicClient] Processed MLS commit, now at epoch \(newEpoch)")
            
            // Update audio keys after epoch advance
            if isVoice {
                try updateAudioKeysFromMls(channelId: channelId)
            }
        } catch {
            print("[QuicClient] Failed to process MLS commit: \(error)")
        }
    }
    
    /// Handle welcome message (we were just added to a group)
    private func handleMlsWelcome(stream: NWConnection) async {
        do {
            // [channel_id: u32] [is_voice: u8] [welcome_len: u32] [welcome]
            let header = try await receive(on: stream, minimumLength: 5, maximumLength: 5)
            let channelId = header.prefix(4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let isVoice = header[4] != 0
            
            var lenData = try await receive(on: stream, minimumLength: 4, maximumLength: 4)
            let welcomeLen = lenData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            let welcome = try await receive(on: stream, minimumLength: Int(welcomeLen), maximumLength: Int(welcomeLen))
            
            guard let mls = mlsWrapper else { return }
            
            try mls.joinGroup(welcomeBytes: Data(welcome))
            print("[QuicClient] Joined MLS \(isVoice ? "voice" : "text") group via Welcome for channel \(channelId)")
            
            // Update audio keys now that we're in the group
            if isVoice {
                try updateAudioKeysFromMls(channelId: channelId)
            }
        } catch {
            print("[QuicClient] Failed to process MLS welcome: \(error)")
        }
    }
    
    /// Update audio sender/receiver keys from MLS
    private func updateAudioKeysFromMls(channelId: UInt32) throws {
        guard let mls = mlsWrapper, let session = sessionId else { return }
        
        // Get our own key for sending
        let myKey = try mls.exportAudioKey(channelId: channelId, senderSessionId: session)
        let epoch = try mls.currentEpoch(channelId: channelId, isVoice: true)
        
        // Update sender with new key
        if let sender = audioSender {
            // Use updateKey to preserve sequence numbers and other state
            try sender.updateKey(key: Data(myKey), epoch: epoch)
            print("[QuicClient] Rotated audio sender key from MLS, epoch=\(epoch)")
        }
        
        // Update receiver keys for all known users
        if let receiver = audioReceiver {
            for (chId, users) in usersByChannel where chId == channelId {
                for user in users {
                    let userKey = try mls.exportAudioKey(channelId: channelId, senderSessionId: user.id)
                    try receiver.updateSenderKey(sessionId: user.id, key: Data(userKey), epochHint: UInt16(epoch & 0xFFFF))
                    print("[QuicClient] Updated receiver key for user \(user.id)")
                }
            }
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
            if let result = receiver.popMixed() {
                // Check if active speakers changed
                let newSpeakers = Set(result.activeSpeakers)
                if newSpeakers != activeSpeakers {
                    print("[QuicClient] Active speakers changed: \(activeSpeakers) -> \(newSpeakers)")
                    activeSpeakers = newSpeakers
                    
                    // Post notification for UI to update (non-blocking)
                    NotificationCenter.default.post(
                        name: .activeSpeakersChanged,
                        object: newSpeakers
                    )
                }
                
                // WORKAROUND: Rust core doesn't have VAD yet - it reports anyone with packets as "active"
                // Manually timeout speakers who stop sending packets (500ms silence detection)
                for speakerId in result.activeSpeakers {
                    // Reset timeout for this speaker
                    speakerTimeouts[speakerId]?.cancel()
                    speakerTimeouts[speakerId] = Task { [weak self] in
                        try? await Task.sleep(nanoseconds: 500_000_000) // 500ms
                        if !Task.isCancelled {
                            await MainActor.run {
                                guard let self = self else { return }
                                self.activeSpeakers.remove(speakerId)
                                self.speakerTimeouts.removeValue(forKey: speakerId)
                                print("[QuicClient] Speaker \(speakerId) timed out (silence detected)")
                                
                                // Post notification
                                NotificationCenter.default.post(
                                    name: .activeSpeakersChanged,
                                    object: self.activeSpeakers
                                )
                            }
                        }
                    }
                }
                
                // Audio processing (immediate, not blocked by UI)
                audioPlayback.enqueue(pcm: result.pcm)
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
        currentVoiceChannelId = channelId
        connectionStatus = "In channel \(channelId)"
        
        // Send MLS join with key package for E2EE (both voice and text groups)
        await sendMlsJoin(channelId: channelId, isVoice: true)
        await sendMlsJoin(channelId: channelId, isVoice: false)
    }
    
    // MARK: - User Status
    
    /// Update own mute/deafen status
    public func updateStatus(isMuted: Bool, isDeafened: Bool) async {
        guard let sessionId = self.sessionId, let stream = controlStream else { return }
        
        let update = UserStatusUpdate(
            sessionId: sessionId,
            isMuted: isMuted,
            isDeafened: isDeafened
        )
        
        do {
            let payload = try encodeUserStatusUpdate(update: update)
            var msg = Data([Self.MSG_UPDATE_STATUS])
            let len = UInt32(payload.count).littleEndian
            msg.append(withUnsafeBytes(of: len) { Data($0) })
            msg.append(payload)
            
            try await send(data: msg, on: stream)
            print("[QuicClient] Sent status update: muted=\(isMuted), deafened=\(isDeafened)")
        } catch {
            print("[QuicClient] Failed to send status update: \(error)")
        }
    }
    
    private func handleUserStatusUpdate(stream: NWConnection) async {
        do {
            // Read length (4 bytes)
            let lenData = try await receive(on: stream, minimumLength: 4, maximumLength: 4)
            let length = lenData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            
            // Read payload
            let payload = try await receive(on: stream, minimumLength: Int(length), maximumLength: Int(length))
            
            // Decode via Rust core
            let update = try decodeUserStatusUpdate(data: payload)
            
            // Update profile state
            await MainActor.run {
                if var profile = profiles[update.sessionId] {
                    // Update profile record (if we want to store it there)
                    // profiles[update.sessionId] = profile
                }
                
                // Update usersByChannel mapping
                for channelId in usersByChannel.keys {
                    if let index = usersByChannel[channelId]?.firstIndex(where: { $0.id == update.sessionId }) {
                        usersByChannel[channelId]?[index].isMuted = update.isMuted
                        usersByChannel[channelId]?[index].isDeafened = update.isDeafened
                        print("[QuicClient] Updated status for user \(update.sessionId) in channel \(channelId): muted=\(update.isMuted), deafened=\(update.isDeafened)")
                    }
                }
            }
        } catch {
            print("[QuicClient] Failed to parse status update: \(error)")
        }
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
        guard let mls = mlsWrapper else {
            throw QuicClientError.protocolError("MLS not initialized")
        }
        
        // Use sessionId if available, otherwise fall back to userId
        let senderSessionId = sessionId ?? userId
        
        print("[QuicClient] Sending encrypted text message to channel \(channelId): \(content.prefix(30))...")
        print("[QuicClient] ID: \(messageId), Session: \(senderSessionId) (replyTo: \(replyToId ?? "nil"))")
        
        // Derive encryption key from MLS text group for our session
        let myKey = try mls.exportTextKey(channelId: channelId, senderSessionId: senderSessionId)
        let epoch = try mls.currentEpoch(channelId: channelId, isVoice: false)
        
        // Create crypto wrapper with our key
        let crypto = try TextCryptoWrapper(key: Data(myKey))
        
        // Create plaintext message record
        let textMsg = TextMessageRecord(
            senderUuid: "user-\(senderSessionId)",  // TODO: Use real UUID from identity
            timestamp: UInt64(Date().timeIntervalSince1970 * 1000),
            content: content,
            replyToId: replyToId ?? "",
            messageId: messageId
        )
        
        // Encrypt using DAVE with MLS-derived key
        let encryptedPacket = try crypto.encrypt(
            epoch: epoch,
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
        
        // Cancel any pending retry
        reconnectTask?.cancel()
        reconnectTask = nil
        isRetrying = false
        retryCount = 0
        
        connectionStatus = "Disconnected"
        print("[QuicClient] Disconnected")
    }
    
    // MARK: - Reconnection
    
    /// Schedule a reconnection attempt with exponential backoff
    private func scheduleReconnect() {
        guard autoReconnectEnabled else {
            print("[QuicClient] Auto-reconnect disabled, not retrying")
            return
        }
        
        guard retryCount < maxRetries else {
            print("[QuicClient] Max retry attempts (\(maxRetries)) reached, giving up")
            connectionStatus = "Disconnected (max retries reached)"
            isRetrying = false
            return
        }
        
        retryCount += 1
        isRetrying = true
        
        // Exponential backoff: 1s, 2s, 4s, 8s, 16s, max 30s
        let baseDelay: TimeInterval = 1.0
        let delay = min(baseDelay * pow(2.0, Double(retryCount - 1)), 30.0)
        
        connectionStatus = "Reconnecting... (attempt \(retryCount)/\(maxRetries))"
        print("[QuicClient] Scheduling reconnect attempt \(retryCount)/\(maxRetries) in \(delay)s")
        
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            
            guard !Task.isCancelled else {
                print("[QuicClient] Reconnect task cancelled")
                return
            }
            
            await self?.attemptReconnect()
        }
    }
    
    /// Attempt to reconnect using saved parameters
    private func attemptReconnect() async {
        guard let host = savedHost, let port = savedPort else {
            print("[QuicClient] No saved connection parameters, cannot reconnect")
            isRetrying = false
            return
        }
        
        print("[QuicClient] Attempting reconnect to \(host):\(port)...")
        
        do {
            // Reconnect
            try await connect(host: host, port: port)
            
            // Re-authenticate if we have saved identity
            if let identity = savedIdentity {
                try await authenticate(identity: identity, serverPassword: savedPassword)
                
                // Success! Reset retry count
                retryCount = 0
                isRetrying = false
                connectionStatus = "Connected (reconnected)"
                print("[QuicClient] ✓ Reconnection successful!")
                
                // Post notification for UI
                NotificationCenter.default.post(name: .connectionRestored, object: nil)
            } else {
                print("[QuicClient] No saved identity, cannot re-authenticate")
                isRetrying = false
            }
        } catch {
            print("[QuicClient] Reconnect attempt failed: \(error)")
            // Schedule next retry
            scheduleReconnect()
        }
    }
    
    /// Handle connection loss and trigger reconnection
    private func handleConnectionLoss() {
        guard isConnected || isAuthenticated else {
            // Already disconnected, don't trigger retry
            return
        }
        
        print("[QuicClient] Connection lost, cleaning up...")
        
        // Clean up connection state
        stopKeepalive()
        stopListening()
        controlStream?.cancel()
        quicGroup?.cancel()
        controlStream = nil
        quicGroup = nil
        isConnected = false
        isAuthenticated = false
        
        connectionStatus = "Disconnected"
        
        // Trigger reconnection
        scheduleReconnect()
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
    
    private func handleAudioSettingsChanged(_ notification: Notification) {
        guard let settings = notification.object as? [String: Any] else { return }
        
        if let ns = settings["noiseSuppression"] as? Bool {
            audioSender?.setNoiseSuppressionEnabled(enabled: ns)
            print("[QuicClient] Runtime: RNNoise=\(ns)")
        }
        if let aec = settings["aecEnabled"] as? Bool {
            audioSender?.setWebrtcAecEnabled(enabled: aec)
            print("[QuicClient] Runtime: AEC=\(aec)")
        }
        if let wns = settings["webrtcNsEnabled"] as? Bool {
            audioSender?.setWebrtcNsEnabled(enabled: wns)
            print("[QuicClient] Runtime: WebRTC-NS=\(wns)")
        }
        if let agc = settings["webrtcAgcEnabled"] as? Bool {
            audioSender?.setWebrtcAgcEnabled(enabled: agc)
            print("[QuicClient] Runtime: AGC=\(agc)")
        }
        if let jitter = settings["jitterBuffer"] as? Int {
            audioReceiver?.setJitterBufferMs(latencyMs: UInt32(jitter))
            print("[QuicClient] Runtime: Jitter=\(jitter)ms")
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
    public let bio: String
    public let avatarData: Data?
    public var isMuted: Bool
    public var isDeafened: Bool
    
    public init(sessionId: UInt32, displayName: String, bio: String = "", avatarData: Data? = nil, isMuted: Bool = false, isDeafened: Bool = false) {
        self.id = sessionId
        self.displayName = displayName
        self.bio = bio
        self.avatarData = avatarData
        self.isMuted = isMuted
        self.isDeafened = isDeafened
    }
}

// MARK: - Channel Model

public struct ChannelModel: Identifiable, Hashable {
    public let id: UInt32
    public let name: String
    public let comment: String
    public let iconEmoji: String?
    public let iconPresetId: String?
    public let iconCustomData: Data?
    public let position: Int32
    
    public init(id: UInt32, name: String, comment: String = "", iconEmoji: String? = nil, iconPresetId: String? = nil, iconCustomData: Data? = nil, position: Int32 = 0) {
        self.id = id
        self.name = name
        self.comment = comment
        self.iconEmoji = iconEmoji
        self.iconPresetId = iconPresetId
        self.iconCustomData = iconCustomData
        self.position = position
    }
    
    public init(record: ChannelInfoRecord) {
        self.id = record.channelId
        self.name = record.name
        self.comment = record.comment
        self.iconEmoji = record.icon?.emoji
        self.iconPresetId = record.icon?.presetId
        self.iconCustomData = record.icon?.customData
        self.position = record.position
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
