import Foundation
import Combine
import Network
import Observation
import UserNotifications
import SwiftUI
import CryptoKit

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
    public var currentChannelId: String?
    public var sessionId: UInt32?  // Our own session ID
    
    public var isMuted = false
    public var isDeafened = false
    
    /// Users by channel ID (tracks all channels, not just current)
    public var usersByChannel: [String: [ChannelUser]] = [:]
    
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
    private var currentVoiceChannelId: String?
    
    /// Audio playback engine
    private let audioPlayback = AudioPlayback()
    
    /// Track which users are currently speaking (for UI indicators)
    public var activeSpeakers: Set<UInt32> = []

    /// Local-only per-user playback gain, keyed by stable user UUID.
    /// 1.0 = unchanged, 0.0..2.0 is the UI-allowed range.
    /// Persists across reconnects via UserDefaults. Nothing about this
    /// is sent to the server or to other clients.
    public var userVolumes: [String: Float] = [:]

    /// User UUIDs that the local user has muted for themselves only.
    /// Persists across reconnects via UserDefaults.
    public var locallyMutedUsers: Set<String> = []

    /// Ephemeral session-id → user-uuid index. Populated from UserJoined
    /// broadcasts and from ServerSnapshot channel user lists, torn down
    /// when a user leaves. Used to translate between the wire-level
    /// session identity and the stable UUID-keyed local state above.
    private var sessionToUuid: [UInt32: String] = [:]

    private static let localVolumesDefaultsKey = "AuraLocalVolumes"
    private static let locallyMutedDefaultsKey = "AuraLocallyMutedUsers"
    
    /// Last detected untrusted certificate fingerprint for TOFU prompt
    public var lastUntrustedFingerprint: String?

    /// Rolling round-trip latency to the server, in milliseconds.
    /// `nil` until the first datagram pong has been received (or after a
    /// reset). Driven by the datagram ping loop below.
    public var latencyMs: Int?

    /// Monotonic-clock timestamps of outstanding ping nonces, keyed by the
    /// 8-byte nonce we sent. Pruned on pong receipt or when older than
    /// `pingTimeoutSeconds`.
    private var pendingPings: [UInt64: DispatchTime] = [:]

    /// Consecutive ping losses; if this exceeds `pingLossThreshold` we
    /// assume the server is gone and trigger a reconnect.
    private var consecutivePingLosses: Int = 0

    /// Task running the datagram RTT probe loop.
    private var pingTask: Task<Void, Never>?
    
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
    private static let MSG_PROFILE_UPDATED: UInt8 = 0x46 // Server → clients broadcast
    
    // MLS Protocol message types
    private static let MSG_MLS_JOIN: UInt8 = 0x50           // Client sends key package
    private static let MSG_MLS_COMMIT_WELCOME: UInt8 = 0x51 // Client sends commit + welcome
    private static let MSG_MLS_CREATE_GROUP: UInt8 = 0x52   // Server tells client to create group
    private static let MSG_MLS_ADD_MEMBER_REQ: UInt8 = 0x53 // Server forwards key package
    private static let MSG_MLS_COMMIT: UInt8 = 0x54         // Server broadcasts commit
    private static let MSG_MLS_WELCOME: UInt8 = 0x55        // Server broadcasts welcome
    
    // Security limits
    private static let MAX_AUDIO_PACKET_SIZE = 65536
    private static let MAX_CONTROL_PACKET_SIZE = 2 * 1024 * 1024 // 2MB
    
    // ALPN protocol identifier
    private static let ALPN = "aura-dave"
    
    // Keepalive interval (must be < server timeout of 30s)
    private static let keepaliveInterval: TimeInterval = 10.0

    // Datagram RTT probe cadence and loss policy.
    private static let pingInterval: TimeInterval = 5.0
    private static let pingTimeoutSeconds: TimeInterval = 15.0
    private static let pingLossThreshold: Int = 3
    
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
    
    /// Guards against reconnection when user intentionally disconnects.
    /// Set to true BEFORE cancelling streams/group in disconnect().
    private var isIntentionalDisconnect = false
    
    /// Saved connection parameters for retry
    private var savedHost: String?
    private var savedPort: UInt16?
    private var savedIdentity: UserIdentity?
    private var savedPassword: String?
    
    public init() {
        loadLocalMixerPrefs()

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
        // Clear intentional disconnect flag for fresh connections
        isIntentionalDisconnect = false
        
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
        
        // Strict certificate verification in release, TOFU in dev
        sec_protocol_options_set_verify_block(quicOptions.securityProtocolOptions, { [weak self] _, trust, completeHandler in
            guard let self = self else {
                completeHandler(false)
                return
            }
            
#if DEBUG
            print("[QuicClient] DEBUG: TLS verify - accepting all certificates")
            completeHandler(true)
#else
            let trustRef = sec_trust_copy_ref(trust).takeRetainedValue()
            var error: CFError?
            let isValid = SecTrustEvaluateWithError(trustRef, &error)
            
            if isValid {
                print("[QuicClient] TLS verify - system trust valid")
                completeHandler(true)
                return
            }
            
            // Standard trust failed - perform TOFU check
            guard let cert = SecTrustGetCertificateAtIndex(trustRef, 0) else {
                print("[QuicClient] TLS verify - no certificate found")
                completeHandler(false)
                return
            }
            
            let data = SecCertificateCopyData(cert) as Data
            let hash = SHA256.hash(data: data)
            let fingerprint = hash.compactMap { String(format: "%02x", $0) }.joined()
            
            print("[QuicClient] TLS verify failure - fingerprint: \(fingerprint)")
            
            // Check if user has already trusted this fingerprint for this host
            if AppSettings.shared.isFingerprintTrusted(host: host, fingerprint: fingerprint) {
                print("[QuicClient] Fingerprint matches known trusted list. Proceeding.")
                completeHandler(true)
            } else {
                print("[QuicClient] Untrusted certificate. Blocking connection.")
                Task { @MainActor in
                    self.lastUntrustedFingerprint = fingerprint
                }
                completeHandler(false)
            }
#endif
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
        
        // Set up handler for incoming datagrams (audio packets + ping echoes)
        group.setReceiveHandler(maximumMessageSize: 1220, rejectOversizedMessages: true) { [weak self] _, content, _ in
            guard let self = self, let data = content, !data.isEmpty else { return }

            // Ping echo from the server: [0x00][8-byte nonce]. A bare
            // 1-byte 0x00 is a legacy server-initiated keepalive; ignore.
            if data[0] == 0x00 {
                if data.count >= 9 {
                    let nonce = data.subdata(in: 1..<9).withUnsafeBytes {
                        $0.load(as: UInt64.self)
                    }
                    Task { @MainActor in
                        self.recordPingEcho(nonce: nonce)
                    }
                }
                return
            }

            if data[0] == 0x01 {  // Audio datagram
                let audioData = data.subdata(in: 1..<data.count)
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
                        
                        // Trigger reconnection only if this was an unexpected disconnect
                        if self?.isAuthenticated == true && self?.isIntentionalDisconnect != true {
                            self?.handleConnectionLoss()
                        }
                        
                        continuation.resume(throwing: error)
                    case .cancelled:
                        print("[QuicClient] Group cancelled")
                        self?.isConnected = false
                        if !resumed {
                            resumed = true
                            
                            // Trigger reconnection only if this was an unexpected disconnect
                            if self?.isAuthenticated == true && self?.isIntentionalDisconnect != true {
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
                    
                    // Trigger reconnection only if we were authenticated and it wasn't intentional
                    if self?.isAuthenticated == true && self?.isIntentionalDisconnect != true {
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
            
            // Start keepalive to prevent session timeout
            startKeepalive()
            
            // Start listening for server messages (presence updates)
            startListening()
        }
    }
    
    // MARK: - Local Mixer Controls (per-user volume / local mute)

    /// Set a local-only volume multiplier for a single remote user.
    /// `volume` is clamped to `[0.0, 2.0]`; 1.0 is unchanged. Nothing
    /// about this is sent to the server — it only affects what this
    /// client hears. Stored per-UUID so it survives reconnects.
    @MainActor
    public func setLocalVolume(sessionId: UInt32, volume: Float) {
        let clamped = max(0.0, min(2.0, volume))
        audioReceiver?.setSenderGain(sessionId: sessionId, gain: clamped)

        guard let uuid = sessionToUuid[sessionId], !uuid.isEmpty else {
            // No stable identity yet — apply to this session only,
            // don't persist. Will snap back on next reconnect.
            return
        }
        if abs(clamped - 1.0) < 0.001 {
            userVolumes.removeValue(forKey: uuid)
        } else {
            userVolumes[uuid] = clamped
        }
        saveLocalMixerPrefs()
    }

    /// Toggle whether a given remote user is locally muted for this
    /// client only. The server and the other user are not told.
    /// Stored per-UUID so it survives reconnects.
    @MainActor
    public func toggleLocalMute(sessionId: UInt32) {
        guard let uuid = sessionToUuid[sessionId], !uuid.isEmpty else {
            // Can't persist without a stable id; fall back to a
            // session-scoped toggle via the Rust mixer directly.
            let nowMuted = !(audioReceiverIsMuted(sessionId: sessionId))
            audioReceiver?.setSenderMuted(sessionId: sessionId, muted: nowMuted)
            return
        }

        if locallyMutedUsers.contains(uuid) {
            locallyMutedUsers.remove(uuid)
            audioReceiver?.setSenderMuted(sessionId: sessionId, muted: false)
        } else {
            locallyMutedUsers.insert(uuid)
            audioReceiver?.setSenderMuted(sessionId: sessionId, muted: true)
        }
        saveLocalMixerPrefs()
    }

    /// Whether the given session id is currently locally muted.
    public func isLocallyMuted(sessionId: UInt32) -> Bool {
        guard let uuid = sessionToUuid[sessionId] else { return false }
        return locallyMutedUsers.contains(uuid)
    }

    /// Current local playback gain (1.0 = unchanged) for a given session.
    public func localVolume(for sessionId: UInt32) -> Float {
        guard let uuid = sessionToUuid[sessionId] else { return 1.0 }
        return userVolumes[uuid] ?? 1.0
    }

    /// Best-effort check: the Rust mixer has no reader for the mute
    /// flag, so we fall back to `false` when there's no UUID mapping.
    /// Only reached in the "no stable id yet" path of toggleLocalMute.
    private func audioReceiverIsMuted(sessionId: UInt32) -> Bool { false }

    /// Push any persisted local volume / mute choices into the Rust
    /// mixer for the given session id. Called right after every
    /// `receiver.addSender(...)` so that re-joining a channel or key
    /// rotation doesn't silently reset prefs, and when the session-to
    /// -UUID mapping is first learned (so a user who is muted by UUID
    /// gets muted immediately after they register in the current
    /// connection).
    fileprivate func applyLocalMixerPrefs(sessionId: UInt32) {
        guard let receiver = audioReceiver else { return }
        guard let uuid = sessionToUuid[sessionId], !uuid.isEmpty else { return }
        if let vol = userVolumes[uuid] {
            receiver.setSenderGain(sessionId: sessionId, gain: vol)
        }
        if locallyMutedUsers.contains(uuid) {
            receiver.setSenderMuted(sessionId: sessionId, muted: true)
        }
    }

    /// Register the session → UUID mapping the server just told us
    /// about and immediately honour any previously-persisted local
    /// volume / mute for that user.
    fileprivate func registerSessionIdentity(sessionId: UInt32, userUuid: String) {
        guard !userUuid.isEmpty else { return }
        sessionToUuid[sessionId] = userUuid
        applyLocalMixerPrefs(sessionId: sessionId)
    }

    /// Forget a session mapping on UserLeft. Local preferences stay
    /// in the UUID-keyed dicts so they re-apply on the user's next
    /// reconnect.
    fileprivate func forgetSessionIdentity(sessionId: UInt32) {
        sessionToUuid.removeValue(forKey: sessionId)
    }

    // MARK: Local Mixer Persistence

    private func loadLocalMixerPrefs() {
        let defaults = UserDefaults.standard
        if let data = defaults.data(forKey: Self.localVolumesDefaultsKey),
           let decoded = try? JSONDecoder().decode([String: Float].self, from: data) {
            userVolumes = decoded
        }
        if let data = defaults.data(forKey: Self.locallyMutedDefaultsKey),
           let decoded = try? JSONDecoder().decode([String].self, from: data) {
            locallyMutedUsers = Set(decoded)
        }
    }

    private func saveLocalMixerPrefs() {
        let defaults = UserDefaults.standard
        if let data = try? JSONEncoder().encode(userVolumes) {
            defaults.set(data, forKey: Self.localVolumesDefaultsKey)
        }
        if let data = try? JSONEncoder().encode(Array(locallyMutedUsers)) {
            defaults.set(data, forKey: Self.locallyMutedDefaultsKey)
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
    public func updateChannel(id: String, name: String? = nil, comment: String? = nil, emoji: String? = nil, presetId: String? = nil, position: Int32? = nil) async {
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

        startPingProbe()

        print("[QuicClient] Keepalive timer started (every \(Self.keepaliveInterval)s)")
    }

    /// Stop the keepalive timer
    private func stopKeepalive() {
        keepaliveTask?.cancel()
        keepaliveTask = nil
        stopPingProbe()
    }

    /// Send a keepalive ping via control stream.
    /// A repeated send failure here is our earliest signal that the server
    /// has vanished, so we hand off to the reconnect path immediately
    /// instead of just logging and looping forever.
    private func sendKeepalivePing() async {
        guard let stream = controlStream else { return }

        let ping = Data([0x00])
        do {
            try await send(data: ping, on: stream)
        } catch {
            print("[QuicClient] Keepalive ping failed: \(error)")
            if self.isAuthenticated && !self.isIntentionalDisconnect {
                await MainActor.run { self.handleConnectionLoss() }
            }
        }
    }

    // MARK: - Datagram RTT Probe

    /// Start the datagram ping loop that measures round-trip latency.
    /// Runs in parallel with the reliable keepalive; pings are unreliable
    /// by design so a single loss does not trip the reconnect path.
    private func startPingProbe() {
        stopPingProbe()
        pendingPings.removeAll()
        consecutivePingLosses = 0

        pingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(Self.pingInterval * 1_000_000_000))
                guard let self = self, self.isAuthenticated else { break }
                await self.sendPingProbe()
            }
        }
    }

    private func stopPingProbe() {
        pingTask?.cancel()
        pingTask = nil
        pendingPings.removeAll()
        consecutivePingLosses = 0
    }

    /// Fire one datagram probe and prune any stale pending entries.
    /// Format: `[0x00][8-byte random nonce]`.
    private func sendPingProbe() async {
        guard let group = quicGroup else { return }

        // Drop any pending probes older than the timeout window and count
        // them as losses. Three in a row = assume the server is gone.
        let nowNs = DispatchTime.now().uptimeNanoseconds
        let windowNs = UInt64(Self.pingTimeoutSeconds * 1_000_000_000)
        let cutoff: UInt64 = nowNs > windowNs ? nowNs - windowNs : 0
        let stale = await MainActor.run { () -> Int in
            let before = self.pendingPings.count
            self.pendingPings = self.pendingPings.filter { $0.value.uptimeNanoseconds >= cutoff }
            let expired = before - self.pendingPings.count
            if expired > 0 {
                self.consecutivePingLosses += expired
            }
            return self.consecutivePingLosses
        }

        if stale >= Self.pingLossThreshold {
            print("[QuicClient] \(stale) consecutive ping losses — treating server as gone")
            if self.isAuthenticated && !self.isIntentionalDisconnect {
                await MainActor.run { self.handleConnectionLoss() }
            }
            return
        }

        let nonce = UInt64.random(in: UInt64.min...UInt64.max)
        var datagram = Data([0x00])
        withUnsafeBytes(of: nonce) { datagram.append(contentsOf: $0) }

        await MainActor.run {
            self.pendingPings[nonce] = DispatchTime.now()
        }

        group.send(content: datagram) { error in
            if let error = error {
                print("[QuicClient] Ping datagram send failed: \(error)")
            }
        }
    }

    /// Called from the datagram receive handler when the server echoes a
    /// ping back. Computes RTT and clears the loss counter.
    @MainActor
    fileprivate func recordPingEcho(nonce: UInt64) {
        guard let sentAt = pendingPings.removeValue(forKey: nonce) else { return }
        let rttNs = DispatchTime.now().uptimeNanoseconds &- sentAt.uptimeNanoseconds
        let rttMs = Int(rttNs / 1_000_000)
        latencyMs = rttMs
        consecutivePingLosses = 0
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

        case Self.MSG_PROFILE_UPDATED: // 0x46 - Server broadcasts a user's profile change
            await handleProfileUpdated(stream: stream)

        default:
            print(String(format: "[QuicClient] Unknown message type: 0x%02X", type))
            break
        }
    }
    
    /// Handle UserJoined message
    private func handleUserJoined(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let join = try decodeUserJoined(data: payload)
            
            let channelId = join.channelId
            let sessionId = join.sessionId
            let displayName = join.displayName
            let userUuid = join.userUuid

            let user = ChannelUser(sessionId: sessionId, displayName: displayName)

            // Add to channel's user list (@Observable tracks this automatically)
            await MainActor.run {
                // Learn the session → UUID mapping and re-apply any
                // persisted local-mixer prefs for this user before they
                // start streaming audio.
                self.registerSessionIdentity(sessionId: sessionId, userUuid: userUuid)

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
                                self.applyLocalMixerPrefs(sessionId: sessionId)
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
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let left = try decodeUserLeft(data: payload)
            
            let channelId = left.channelId
            let sessionId = left.sessionId

            // Remove from audio receiver
            audioReceiver?.removeSender(sessionId: sessionId)

            // Drop the session → UUID mapping. Local prefs stay in
            // the UUID-keyed dicts so they re-apply on reconnect.
            await MainActor.run {
                self.forgetSessionIdentity(sessionId: sessionId)
            }
            
            // Global cleanup across ALL channels to prevent state drift
            await MainActor.run {
                var foundUser: ChannelUser?
                var foundChannelId: String?
                
                // Search all channels for this session
                for (chId, users) in usersByChannel {
                    if let index = users.firstIndex(where: { $0.id == sessionId }) {
                        foundUser = users[index]
                        foundChannelId = chId
                        
                        // Mark as disconnected in the array (Ghost state)
                        withAnimation(.easeOut(duration: 0.5)) {
                            usersByChannel[chId]?[index].isDisconnected = true
                        }
                        break
                    }
                }
                
                var name = foundUser?.displayName ?? "Unknown User"
                
                // If not found in channels, try a profile cache lookup for the session ID
                if foundUser == nil {
                    if let profile = profiles[sessionId] {
                        name = profile.displayName
//                    } else if sessionId == userId {
//                        name = identity?.displayName ?? "You"
                    }
                }
                
                // 1. Post macOS system notification
                let content = UNMutableNotificationContent()
                content.title = "Aura"
                content.body = "\(name) disconnected"
                content.sound = .default
                let request = UNNotificationRequest(identifier: "aura_user_left_\(sessionId)", content: content, trigger: nil)
                UNUserNotificationCenter.current().add(request)
                
                // 2. Add system event to chat
                let event = SystemEvent(content: "\(name) disconnected", channelId: foundChannelId ?? channelId)
                systemEvents.append(event)
                
                // 3. Delayed removal (Ghost cleanup)
                Task {
                    try? await Task.sleep(nanoseconds: 2_000_000_000) // 2 seconds
                    await MainActor.run {
                        if let chId = foundChannelId,
                           let index = usersByChannel[chId]?.firstIndex(where: { $0.id == sessionId }) {
                            withAnimation {
                                usersByChannel[chId]?.remove(at: index)
                            }
                        }
                    }
                }
            }
        } catch {
            print("[QuicClient] Failed to parse UserLeft: \(error)")
        }
    }
    
    /// Handle ServerState snapshot (Protobuf via UniFFI)
    private func handleServerState(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            
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
                var newUserMapping: [String: [ChannelUser]] = [:]
                for c in snapshot.channels {
                    var usersList: [ChannelUser] = []
                    for userStatus in c.users {
                        let sid = userStatus.sessionId

                        // Learn session → UUID for every user in the
                        // snapshot — including ourselves — so local-only
                        // prefs attach to stable identities.
                        self.registerSessionIdentity(sessionId: sid, userUuid: userStatus.userUuid)

                        guard sid != self.sessionId else { continue }
                        usersList.append(ChannelUser(sessionId: sid, displayName: userStatus.displayName))
                    }
                    newUserMapping[c.channelId] = usersList

                    // Add listeners for decryption
                    if let receiver = self.audioReceiver, let mls = self.mlsWrapper {
                        for userStatus in c.users {
                            let sid = userStatus.sessionId
                            if sid != self.sessionId && mls.isMember(channelId: c.channelId, isVoice: true) {
                                do {
                                    let keyBytes = try mls.exportAudioKey(channelId: c.channelId, senderSessionId: sid)
                                    let epoch = try mls.currentEpoch(channelId: c.channelId, isVoice: true)
                                    try receiver.addSender(sessionId: sid, key: Data(keyBytes), epochHint: UInt16(epoch & 0xFFFF))
                                    self.applyLocalMixerPrefs(sessionId: sid)
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
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let packet = try decodeEncryptedTextPacket(data: payload)
            
            let channelId = packet.channelId
            let senderSessionId = packet.senderSessionId
            let epoch = packet.epoch
            let messageId = packet.messageId
            let replyToId = packet.replyToId.isEmpty ? nil : packet.replyToId
            
            // Decrypt the message using MLS-derived key for the sender
            guard let mls = mlsWrapper else {
                print("[QuicClient] MLS not initialized, cannot decrypt text")
                return
            }
            
            // Derive decryption key from MLS text group for this sender
            // The MLS text group may not be established yet if we just joined —
            // wait briefly for the Welcome handshake to complete.
            var senderKey: Data?
            for attempt in 1...10 {
                do {
                    let keyBytes = try mls.exportTextKey(channelId: channelId, senderSessionId: senderSessionId)
                    senderKey = keyBytes
                    break
                } catch {
                    if attempt < 10 {
                        try? await Task.sleep(nanoseconds: 200_000_000) // 200ms
                    } else {
                        print("[QuicClient] Failed to derive text key for sender \(senderSessionId) after \(attempt) attempts: \(error)")
                        return
                    }
                }
            }
            
            guard let senderKey = senderKey else {
                print("[QuicClient] Could not derive text key for sender \(senderSessionId)")
                return
            }
            
            // Create crypto wrapper with sender's key
            let crypto = try TextCryptoWrapper(key: senderKey)
            
            let decryptedMessage: TextMessageRecord
            do {
                decryptedMessage = try crypto.decrypt(packet: packet)
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
                    if let user = users.first(where: { $0.id == senderSessionId }) {
                        senderName = user.displayName
                    }
                }
            }
            
            let message = ReceivedTextMessage(
                id: messageId,
                senderSessionId: senderSessionId,
                senderName: senderName,
                channelId: channelId,
                content: decryptedMessage.content,
                timestamp: Date(timeIntervalSince1970: TimeInterval(decryptedMessage.timestamp) / 1000),
                rawPacket: payload,
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
    private func sendMlsJoin(channelId: String, isVoice: Bool) async {
        guard let stream = controlStream, let mls = mlsWrapper else { return }
        
        do {
            let keyPackage = try mls.createKeyPackage()
            let envelope = MlsEnvelopeRecord(
                senderId: sessionId ?? userId,
                channelId: String(channelId),
                groupType: isVoice ? .voice : .text,
                targetSessionId: 0,
                targetUuid: "",
                epoch: 0,
                keyPackage: keyPackage,
                commit: nil,
                welcome: nil,
                commitWelcome: nil
            )
            
            let payload = encodeMlsEnvelope(envelope: envelope)
            var msg = Data([Self.MSG_MLS_JOIN])
            let len = UInt32(payload.count).littleEndian
            msg.append(withUnsafeBytes(of: len) { Data($0) })
            msg.append(payload)
            
            try await send(data: msg, on: stream)
            print("[QuicClient] Sent MLS join for \(isVoice ? "voice" : "text") channel \(channelId)")
        } catch {
            print("[QuicClient] Failed to send MLS join: \(error)")
        }
    }
    
    /// Handle server telling us to create a new MLS group (we're the first joiner)
    private func handleMlsCreateGroup(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let envelope = try decodeMlsEnvelope(data: payload)
            
            let channelId = envelope.channelId
            let isVoice = envelope.groupType == .voice
            
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
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let envelope = try decodeMlsEnvelope(data: payload)
            
            let channelId = envelope.channelId
            let isVoice = envelope.groupType == .voice
            let joinerSessionId = envelope.targetSessionId
            guard let keyPackage = envelope.keyPackage else { return }
            
            if let mls = mlsWrapper {
                // Add the member - returns commit and welcome
                let result = try mls.addMember(channelId: channelId, isVoice: isVoice, keyPackageBytes: Data(keyPackage))
                print("[QuicClient] Added member \(joinerSessionId) to MLS group, sending commit/welcome")
                
                // Send commit + welcome back to server
                guard let stream = controlStream else { return }
                
                let envelopeOut = MlsEnvelopeRecord(
                    senderId: sessionId ?? userId,
                    channelId: channelId,
                    groupType: envelope.groupType,
                    targetSessionId: 0,
                    targetUuid: "",
                    epoch: 0,
                    keyPackage: nil,
                    commit: nil,
                    welcome: nil,
                    commitWelcome: MlsCommitWelcomeDetailRecord(
                        commit: result.commit,
                        welcome: result.welcome,
                        newMemberSessionId: joinerSessionId
                    )
                )
                
                let responsePayload = encodeMlsEnvelope(envelope: envelopeOut)
                var msg = Data([Self.MSG_MLS_COMMIT_WELCOME])
                let len = UInt32(responsePayload.count).littleEndian
                msg.append(withUnsafeBytes(of: len) { Data($0) })
                msg.append(responsePayload)
                
                try await send(data: msg, on: stream)
                print("[QuicClient] Sent commit/welcome for new member \(joinerSessionId)")
                
                // Update audio keys after epoch advance
                if isVoice {
                    try updateAudioKeysFromMls(channelId: channelId)
                }
            }
        } catch {
            print("[QuicClient] Failed to handle MLS add member: \(error)")
        }
    }
    
    /// Handle commit message from another member
    private func handleMlsCommit(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let envelope = try decodeMlsEnvelope(data: payload)
            
            let channelId = envelope.channelId
            let isVoice = envelope.groupType == .voice
            guard let commit = envelope.commit else { return }
            
            guard let mls = mlsWrapper else { return }
            
            let newEpoch = try mls.processCommit(channelId: channelId, isVoice: isVoice, commitBytes: Data(commit))
            print("[QuicClient] Processed MLS commit from \(envelope.senderId), now at epoch \(newEpoch)")
            
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
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let envelope = try decodeMlsEnvelope(data: payload)
            
            let channelId = envelope.channelId
            let isVoice = envelope.groupType == .voice
            guard let welcome = envelope.welcome else { return }
            
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
    private func updateAudioKeysFromMls(channelId: String) throws {
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
                    do {
                        let userKey = try mls.exportAudioKey(channelId: channelId, senderSessionId: user.id)
                        let updated = try receiver.updateSenderKey(sessionId: user.id, key: Data(userKey), epochHint: UInt16(epoch & 0xFFFF))
                        
                        if updated {
                            print("[QuicClient] Updated receiver key for user \(user.id)")
                        } else {
                            // If update failed, it's a new sender we missed during the initial Join race
                            print("[QuicClient] New user \(user.id) found during key rotation, adding sender...")
                            try receiver.addSender(sessionId: user.id, key: Data(userKey), epochHint: UInt16(epoch & 0xFFFF))
                            self.applyLocalMixerPrefs(sessionId: user.id)
                        }
                    } catch {
                        print("[QuicClient] Failed to update/add audio key for user \(user.id): \(error)")
                    }
                }
            }
        }
    }
    
    /// Handle incoming audio packet from server
    private func handleAudioPacket(stream: NWConnection) async {
        do {
            let packetData = try await receiveHardenedPayload(maxLen: Self.MAX_AUDIO_PACKET_SIZE, on: stream)
            
            print("[QuicClient] Received audio packet: \(packetData.count) bytes")
            
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
    
    public func joinChannel(_ channelId: String) async throws {
        guard let stream = controlStream else {
            throw QuicClientError.notConnected
        }
        
        print("[QuicClient] Joining channel \(channelId)...")
        
        let req = JoinChannelRequestRecord(channelId: String(channelId))
        let payload = encodeJoinChannelRequest(req: req)
        
        var msg = Data([Self.MSG_JOIN_CHANNEL])
        let len = UInt32(payload.count).littleEndian
        msg.append(withUnsafeBytes(of: len) { Data($0) })
        msg.append(payload)
        
        try await send(data: msg, on: stream)
        currentChannelId = channelId
        currentVoiceChannelId = channelId
        let channelName = channels.first(where: { $0.id == channelId })?.name ?? "channel \(channelId)"
        connectionStatus = "In #\(channelName)"
        
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
    
    /// Handle an incoming profile broadcast (bio / avatar / display name)
    /// that the server forwarded from another connected client.
    private func handleProfileUpdated(stream: NWConnection) async {
        do {
            let lenData = try await receive(on: stream, minimumLength: 4, maximumLength: 4)
            let length = lenData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }

            if length == 0 || Int(length) > Self.MAX_CONTROL_PACKET_SIZE {
                print("[QuicClient] Profile broadcast rejected: length \(length) out of bounds")
                return
            }

            let payload = try await receive(on: stream, minimumLength: Int(length), maximumLength: Int(length))
            let profile = try decodeUserProfile(data: payload)

            await MainActor.run {
                self.profiles[profile.userId] = profile

                // Propagate display-name / bio / avatar into the per-channel
                // user lists so speaker labels and avatar thumbnails update
                // without waiting for a full ServerSnapshot round-trip.
                for channelId in self.usersByChannel.keys {
                    if let idx = self.usersByChannel[channelId]?.firstIndex(where: { $0.id == profile.userId }),
                       let existing = self.usersByChannel[channelId]?[idx] {
                        self.usersByChannel[channelId]?[idx] = ChannelUser(
                            sessionId: existing.id,
                            displayName: profile.displayName,
                            bio: profile.bio,
                            avatarData: profile.avatarData.isEmpty ? nil : profile.avatarData,
                            isMuted: existing.isMuted,
                            isDeafened: existing.isDeafened,
                            isDisconnected: existing.isDisconnected
                        )
                    }
                }

                print("[QuicClient] Profile updated for user \(profile.userId) (bio: \(profile.bio.count)B, avatar: \(profile.avatarData.count)B)")
                NotificationCenter.default.post(name: .profileUpdated, object: profile.userId)
            }
        } catch {
            print("[QuicClient] Failed to parse profile update: \(error)")
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
        
        // Wait for MLS text group to be established (the Welcome/CreateGroup handshake
        // completes asynchronously after joinChannel returns).
        var myKey: Data?
        var epoch: UInt64 = 0
        for attempt in 1...15 {
            do {
                myKey = try mls.exportTextKey(channelId: channelId, senderSessionId: senderSessionId)
                epoch = try mls.currentEpoch(channelId: channelId, isVoice: false)
                break
            } catch {
                if attempt < 15 {
                    print("[QuicClient] MLS text group not ready yet (attempt \(attempt)/15), waiting...")
                    try await Task.sleep(nanoseconds: 200_000_000) // 200ms
                } else {
                    print("[QuicClient] MLS text group unavailable after \(attempt) attempts")
                    throw error
                }
            }
        }
        
        guard let myKey = myKey else {
            throw QuicClientError.protocolError("MLS text group not available")
        }
        
        // Create crypto wrapper with our key
        let crypto = try TextCryptoWrapper(key: myKey)
        
        // Create plaintext message record
        let textMsg = TextMessageRecord(
            senderUuid: "user-\(senderSessionId)",  // TODO: Use real UUID from identity
            timestamp: UInt64(Date().timeIntervalSince1970 * 1000),
            content: content,
            replyToId: replyToId ?? "",
            messageId: messageId,
            mediaType: 0, // TEXT
            fileSize: 0,
            sha256Hash: ""
        )
        
        // Encrypt using DAVE with MLS-derived key
        let encryptedPacket = try crypto.encrypt(
            epoch: epoch,
            channelId: channelId,
            senderSessionId: senderSessionId,
            message: textMsg
        )
        
        let packetRecord = EncryptedTextPacketRecord(
            senderSessionId: encryptedPacket.senderSessionId,
            channelId: String(encryptedPacket.channelId),
            epoch: encryptedPacket.epoch,
            messageId: messageId,
            ciphertext: encryptedPacket.ciphertext,
            nonce: encryptedPacket.nonce,
            tag: encryptedPacket.tag,
            replyToId: replyToId ?? ""
        )
        
        let payload = encodeEncryptedTextPacket(packet: packetRecord)
        var msg = Data([Self.MSG_TEXT_PACKET])
        let len = UInt32(payload.count).littleEndian
        msg.append(withUnsafeBytes(of: len) { Data($0) })
        msg.append(payload)
        
        try await send(data: msg, on: stream)
        print("[QuicClient] Sent encrypted text message (\(msg.count) bytes)")
    }
    
    // MARK: - Disconnect
    
    public func disconnect() {
        // Set intentional flag BEFORE cancelling anything to prevent
        // stale state handler callbacks from triggering reconnection
        isIntentionalDisconnect = true
        
        stopKeepalive()
        stopListening()
        
        // Clear state handlers before cancelling to avoid race conditions
        quicGroup?.stateUpdateHandler = nil
        controlStream?.stateUpdateHandler = nil
        
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
        // Never reconnect if the user intentionally disconnected
        guard !isIntentionalDisconnect else {
            print("[QuicClient] Ignoring connection loss — intentional disconnect")
            return
        }
        
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
    
    /// Receive a length-prefixed payload with strict size limits to prevent OOM
    private func receiveHardenedPayload(maxLen: Int, on stream: NWConnection) async throws -> Data {
        let lenData = try await receive(on: stream, minimumLength: 4, maximumLength: 4)
        let length = lenData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
        
        guard length <= maxLen else {
            print("[QuicClient] Incoming frame too large: \(length) bytes (max \(maxLen))")
            throw QuicClientError.protocolError("Incoming frame exceeds size limit")
        }
        
        return try await receive(on: stream, minimumLength: Int(length), maximumLength: Int(length))
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
    case untrustedCertificate(host: String, fingerprint: String)
    
    public var errorDescription: String? {
        switch self {
        case .notConnected: return "Not connected to server"
        case .noIdentity: return "No identity available"
        case .signingFailed: return "Failed to sign challenge"
        case .protocolError(let msg): return "Protocol error: \(msg)"
        case .authenticationFailed(let msg): return "Authentication failed: \(msg)"
        case .connectionClosed: return "Connection closed"
        case .untrustedCertificate(let host, let fingerprint):
            return "Untrusted certificate from \(host)\n\nSHA256: \(fingerprint)"
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
    public var isDisconnected: Bool = false
    
    public init(sessionId: UInt32, displayName: String, bio: String = "", avatarData: Data? = nil, isMuted: Bool = false, isDeafened: Bool = false, isDisconnected: Bool = false) {
        self.id = sessionId
        self.displayName = displayName
        self.bio = bio
        self.avatarData = avatarData
        self.isMuted = isMuted
        self.isDeafened = isDeafened
        self.isDisconnected = isDisconnected
    }
}

// MARK: - Channel Model

public struct ChannelModel: Identifiable, Hashable {
    public let id: String
    public let name: String
    public let comment: String
    public let iconEmoji: String?
    public let iconPresetId: String?
    public let iconCustomData: Data?
    public let position: Int32
    public let isLobby: Bool
    
    public init(id: String, name: String, comment: String = "", iconEmoji: String? = nil, iconPresetId: String? = nil, iconCustomData: Data? = nil, position: Int32 = 0, isLobby: Bool = false) {
        self.id = id
        self.name = name
        self.comment = comment
        self.iconEmoji = iconEmoji
        self.iconPresetId = iconPresetId
        self.iconCustomData = iconCustomData
        self.position = position
        self.isLobby = isLobby
    }
    
    public init(record: ChannelInfoRecord) {
        self.id = record.channelId
        self.name = record.name
        self.comment = record.comment
        self.iconEmoji = record.icon?.emoji
        self.iconPresetId = record.icon?.presetId
        self.iconCustomData = record.icon?.customData
        self.position = record.position
        self.isLobby = record.channelType == .lobby
    }
}

// MARK: - Received Text Message Model

/// Represents a received text message from the server
public struct ReceivedTextMessage: Identifiable, Equatable {
    public let id: String
    public let senderSessionId: UInt32
    public let senderName: String
    public let channelId: String
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
    public let channelId: String // "0" for global
    
    public init(content: String, channelId: String = "0") {
        self.content = content
        self.channelId = channelId
    }
}
