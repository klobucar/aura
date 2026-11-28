import AuraCore
import Foundation
import Combine
import Network
import Observation
import UserNotifications
import SwiftUI
import CryptoKit
import UniformTypeIdentifiers
import os

/// Native QUIC client for Aura server using Apple's Network framework.
/// Uses NWConnectionGroup to handle server-initiated streams for Apple/Quinn interop.
@Observable
@MainActor
public class QuicNetworkClient {
    
    // MARK: - Published State
    
    public var isConnected = false
    public var isAuthenticated = false {
        // The audio domain sends nothing while we aren't logged in.
        didSet { voice.post(.setAuthenticated(isAuthenticated)) }
    }
    public var isAdmin = false
    /// True once the server has told us we were banned (a close with
    /// application error 0x01, or a refused login). Nothing reconnects until
    /// the next explicit `connect`.
    public var wasBanned = false

    // Moderation console state (admins only). See `MARK: - Moderation`.
    var moderationMembers: [ModerationMember] = []
    var moderationIsLoading = false
    /// The server's `error_message` for the last refused request.
    var moderationError: String?
    /// What the last request that succeeded did, for the panel to show.
    var moderationNotice: String?
    /// Requests sent and not yet answered, oldest first.
    private(set) var moderationQueue: [ModerationAction] = []
    var moderationBusy: Bool { !moderationQueue.isEmpty }
    private var moderationListBuffer: [ModerationMember] = []
    private var moderationRelistQueued = false
    public var connectionStatus = "Disconnected"
    public var userId: UInt32 = 0
    public var sessionToken: String?
    /// Our account's server-assigned id (`U_…`), from the login response.
    /// It is also our identity in MLS group credentials.
    public var userUuid: String?
    public var currentChannelId: String?
    public var sessionId: UInt32?  // Our own session ID
    
    public var isMuted = false
    public var isDeafened = false
    public var isServerMuted = false

    private var userPresenceByUuid: [String: AuraPresenceStatus] = [:]
    private var userCustomStatusByUuid: [String: String] = [:]
    private var userServerMutedByUuid: [String: Bool] = [:]
    
    /// Users by channel ID (tracks all channels, not just current)
    public var usersByChannel: [String: [ChannelUser]] = [:]
    
    /// All channels on the server
    public var channels: [ChannelModel] = []
    
    /// User profiles by account id (`U_…`) — the key the server uses. Session
    /// ids are ephemeral; go through `profile(forSession:)` to look one up.
    public var profiles: [String: UserProfileRecord] = [:]

    /// The profile of whoever holds `sessionId` right now, if known.
    public func profile(forSession sessionId: UInt32?) -> UserProfileRecord? {
        guard let sessionId else { return nil }
        let uuid = sessionId == self.sessionId ? userUuid : uuidForSession(sessionId)
        return uuid.flatMap { profiles[$0] }
    }

    /// Received chat messages (populated by incoming text packets)
    public var receivedMessages: [ReceivedTextMessage] = []

    /// E2EE file transfer states keyed by lowercase hex `fileId`.
    public var fileTransferStates: [String: FileTransferState] = [:]

    private struct PendingManifest {
        let manifest: FileManifestRecord
        let senderSessionId: UInt32
    }

    private struct PendingDelivery {
        let header: FileStreamHeaderRecord
        let chunks: [Data]
    }

    private var pendingManifests: [String: PendingManifest] = [:]
    private var pendingDeliveries: [String: PendingDelivery] = [:]
    private var transferTimeoutTasks: [String: Task<Void, Never>] = [:]
    
    /// System events (user leave/join) for UI
    public var systemEvents: [SystemEvent] = []
    
    // MARK: - Private Properties
    
    /// The QUIC connection group (manages the tunnel and accepts streams)
    private var quicGroup: NWConnectionGroup? {
        // Audio datagrams go out from the audio domain, on this group.
        didSet { voice.post(.setLink(quicGroup.map { ConnectionGroupLink(group: $0) })) }
    }
    
    /// Control stream from server (for auth and signaling)
    private var controlStream: NWConnection?
    
    /// Continuation for waiting on server stream
    private var streamContinuation: CheckedContinuation<NWConnection, Error>?
    
    /// The audio domain. It owns the Rust audio sender and receiver,
    /// playback, and the audio datagram path; frames never pass through the
    /// main actor. This class derives keys (MLS lives here) and settings, and
    /// posts them to it in order. See `VoiceEngine`.
    nonisolated let voice = VoiceEngine(output: AudioPlayback())

    /// True once a sender and receiver have been handed to `voice`.
    private var isVoiceAttached = false

    /// Bumped by `clearLocalSpeaking()`; see `VoiceMeters.localSpeechGeneration`.
    private var localSpeechGeneration: UInt64 = 0
    
    // Text crypto now uses MLS-derived per-sender keys (no shared instance)
    
    /// MLS wrapper (Rust UniFFI wrapper for MLS group management and key derivation)
    private var mlsWrapper: MlsWrapper?
    
    /// Track the current voice channel for MLS group ID generation
    private var currentVoiceChannelId: String? {
        // A new channel needs its own key: the audio domain stops sending
        // until that channel's MLS key arrives.
        didSet {
            if currentVoiceChannelId != oldValue {
                voice.post(.setVoiceChannel(currentVoiceChannelId))
            }
        }
    }
    
    /// Track which users are currently speaking (for UI indicators).
    ///
    /// This is fed from `receiver.popMixed()`, so it only ever contains
    /// *remote* senders — our own audio never round-trips through the
    /// receiver. Use `isLocalSpeaking` for the local talking signal.
    ///
    /// This and the three properties below are published by `voice` at most
    /// ~30 times a second; a speaker drops out 500ms after their last frame.
    public var activeSpeakers: Set<UInt32> = []

    /// True while the local send path is actually emitting packets, i.e. the
    /// VAD gate is open and the frame is loud enough to be speech.
    ///
    /// The talk lanes need the local user's lane filled in too, and
    /// `activeSpeakers` structurally cannot provide it (see above), so the
    /// signal is lifted off the send path in `VoiceEngine` instead.
    public var isLocalSpeaking = false

    /// Level of the most recent captured frame, in dBFS, floored at -60.
    /// Drives the Command HUD input meter.
    public var inputLevelDb: Float = QuicNetworkClient.levelFloorDb

    /// Level of the most recent mixed *received* frame, in dBFS, floored at
    /// -60. With one remote speaker this is that speaker's level, which is
    /// what the current-speaker waveform wants.
    public var outputLevelDb: Float = QuicNetworkClient.levelFloorDb

    /// Quietest level the meters represent; everything below reads as silence.
    static let levelFloorDb: Float = VoiceMeters.floorDb

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

    /// Set before `connect` to accept the server only if its leaf
    /// certificate's SHA-256 is this (an invite link's `fp`), in every build
    /// configuration and whether or not the certificate is publicly valid.
    /// Hex, either case.
    public var expectedCertificateFingerprint: String?

    /// Outcome of the pin check for the latest `connect`.
    private var pinCheck: CertificatePinCheck?

    /// True when the latest `connect` failed because the server's certificate
    /// did not match `expectedCertificateFingerprint`.
    public var certificateDidNotMatchPin: Bool { pinCheck?.mismatched ?? false }

    /// Host and port of the server this client dialled, as dialled.
    public var connectedHost: String? { savedHost }
    public var connectedPort: UInt16? { savedPort }

    /// The server password this connection logged in with, if any.
    public var connectionPassword: String? {
        savedPassword.flatMap { $0.isEmpty ? nil : $0 }
    }

    /// The pending `deleteOwnAccount()` call, resumed by the server's 0x44
    /// reply, a timeout, or the connection dropping.
    private var accountDeletion: CheckedContinuation<Void, Error>?
    private var accountDeletionToken: UUID?

    /// The pending `updateProfile` call, resumed by the server's 0x42 reply
    /// or a timeout.
    private var profileUpdate: CheckedContinuation<Void, Error>?
    private var profileUpdateToken: UUID?

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
    private static let MSG_DELETE_CHANNEL: UInt8 = 0x43
    private static let MSG_DELETE_USER: UInt8 = 0x44
    private static let MSG_UPDATE_STATUS: UInt8 = 0x45
    private static let MSG_PROFILE_UPDATED: UInt8 = 0x46 // Server → clients broadcast
    private static let MSG_CHANNEL_DELETED: UInt8 = 0x47 // Server → occupants of a deleted channel
    private static let MSG_LIST_USERS: UInt8 = 0x48
    private static let MSG_VERIFY_USER: UInt8 = 0x49
    private static let MSG_BAN_USER: UInt8 = 0x4A
    private static let MSG_GRANT_ADMIN: UInt8 = 0x4B
    private static let MSG_KICK_USER: UInt8 = 0x4C
    private static let MSG_SERVER_MUTE: UInt8 = 0x4D
    
    // MLS Protocol message types
    private static let MSG_MLS_JOIN: UInt8 = 0x50           // Client sends key package
    private static let MSG_MLS_COMMIT_WELCOME: UInt8 = 0x51 // Client sends commit + welcome
    private static let MSG_MLS_CREATE_GROUP: UInt8 = 0x52   // Server tells client to create group
    private static let MSG_MLS_ADD_MEMBER_REQ: UInt8 = 0x53 // Server forwards key package
    private static let MSG_MLS_COMMIT: UInt8 = 0x54         // Server broadcasts commit
    private static let MSG_MLS_WELCOME: UInt8 = 0x55        // Server broadcasts welcome
    private static let MSG_MLS_RATCHET_REQUEST: UInt8 = 0x56 // Server asks us (founder) to ratchet
    private static let MSG_MLS_RATCHET_COMMIT: UInt8 = 0x57  // We return a commit with no Welcome (ratchet or removals)
    private static let MSG_MLS_SYNC_MEMBERS: UInt8 = 0x58    // Server tells us (founder) who should remain in a group
    
    // Security limits
    private static let MAX_AUDIO_PACKET_SIZE = 65536
    private static let MAX_CONTROL_PACKET_SIZE = 2 * 1024 * 1024 // 2MB
    private static let MAX_AUTH_RESPONSE_SIZE = 64 * 1024
    
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
    /// The notification listeners started in `init`, cancelled in `deinit`.
    /// Tasks are Sendable, so nothing here needs an unchecked wrapper.
    private nonisolated let observerTasks = OSAllocatedUnfairLock(initialState: [Task<Void, Never>]())

    private var savedHost: String?
    private var savedPort: UInt16?
    private var savedIdentity: UserIdentity?
    private var savedPassword: String?
    
    public init() {
        loadLocalMixerPrefs()

        if let initialOutputID = AudioDeviceManager.shared.selectedOutputDeviceID {
            voice.post(.setOutputDevice(initialOutputID))
        }

        // Each listener maps its notification to a Sendable value before it
        // leaves the sequence; `Notification` itself is not Sendable.
        let center = NotificationCenter.default
        let tasks: [Task<Void, Never>] = [
            // Audio settings changes. Everything they touch belongs to the
            // audio domain, so they go straight there.
            Task { [voice] in
                let changes = center.notifications(named: .audioSettingsChanged)
                    .compactMap { AudioSettingsChange($0.object) }
                for await change in changes {
                    voice.post(.applySettings(change))
                }
            },
            Task { [voice] in
                let devices = center.notifications(named: .audioOutputDeviceChanged)
                    .map { $0.object as? UInt32 }
                for await deviceID in devices {
                    voice.post(.setOutputDevice(deviceID))
                }
            },
            Task { [voice] in
                let restarts = center.notifications(named: .audioOutputNeedsRestart).map { _ in () }
                for await _ in restarts {
                    voice.post(.restartOutput)
                }
            },
            Task { [weak self] in
                let changes = center.notifications(named: .presenceStatusChanged).map { _ in () }
                for await _ in changes {
                    guard let self else { return }
                    await self.updateStatus(isMuted: self.isMuted, isDeafened: self.isDeafened)
                }
            },
        ]
        observerTasks.withLock { $0 = tasks }

        // Meter snapshots from the audio domain: at most ~30 a second, and
        // only the newest if the main actor falls behind.
        Task { [weak self, meters = voice.meters] in
            for await snapshot in meters {
                self?.applyVoiceMeters(snapshot)
            }
        }
    }
    
    deinit {
        for task in observerTasks.withLock({ $0 }) {
            task.cancel()
        }
        voice.shutdown()
    }
    
    // MARK: - Telemetry

    /// Run an app-level operation inside an opt-in telemetry span (aura-core
    /// `TelemetrySpan`). The span carries only the operation and its outcome;
    /// it does nothing unless telemetry was started.
    private func traced(_ operation: ClientOperation, _ body: () async throws -> Void) async throws {
        let span = TelemetrySpan(operation: operation)
        do {
            try await body()
            span.finish(outcome: .ok)
        } catch is CancellationError {
            span.finish(outcome: .cancelled)
            throw CancellationError()
        } catch {
            span.finish(outcome: .failed)
            throw error
        }
    }

    // MARK: - Connection
    
    /// Connect to the Aura server via QUIC using NWConnectionGroup.
    /// This allows accepting server-initiated streams.
    public func connect(host: String, port: UInt16 = 8443) async throws {
        try await traced(.connect) { try await self.performConnect(host: host, port: port) }
    }

    private func performConnect(host: String, port: UInt16) async throws {
        // Clear intentional disconnect flag and any stale control stream before connecting.
        isIntentionalDisconnect = false
        wasBanned = false
        controlStream?.stateUpdateHandler = nil
        controlStream?.cancel()
        controlStream = nil
        
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
        
        // An invite's fingerprint, when there is one, decides alone.
        let expectedFingerprint = expectedCertificateFingerprint?.lowercased()
        let pinCheck = CertificatePinCheck()
        self.pinCheck = pinCheck
        // The verify block runs on a background queue, so it gets the
        // fingerprint trusted for this host now rather than reading settings.
        let trustedFingerprint = AppSettings.shared.pinnedFingerprint(forHost: host)

        // Strict certificate verification / TOFU in all builds (including DEBUG).
        // The block runs on the queue passed below, never on the main actor.
        sec_protocol_options_set_verify_block(quicOptions.securityProtocolOptions, { @Sendable [weak self] _, trust, completeHandler in
            guard let self = self else {
                completeHandler(false)
                return
            }

            if let expectedFingerprint {
                let presented = Self.leafCertificateFingerprint(sec_trust_copy_ref(trust).takeRetainedValue())
                if presented == expectedFingerprint {
                    print("[QuicClient] TLS verify - certificate matches the invite's fingerprint")
                    completeHandler(true)
                } else {
                    print("[QuicClient] TLS verify - certificate does not match the invite's fingerprint")
                    pinCheck.recordMismatch()
                    completeHandler(false)
                }
                return
            }

            let trustRef = sec_trust_copy_ref(trust).takeRetainedValue()
            var error: CFError?
            let isValid = SecTrustEvaluateWithError(trustRef, &error)
            
            if isValid {
                print("[QuicClient] TLS verify - system trust valid")
                completeHandler(true)
                return
            }
            
            // Standard trust failed - perform TOFU check
            guard let fingerprint = Self.leafCertificateFingerprint(trustRef) else {
                print("[QuicClient] TLS verify - no certificate found")
                completeHandler(false)
                return
            }
            
            print("[QuicClient] TLS verify failure - fingerprint: \(fingerprint)")
            
            // Check if user has already trusted this fingerprint for this host
            if trustedFingerprint == fingerprint {
                print("[QuicClient] Fingerprint matches known trusted list. Proceeding.")
                completeHandler(true)
            } else {
                print("[QuicClient] Untrusted certificate. Blocking connection.")
                Task { @MainActor in
                    self.lastUntrustedFingerprint = fingerprint
                }
                completeHandler(false)
            }
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
        group.setReceiveHandler(maximumMessageSize: 1220, rejectOversizedMessages: true) { [weak self, voice] _, content, _ in
            guard let data = content, !data.isEmpty else { return }

            // Ping echo from the server: [0x00][8-byte nonce]. A bare
            // 1-byte 0x00 is a legacy server-initiated keepalive; ignore.
            if data[0] == 0x00 {
                if data.count >= 9 {
                    let nonce = data.subdata(in: 1..<9).withUnsafeBytes {
                        $0.load(as: UInt64.self)
                    }
                    Task { @MainActor in
                        self?.recordPingEcho(nonce: nonce)
                    }
                }
                return
            }

            if data[0] == 0x02 {  // Audio datagram: straight to the audio domain
                voice.submitReceived(data.subdata(in: 1..<data.count))
            } else if data[0] == 0x01 {  // Older audio format: reported, never played
                voice.submitLegacyReceived(data.subdata(in: 1..<data.count))
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
    
    /// Lowercase hex SHA-256 of the leaf certificate's DER: the value pinned
    /// for trust-on-first-use and carried by invite links.
    nonisolated static func leafCertificateFingerprint(_ trust: SecTrust) -> String? {
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
              let leaf = chain.first
        else { return nil }
        let der = SecCertificateCopyData(leaf) as Data
        return SHA256.hash(data: der).map { String(format: "%02x", $0) }.joined()
    }

    private func waitForGroupReady(_ group: NWConnectionGroup) async throws {
        return try await withCheckedThrowingContinuation { continuation in
            // Set on the main actor once the continuation has been resumed;
            // the handler checks it first on the group's queue.
            let resumed = OSAllocatedUnfairLock(initialState: false)
            
            group.stateUpdateHandler = { [weak self] state in
                guard !resumed.withLock({ $0 }) else { return }
                
                Task { @MainActor in
                    switch state {
                    case .setup:
                        print("[QuicClient] Group setup...")
                    case .waiting(let error):
                        print("[QuicClient] Group waiting: \(error)")
                    case .ready:
                        print("[QuicClient] Group ready!")
                        resumed.withLock { $0 = true }
                        continuation.resume()
                    case .failed(let error):
                        print("[QuicClient] Group failed: \(error)")
                        self?.isConnected = false
                        resumed.withLock { $0 = true }
                        
                        // Trigger reconnection only if this was an unexpected disconnect
                        if self?.isAuthenticated == true && self?.isIntentionalDisconnect != true {
                            self?.handleConnectionLoss()
                        }
                        
                        continuation.resume(throwing: error)
                    case .cancelled:
                        print("[QuicClient] Group cancelled")
                        self?.isConnected = false
                        let wasResumed = resumed.withLock { flag in
                            let was = flag
                            flag = true
                            return was
                        }
                        if !wasResumed {
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
        if controlStream != nil {
            print("[QuicClient] Incoming secondary stream — treating as file delivery stream")
            receiveFileDeliveryStream(connection)
            return
        }

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
    
    /// Log in with the hybrid identity (login v2). The server opens the
    /// control stream and speaks first:
    ///
    ///     server → [0x02][32-byte challenge]
    ///     client → [0x03][u32 LE length][AuthRequest]   (built and signed by aura-core)
    ///     server → [0x04][u32 LE length][AuthResponse]  (success and failure alike)
    public func authenticate(identity: UserIdentity, serverPassword: String? = nil) async throws {
        try await traced(.login) {
            try await self.performAuthenticate(identity: identity, serverPassword: serverPassword)
        }
    }

    private func performAuthenticate(identity: UserIdentity, serverPassword: String?) async throws {
        guard identity.hasIdentity else {
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
        
        // Step 2: Build the signed AuthRequest frame and send it
        let authRequest: Data
        do {
            authRequest = try identity.authRequestFrame(challenge: challenge, serverPassword: serverPassword)
        } catch {
            print("[QuicClient] Failed to build auth request: \(error)")
            throw QuicClientError.signingFailed
        }
        
        print("[QuicClient] Sending auth request (\(authRequest.count) bytes)...")
        try await send(data: authRequest, on: stream)
        
        // Step 3: Receive the framed AuthResponse
        print("[QuicClient] Waiting for auth response...")
        let responseType = try await receive(on: stream, minimumLength: 1, maximumLength: 1)
        
        guard let type = responseType.first, type == Self.MSG_AUTH_RESPONSE else {
            let got = responseType.first.map { String(format: "0x%02X", $0) } ?? "nothing"
            throw QuicClientError.protocolError("Invalid auth response: expected 0x04, got \(got)")
        }
        
        let payload = try await receiveHardenedPayload(maxLen: Self.MAX_AUTH_RESPONSE_SIZE, on: stream)
        let response: AuthResponseRecord
        do {
            response = try decodeAuthResponse(payload: payload)
        } catch {
            throw QuicClientError.protocolError("Malformed auth response: \(error)")
        }
        
        let responseUserId = response.sessionId
        let finalToken = response.sessionToken
        
        print("[QuicClient] Auth response: success=\(response.success), sessionId=\(responseUserId), user=\(response.userUuid), token=\(finalToken.prefix(8))..., verified=\(response.verified), isAdmin=\(response.isAdmin)")
        
        guard response.success else {
            throw QuicClientError.authenticationFailed(response.errorMessage.isEmpty ? "Unknown error" : response.errorMessage)
        }
        
        self.userId = responseUserId
        self.sessionToken = finalToken
        self.userUuid = response.userUuid
        self.isAuthenticated = true
        self.isAdmin = response.isAdmin
        
        // Use userId as sessionId immediately (server sends the real session ID now)
        self.sessionId = responseUserId
        print("[QuicClient] Session ID set to: \(responseUserId) from auth response")
        self.connectionStatus = "Authenticated as user \(userId)" + (response.verified ? " (verified)" : "")
        
        print("[QuicClient] ✓ Authentication SUCCESS!")
        
        // MLS client for this account: its credential names the server's user
        // id, and its signing key is derived from the identity secret, so it is
        // the same on every connection.
        do {
            mlsWrapper = try identity.makeMlsWrapper(userId: response.userUuid)
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
                // Initialize audio sender/receiver with a placeholder DAVE
                // key. It never encrypts audio: `voice` sends nothing until an
                // MLS-derived key for the current channel replaces it.
                //
                // Both are built and configured here, then handed to `voice`,
                // which is the only thing that uses them from then on.
                let tempKeyData = Data(repeating: 0x42, count: 32)
                let sender = try AudioSenderWrapper(sessionId: responseUserId, key: tempKeyData)
                sender.setEpoch(epoch: 0)
                
                // Enable DRED (10 frames = 100ms of redundancy)
                sender.setDredDuration(duration: 10)
                print("[QuicClient] DRED enabled (100ms redundancy)")
                
                // Initialize audio receiver (for receiving others' voice)
                let receiver = AudioReceiverWrapper()
                
                // Apply saved audio settings
                let noiseSuppressionEnabled = UserDefaults.standard.object(forKey: "noiseSuppressionEnabled") as? Bool ?? true
                let aecEnabled = UserDefaults.standard.bool(forKey: "aecEnabled")
                let webrtcNsEnabled = UserDefaults.standard.bool(forKey: "webrtcNsEnabled")
                let webrtcAgcEnabled = UserDefaults.standard.object(forKey: "webrtcAgcEnabled") as? Bool ?? true
                let jitterBufferMs = UserDefaults.standard.object(forKey: "jitterBufferMs") as? Int ?? 20
                
                sender.setNoiseSuppressionEnabled(enabled: noiseSuppressionEnabled)
                sender.setWebrtcAecEnabled(enabled: aecEnabled)
                sender.setWebrtcNsEnabled(enabled: webrtcNsEnabled)
                sender.setWebrtcAgcEnabled(enabled: webrtcAgcEnabled)

                let savedBitrateKbps = UserDefaults.standard.integer(forKey: "AuraOpusBitrateKbps")
                let opusBitrateKbps = AppSettings.supportedBitratesKbps.contains(savedBitrateKbps) ? savedBitrateKbps : 64
                try? sender.setBitrate(bps: UInt32(opusBitrateKbps * 1000))

                // VAD: enabled when transmission mode is voice activation; threshold derived
                // from the saved sensitivity slider (0.0–1.0 maps to -50..-20 dB).
                let savedMode = UserDefaults.standard.string(forKey: "AuraTransmissionMode")
                let vadEnabled = (savedMode == "vad")
                let savedSensitivity = UserDefaults.standard.float(forKey: "AuraVADSensitivity")
                let sensitivity = savedSensitivity == 0 ? Float(0.5) : savedSensitivity
                let thresholdDb = -50.0 + (sensitivity * 30.0) // 0.0 → -50 dB, 1.0 → -20 dB
                sender.setVadThresholdDb(thresholdDb: thresholdDb)
                sender.setVadEnabled(enabled: vadEnabled)

                receiver.setJitterBufferMs(latencyMs: UInt32(jitterBufferMs))

                let masterVolume = UserDefaults.standard.object(forKey: "masterVolume") as? Double ?? 1.0

                print("[QuicClient] Applied settings: RNNoise=\(noiseSuppressionEnabled), AEC=\(aecEnabled), WebRTC-NS=\(webrtcNsEnabled), AGC=\(webrtcAgcEnabled), Jitter=\(jitterBufferMs)ms, VAD=\(vadEnabled)@\(thresholdDb)dB, Volume=\(Int((masterVolume * 100).rounded()))%")
                
                // Text crypto will use MLS-derived keys per-sender (no initialization needed)
                
                // Hand over; the audio domain sets the volume and starts playback.
                voice.post(.attach(sender: sender, receiver: receiver, masterVolume: Float(masterVolume)))
                isVoiceAttached = true
                
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

            // Publish local presence and custom status on connect
            Task {
                await self.updateStatus(isMuted: self.isMuted, isDeafened: self.isDeafened)
            }
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
        voice.post(.setSenderGain(sessionId: sessionId, gain: clamped))

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
            voice.post(.setSenderMuted(sessionId: sessionId, muted: nowMuted))
            return
        }

        if locallyMutedUsers.contains(uuid) {
            locallyMutedUsers.remove(uuid)
            voice.post(.setSenderMuted(sessionId: sessionId, muted: false))
        } else {
            locallyMutedUsers.insert(uuid)
            voice.post(.setSenderMuted(sessionId: sessionId, muted: true))
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
        guard isVoiceAttached, let prefs = mixerPrefs(forSession: sessionId),
              prefs != VoiceMixerPrefs() else { return }
        voice.post(.applyMixerPrefs(sessionId: sessionId, prefs: prefs))
    }

    /// The persisted local volume / mute for whoever holds `sessionId`, or
    /// nil while we don't know who that is.
    private func mixerPrefs(forSession sessionId: UInt32) -> VoiceMixerPrefs? {
        guard let uuid = sessionToUuid[sessionId], !uuid.isEmpty else { return nil }
        return VoiceMixerPrefs(gain: userVolumes[uuid], muted: locallyMutedUsers.contains(uuid))
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

    /// The stable user id behind an ephemeral session, if we've been told it.
    func uuidForSession(_ sessionId: UInt32) -> String? {
        sessionToUuid[sessionId]
    }

    /// Resolve an MLS credential identity (a user UUID) to the display name the
    /// roster knows, if any member currently in view maps to it.
    ///
    /// Only ever a label: identity is the key, and a name that cannot be
    /// resolved is not a reason to trust or distrust anything.
    func displayName(forUuid uuid: String) -> String? {
        guard !uuid.isEmpty else { return nil }

        for users in usersByChannel.values {
            for user in users where sessionToUuid[user.id] == uuid {
                return user.displayName
            }
        }
        return nil
    }

    /// The MLS client, exposed for reading group membership during identity
    /// verification. Member keys must come from here — the authenticated
    /// ratchet tree — and never from the server's user list, which carries no
    /// key material precisely so it cannot be used to assert identity.
    var mls: MlsWrapper? { mlsWrapper }

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

    /// Update user profile. Returns once the server has accepted it, and
    /// throws the server's reason (or a timeout) otherwise.
    public func updateProfile(bio: String, avatarData: Data) async throws {
        // The server only accepts a profile whose id is our account id.
        guard self.sessionId != nil, let userUuid else { throw QuicClientError.notConnected }
        guard profileUpdate == nil else {
            throw QuicClientError.profileUpdateFailed("A profile update is already in progress")
        }

        let record = UserProfileRecord(
            userId: userUuid,
            displayName: profiles[userUuid]?.displayName
                ?? savedIdentity?.displayName
                ?? displayName(forUuid: userUuid)
                ?? "Unknown",
            bio: bio,
            avatarData: avatarData,
            signature: Data(), // Server handles signature for now or we do it in Rust
            signingKey: Data()
        )
        
        let payload = encodeUpdateProfile(profile: record)
        
        // Send MSG_UPDATE_PROFILE (0x42)
        let mutStream: NWConnection? = await MainActor.run { self.controlStream }
        guard let stream = mutStream else { throw QuicClientError.notConnected }

        var msg = Data([Self.MSG_UPDATE_PROFILE])
        let len = UInt32(payload.count).littleEndian
        msg.append(Data(withUnsafeBytes(of: len) { Array($0) }))
        msg.append(Data(payload))

        let token = UUID()
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            profileUpdate = continuation
            profileUpdateToken = token
            Task {
                do {
                    try await self.send(data: msg, on: stream)
                    print("[QuicClient] Profile update sent")
                } catch {
                    print("[QuicClient] Failed to send profile update: \(error)")
                    self.finishProfileUpdate(.failure(error))
                }
            }
            Task {
                try? await Task.sleep(for: .seconds(15))
                guard self.profileUpdateToken == token else { return }
                self.finishProfileUpdate(.failure(QuicClientError.profileUpdateFailed("The server didn't respond")))
            }
        }
    }

    private func finishProfileUpdate(_ result: Result<Void, Error>) {
        guard let continuation = profileUpdate else { return }
        profileUpdate = nil
        profileUpdateToken = nil
        continuation.resume(with: result)
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

    /// Delete a channel (Admin only). The server replies with an `AdminResponse`
    /// and broadcasts a fresh server snapshot (`MSG_CHANNEL_STATE`), so the
    /// channel list refreshes for everyone without a response handler here.
    public func deleteChannel(id: String) async {
        guard isAdmin else { return }

        // DeleteChannelRequest is a single proto field: `string channel_id = 1`.
        // No generated encoder exists for it, so encode the one field inline:
        // tag 0x0A (field 1, length-delimited) + varint length + UTF-8 bytes.
        let idBytes = Array(id.utf8)
        var payload = Data([0x0A])
        var len = idBytes.count
        repeat {
            var byte = UInt8(len & 0x7F)
            len >>= 7
            if len > 0 { byte |= 0x80 }
            payload.append(byte)
        } while len > 0
        payload.append(contentsOf: idBytes)

        let mutStream: NWConnection? = await MainActor.run { self.controlStream }
        guard let stream = mutStream else { return }

        var msg = Data([Self.MSG_DELETE_CHANNEL]) // MSG_DELETE_CHANNEL
        let frameLen = UInt32(payload.count).littleEndian
        msg.append(Data(withUnsafeBytes(of: frameLen) { Array($0) }))
        msg.append(payload)

        do {
            try await send(data: msg, on: stream)
            print("[QuicClient] Delete channel request sent for \(id)")
        } catch {
            print("[QuicClient] Failed to send delete channel request: \(error)")
        }
    }

    // MARK: - Moderation

    /// Send one `[type u8][u32 LE length][payload]` control frame.
    /// Returns false when there is no stream or the send failed.
    private func sendControlFrame(type: UInt8, payload: Data) async -> Bool {
        guard let stream = controlStream else { return false }
        var msg = Data([type])
        let frameLen = UInt32(payload.count).littleEndian
        msg.append(Data(withUnsafeBytes(of: frameLen) { Array($0) }))
        msg.append(payload)
        do {
            try await send(data: msg, on: stream)
            return true
        } catch {
            print("[QuicClient] Failed to send control frame 0x\(String(type, radix: 16)): \(error)")
            return false
        }
    }

    /// Send a moderation request and remember it, so the reply (which carries
    /// no id of its own) can be matched. Replies come back in request order.
    private func sendModeration(_ action: ModerationAction, payload: Data) async {
        guard isAdmin else { return }
        moderationError = nil
        moderationNotice = nil
        moderationQueue.append(action)
        if await !sendControlFrame(type: action.messageType, payload: payload) {
            if let i = moderationQueue.lastIndex(of: action) { moderationQueue.remove(at: i) }
            if action == .list { moderationIsLoading = false }
            moderationError = "Couldn't reach the server."
        }
    }

    /// Fetch every account, a page at a time (the server caps a page at 500).
    func refreshModerationMembers() async {
        guard isAdmin else { return }
        if moderationQueue.contains(.list) {
            // A page is in flight; fetch again once it lands.
            moderationRelistQueued = true
            return
        }
        moderationListBuffer = []
        moderationIsLoading = true
        await requestModerationPage(offset: 0)
    }

    private func requestModerationPage(offset: Int) async {
        await sendModeration(.list, payload: encodeListUsersRequest(offset: UInt32(offset), limit: 500))
    }

    func verifyUser(_ uuid: String) async {
        await sendModeration(.verify, payload: encodeVerifyUserRequest(targetUserUuid: uuid))
    }

    /// Ban or unban. `reason` goes to the server log only.
    func banUser(_ uuid: String, banned: Bool, reason: String) async {
        await sendModeration(
            .ban(banned: banned),
            payload: encodeBanUserRequest(targetUserUuid: uuid, banned: banned, reason: reason))
    }

    /// Replace the target's admin permissions. All off revokes admin.
    func grantAdmin(_ uuid: String, permissions: AdminPermissionsRecord) async {
        await sendModeration(
            .grant,
            payload: encodeGrantAdminRequest(targetUserUuid: uuid, permissions: permissions))
    }

    /// Take the target out of its channel and into the lobby.
    func kickUser(_ uuid: String, reason: String) async {
        await sendModeration(.kick, payload: encodeKickUserRequest(targetUserUuid: uuid, reason: reason))
    }

    /// Server-mute or unmute a live session (requires `ban_users` permission).
    func serverMuteUser(sessionId: UInt32, muted: Bool) async {
        await sendModeration(
            .serverMute(muted: muted),
            payload: encodeServerMuteRequest(targetSessionId: sessionId, muted: muted)
        )
    }

    /// Where an account is right now, from the roster we already hold.
    func moderationPresence(of uuid: String) -> ModerationPresence {
        var occupants: [String: [String]] = [:]
        for (channelId, users) in usersByChannel {
            occupants[channelId] = users.compactMap { uuidForSession($0.id) }
        }
        var summaries: [String: ChannelSummary] = [:]
        for channel in channels {
            summaries[channel.id] = ChannelSummary(name: channel.name, isLobby: channel.isLobby)
        }
        return ModerationLogic.presence(
            of: uuid, occupants: occupants, channels: summaries,
            selfUuid: userUuid, selfChannelId: currentChannelId)
    }

    private func popModerationAction(type: UInt8) -> ModerationAction? {
        guard let i = moderationQueue.firstIndex(where: { $0.messageType == type }) else { return nil }
        return moderationQueue.remove(at: i)
    }

    private func handleListUsersResponse(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let response = try decodeListUsersResponse(data: payload)
            _ = popModerationAction(type: Self.MSG_LIST_USERS)

            guard response.success else {
                moderationIsLoading = false
                moderationError = response.errorMessage.isEmpty ? "The server refused." : response.errorMessage
                demoteIfAdminRequired(response.errorMessage)
                return
            }

            moderationListBuffer.append(contentsOf: response.users.map(ModerationMember.init))
            for user in response.users {
                let mappedPresence = user.presence.map(Self.mapPresence)
                if let mappedPresence {
                    userPresenceByUuid[user.userUuid] = mappedPresence
                }
                if let cs = user.customStatus {
                    if cs.isEmpty {
                        userCustomStatusByUuid.removeValue(forKey: user.userUuid)
                    } else {
                        userCustomStatusByUuid[user.userUuid] = cs
                    }
                }
                userServerMutedByUuid[user.userUuid] = user.isServerMuted
                if user.userUuid == self.userUuid {
                    self.isServerMuted = user.isServerMuted
                }
                for channelId in usersByChannel.keys {
                    if let idx = usersByChannel[channelId]?.firstIndex(where: { self.uuidForSession($0.id) == user.userUuid }) {
                        if let mappedPresence {
                            usersByChannel[channelId]?[idx].presence = mappedPresence
                        }
                        if let cs = user.customStatus {
                            usersByChannel[channelId]?[idx].customStatus = cs.isEmpty ? nil : cs
                        }
                        usersByChannel[channelId]?[idx].isServerMuted = user.isServerMuted
                    }
                }
            }
            // An empty page ends the walk even if the count disagrees, so a
            // shrinking list can't loop forever.
            if moderationListBuffer.count < Int(response.totalCount), !response.users.isEmpty {
                await requestModerationPage(offset: moderationListBuffer.count)
                return
            }

            moderationMembers = moderationListBuffer
            moderationListBuffer = []
            moderationIsLoading = false
            if moderationRelistQueued {
                moderationRelistQueued = false
                await refreshModerationMembers()
            }
        } catch {
            print("[QuicClient] Failed to parse ListUsersResponse: \(error)")
            _ = popModerationAction(type: Self.MSG_LIST_USERS)
            moderationIsLoading = false
        }
    }

    private func handleModerationActionResponse(type: UInt8, stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let response = try decodeAdminResponse(data: payload)
            let action = popModerationAction(type: type)

            if response.success {
                moderationError = nil
                moderationNotice = action?.successNotice
                // Kicking changes the roster, which arrives on its own;
                // everything else changes an account's row.
                if action != .kick { await refreshModerationMembers() }
            } else {
                print("[QuicClient] Moderation request 0x\(String(type, radix: 16)) refused: \(response.errorMessage)")
                moderationNotice = nil
                moderationError = response.errorMessage.isEmpty ? "The server refused." : response.errorMessage
                demoteIfAdminRequired(response.errorMessage)
            }
        } catch {
            print("[QuicClient] Failed to parse moderation AdminResponse: \(error)")
            _ = popModerationAction(type: type)
        }
    }

    /// Forget requests that will never be answered (the connection is gone).
    private func resetModeration() {
        moderationQueue = []
        moderationListBuffer = []
        moderationRelistQueued = false
        moderationIsLoading = false
    }

    /// The server closed on us because the account was banned. Reconnecting
    /// would only be refused, so stop and say why.
    private func handleBanned() {
        print("[QuicClient] Banned by the server; not reconnecting")
        isIntentionalDisconnect = true
        wasBanned = true
        reconnectTask?.cancel()
        reconnectTask = nil
        isRetrying = false
        retryCount = 0
        stopKeepalive()
        stopListening()
        controlStream?.cancel()
        quicGroup?.cancel()
        controlStream = nil
        quicGroup = nil
        isConnected = false
        isAuthenticated = false
        resetModeration()
        connectionStatus = ModerationLogic.bannedMessage
    }

    /// Best-effort read of the QUIC application error the peer closed with.
    /// Network.framework only fills this in on some paths, so a nil here
    /// doesn't mean "not banned": the refused login on the next attempt is the
    /// backstop.
    private func closedWithBanned() -> Bool {
        let metadata = (quicGroup?.metadata(definition: NWProtocolQUIC.definition)
            ?? controlStream?.metadata(definition: NWProtocolQUIC.definition)) as? NWProtocolQUIC.Metadata
        guard let error = metadata?.applicationError else { return false }
        return ModerationLogic.isBannedClose(code: error.code, reason: error.reason)
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

        // Runs on the group's queue, not the main actor.
        group.send(content: datagram) { @Sendable error in
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
            
        case 0x21: // MSG_AUDIO - audio packet from server (v2, header authenticated)
            await handleAudioPacket(stream: stream)

        case 0x20: // MSG_AUDIO_V1 - older audio format: reported, never played
            await handleAudioPacket(stream: stream, legacy: true)
            
            
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

        case Self.MSG_MLS_SYNC_MEMBERS: // 0x58 - Remove members who left the channel
            await handleMlsSyncMembers(stream: stream)

        case Self.MSG_MLS_RATCHET_REQUEST: // 0x56 - Server asks us to advance the epoch
            await handleMlsRatchetRequest(stream: stream)
            
        case Self.MSG_UPDATE_STATUS: // 0x45
            await handleUserStatusUpdate(stream: stream)

        case Self.MSG_PROFILE_UPDATED: // 0x46 - Server broadcasts a user's profile change
            await handleProfileUpdated(stream: stream)

        case Self.MSG_CHANNEL_DELETED: // 0x47 - The channel we were in was deleted
            await handleChannelDeleted(stream: stream)

        case Self.MSG_CREATE_CHANNEL: // 0x40 - Reply to our CreateChannelRequest
            await handleCreateChannelResponse(stream: stream)

        case Self.MSG_UPDATE_CHANNEL: // 0x41 - Reply to our UpdateChannelRequest
            await handleUpdateChannelResponse(stream: stream)

        case Self.MSG_UPDATE_PROFILE: // 0x42 - Reply to our UpdateProfile request
            await handleUpdateProfileResponse(stream: stream)

        case Self.MSG_DELETE_CHANNEL: // 0x43 - Reply to our DeleteChannelRequest
            await handleDeleteChannelResponse(stream: stream)

        case Self.MSG_DELETE_USER: // 0x44 - Reply to our DeleteUserRequest
            await handleDeleteUserResponse(stream: stream)

        case Self.MSG_LIST_USERS: // 0x48 - Reply to our ListUsersRequest
            await handleListUsersResponse(stream: stream)

        case Self.MSG_VERIFY_USER, Self.MSG_BAN_USER, Self.MSG_GRANT_ADMIN, Self.MSG_KICK_USER, Self.MSG_SERVER_MUTE:
            // 0x49-0x4D - AdminResponse, framed with the request's type
            await handleModerationActionResponse(type: type, stream: stream)

        default:
            // Every control frame is [type][u32 LE length][payload]. Consume
            // the payload of a type we don't know, or its bytes would be read
            // as the next frames and the stream would never recover.
            print(String(format: "[QuicClient] Skipping unknown message type: 0x%02X", type))
            _ = try? await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
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
                    MessageNotifier.shared.presenceChanged(
                        "\(displayName) joined", identifier: "aura_user_joined_\(sessionId)")
                    
                    print("[QuicClient] User joined channel \(channelId): \(displayName) (session \(sessionId))")
                    print("[QuicClient] Channel \(channelId) now has \(usersByChannel[channelId]?.count ?? 0) users")
                    
                    // Add sender to audio receiver for decryption
                    if self.isVoiceAttached, let mls = self.mlsWrapper {
                        // Derive actual DAVE key from MLS if we are in the group
                        if mls.isMember(channelId: channelId, isVoice: true) {
                            do {
                                let keyBytes = try mls.exportAudioKey(channelId: channelId, senderSessionId: sessionId)
                                let epoch = try mls.currentEpoch(channelId: channelId, isVoice: true)
                                self.voice.post(.addSender(
                                    sessionId: sessionId,
                                    key: Data(keyBytes),
                                    epochHint: UInt16(epoch & 0xFFFF),
                                    prefs: self.mixerPrefs(forSession: sessionId) ?? VoiceMixerPrefs()
                                ))
                            } catch {
                                print("[QuicClient] Failed to derive MLS key for new user \(sessionId): \(error)")
                            }
                        } else {
                            print("[QuicClient] Not an MLS member for channel \(channelId), waiting for epoch advance to add sender \(sessionId)")
                        }
                    }

                    // Warm a text key for them at the current epoch so their
                    // first message survives the epoch bump their own join
                    // causes.
                    self.registerTextSender(channelId: channelId, sessionId: sessionId)
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
            voice.post(.removeSender(sessionId: sessionId))

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
                    if let profile = profile(forSession: sessionId) {
                        name = profile.displayName
//                    } else if sessionId == userId {
//                        name = identity?.displayName ?? "You"
                    }
                }
                
                // 1. Banner — only if the user asked for join and leave
                //    banners, and only while Aura is in the background.
                MessageNotifier.shared.presenceChanged(
                    "\(name) disconnected", identifier: "aura_user_left_\(sessionId)")
                
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

    /// Handle notification that we are no longer in a channel: it was deleted,
    /// it never existed, or an admin moved us out of it.
    ///
    /// The server has already torn down our MLS groups and told everyone we
    /// left, so all that remains is to say why, drop local state for a channel
    /// that is gone (not for one we were only kicked from), and move somewhere
    /// that still exists.
    private func handleChannelDeleted(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let notice = try decodeChannelDeleted(data: payload)
            let deletedId = notice.channelId
            let stillExists = ChannelRemovalNotice.channelStillExists(notice.reason)

            print("[QuicClient] Removed from channel \(deletedId) (\(notice.reason))")

            let wasCurrent = await MainActor.run { () -> Bool in
                let wasCurrent = self.currentChannelId == deletedId

                if !stillExists {
                    // Drop everything scoped to the dead channel.
                    self.usersByChannel.removeValue(forKey: deletedId)
                    self.receivedMessages.removeAll { $0.channelId == deletedId }
                    self.systemEvents.removeAll { $0.channelId == deletedId }
                }

                if wasCurrent {
                    self.forgetChannelGroups(deletedId)
                    self.currentChannelId = nil
                    self.currentVoiceChannelId = nil
                    self.connectionStatus = "Connected"
                    self.systemEvents.append(
                        SystemEvent(
                            content: ChannelRemovalNotice.text(reason: notice.reason, detail: notice.detail),
                            channelId: "0"))
                }
                return wasCurrent
            }

            guard wasCurrent else { return }

            // Fall back to the server's suggestion, else the lobby, else the
            // first channel that still exists (other than the one we were
            // just removed from).
            let fallback = await MainActor.run { () -> String? in
                if !notice.fallbackChannelId.isEmpty,
                   self.channels.contains(where: { $0.id == notice.fallbackChannelId }) {
                    return notice.fallbackChannelId
                }
                return self.channels.first(where: { $0.isLobby })?.id
                    ?? self.channels.first(where: { $0.id != deletedId })?.id
            }

            if let fallback {
                do {
                    try await joinChannel(fallback)
                } catch {
                    print("[QuicClient] Failed to move to fallback channel \(fallback): \(error)")
                }
            }
        } catch {
            print("[QuicClient] Failed to parse ChannelDeleted: \(error)")
        }
    }

    /// Handle the server's reply to our CreateChannelRequest. On success the
    /// new channel arrives via the ServerState broadcast that follows, so we
    /// only need to surface failures here.
    private func handleCreateChannelResponse(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let response = try decodeCreateChannelResponse(data: payload)
            if !response.success {
                print("[QuicClient] Create channel failed: \(response.errorMessage)")
                await MainActor.run {
                    self.demoteIfAdminRequired(response.errorMessage)
                    self.systemEvents.append(
                        SystemEvent(content: "Failed to create channel: \(response.errorMessage)"))
                }
            }
        } catch {
            print("[QuicClient] Failed to parse CreateChannelResponse: \(error)")
        }
    }

    /// Handle the server's reply to our UpdateChannelRequest.
    private func handleUpdateChannelResponse(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let response = try decodeMetadataUpdateResponse(data: payload)
            if !response.success {
                print("[QuicClient] Update channel failed: \(response.errorMessage)")
                await MainActor.run {
                    self.demoteIfAdminRequired(response.errorMessage)
                    self.systemEvents.append(
                        SystemEvent(content: "Failed to update channel: \(response.errorMessage)"))
                }
            }
        } catch {
            print("[QuicClient] Failed to parse UpdateChannel MetadataUpdateResponse: \(error)")
        }
    }

    /// Handle the server's reply to our UpdateProfile request. Not
    /// admin-gated, so no `isAdmin` bookkeeping here.
    private func handleUpdateProfileResponse(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let response = try decodeMetadataUpdateResponse(data: payload)
            await MainActor.run {
                if response.success {
                    self.finishProfileUpdate(.success(()))
                } else if self.profileUpdate != nil {
                    // The profile editor shows the reason itself.
                    self.finishProfileUpdate(.failure(QuicClientError.profileUpdateFailed(response.errorMessage)))
                } else {
                    self.systemEvents.append(
                        SystemEvent(content: "Failed to update profile: \(response.errorMessage)"))
                }
            }
            if !response.success {
                print("[QuicClient] Update profile failed: \(response.errorMessage)")
            }
        } catch {
            print("[QuicClient] Failed to parse UpdateProfile MetadataUpdateResponse: \(error)")
            await MainActor.run {
                self.finishProfileUpdate(.failure(QuicClientError.profileUpdateFailed("Unreadable reply from the server")))
            }
        }
    }

    /// Handle the server's reply to our DeleteChannelRequest.
    private func handleDeleteChannelResponse(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let response = try decodeAdminResponse(data: payload)
            if !response.success {
                print("[QuicClient] Delete channel failed: \(response.errorMessage)")
                await MainActor.run {
                    self.demoteIfAdminRequired(response.errorMessage)
                    self.systemEvents.append(
                        SystemEvent(content: "Failed to delete channel: \(response.errorMessage)"))
                }
            }
        } catch {
            print("[QuicClient] Failed to parse DeleteChannel AdminResponse: \(error)")
        }
    }

    /// Handle the server's reply to our DeleteUserRequest: an AdminResponse,
    /// framed as 0x44 like the request.
    private func handleDeleteUserResponse(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let response = try decodeAdminResponse(data: payload)
            guard accountDeletion != nil else {
                print("[QuicClient] Unexpected DeleteUser response (success=\(response.success))")
                return
            }
            if response.success {
                print("[QuicClient] Account deleted by the server")
                finishAccountDeletion(.success(()))
            } else {
                print("[QuicClient] Delete account failed: \(response.errorMessage)")
                let message = response.errorMessage.isEmpty ? "The server refused." : response.errorMessage
                finishAccountDeletion(.failure(QuicClientError.accountDeletionFailed(message)))
            }
        } catch {
            print("[QuicClient] Failed to parse DeleteUser AdminResponse: \(error)")
        }
    }

    /// The server re-checks admin status on every request rather than
    /// trusting a cached flag, but our own `isAdmin` is only set once at
    /// login (see AuthResponse handling above) and never refreshed. If a
    /// request comes back rejected specifically because admin rights are
    /// gone, drop the local flag immediately instead of waiting for a
    /// reconnect to notice the mismatch — this hides the create/edit/delete
    /// UI right away rather than letting the user hit the same rejection
    /// repeatedly.
    @MainActor
    private func demoteIfAdminRequired(_ errorMessage: String) {
        if isAdmin && errorMessage == "Admin required" {
            print("[QuicClient] Server rejected admin action; clearing stale isAdmin flag")
            isAdmin = false
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
                
                // Snapshots don't carry avatars: the server sends them after
                // login and on change as profile updates. An empty one here
                // says nothing, so keep the avatar we already hold.
                var newProfiles: [String: UserProfileRecord] = [:]
                for p in snapshot.profiles {
                    var merged = p
                    if merged.avatarData.isEmpty,
                       let known = self.profiles[p.userId]?.avatarData, !known.isEmpty {
                        merged.avatarData = known
                    }
                    newProfiles[p.userId] = merged
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
                        // Bio and avatar come from the snapshot's profiles,
                        // which are keyed by account id, not session.
                        let profile = newProfiles[userStatus.userUuid]
                        usersList.append(ChannelUser(
                            sessionId: sid,
                            displayName: userStatus.displayName,
                            bio: profile?.bio ?? "",
                            avatarData: profile.flatMap { $0.avatarData.isEmpty ? nil : $0.avatarData },
                            isMuted: userStatus.isMuted,
                            isDeafened: userStatus.isDeafened,
                            presence: self.userPresenceByUuid[userStatus.userUuid] ?? .online,
                            customStatus: self.userCustomStatusByUuid[userStatus.userUuid],
                            isServerMuted: self.userServerMutedByUuid[userStatus.userUuid] ?? false
                        ))
                    }
                    newUserMapping[c.channelId] = usersList

                    // Add listeners for decryption
                    if self.isVoiceAttached, let mls = self.mlsWrapper {
                        for userStatus in c.users {
                            let sid = userStatus.sessionId
                            if sid != self.sessionId && mls.isMember(channelId: c.channelId, isVoice: true) {
                                do {
                                    let keyBytes = try mls.exportAudioKey(channelId: c.channelId, senderSessionId: sid)
                                    let epoch = try mls.currentEpoch(channelId: c.channelId, isVoice: true)
                                    self.voice.post(.addSender(
                                        sessionId: sid,
                                        key: Data(keyBytes),
                                        epochHint: UInt16(epoch & 0xFFFF),
                                        prefs: self.mixerPrefs(forSession: sid) ?? VoiceMixerPrefs()
                                    ))
                                } catch {
                                    print("[QuicClient] Failed to derive snapshot key for \(sid): \(error)")
                                }
                            }
                        }
                    }
                }
                self.usersByChannel = newUserMapping

                // Warm text keys for members who were already present before
                // we connected — usersByChannel must be set first.
                for c in snapshot.channels {
                    self.refreshTextSenderKeys(channelId: c.channelId)
                }

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
            
            // Derive the decryption key for the epoch this message was
            // encrypted under — NOT the group's current epoch. A membership
            // change between send and receive advances the epoch, and the
            // epoch's keys are what the sender used.
            //
            // The MLS text group may not be established yet if we just joined,
            // so retry while it settles.
            var senderKey: Data?
            for attempt in 1...10 {
                do {
                    senderKey = try mls.exportTextKey(
                        channelId: channelId,
                        senderSessionId: senderSessionId,
                        epoch: epoch
                    )
                    break
                } catch MlsError.EpochUnavailable(let messageEpoch, let current) {
                    // The epoch aged out of the retention window. Retrying
                    // cannot help — the key material is gone for good.
                    print("[QuicClient] Dropping message \(messageId): epoch \(messageEpoch) no longer retained (now at \(current))")
                    return
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
            
            let announcement: DecryptedFileAnnouncement
            do {
                announcement = try decryptTextWithAttachment(daveKey: senderKey, packet: packet)
            } catch {
                print("[QuicClient] Failed to decrypt text message: \(error)")
                return
            }
            let decryptedMessage = announcement.message

            var attachmentInfo: FileAttachmentInfo?
            if let manifest = announcement.manifest {
                let info = FileAttachmentInfo(manifest: manifest)
                attachmentInfo = info
                let hex = info.fileIdHex
                pendingManifests[hex] = PendingManifest(manifest: manifest, senderSessionId: senderSessionId)
                if fileTransferStates[hex] == nil {
                    fileTransferStates[hex] = .pending
                }
                scheduleTransferTimeout(fileIdHex: hex)
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
                replyToId: replyToId,
                attachment: attachmentInfo
            )
            
            await MainActor.run {
                receivedMessages.append(message)
                print("[QuicClient] Received encrypted text message from \(senderName): \(decryptedMessage.content.prefix(30))")
                if let hex = attachmentInfo?.fileIdHex {
                    processPendingDelivery(fileIdHex: hex)
                }
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
            } else if !isVoice {
                await MainActor.run { self.refreshTextSenderKeys(channelId: channelId) }
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
                
                // Update keys after epoch advance
                if isVoice {
                    try updateAudioKeysFromMls(channelId: channelId)
                } else {
                    await MainActor.run { self.refreshTextSenderKeys(channelId: channelId) }
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

            // Update keys after epoch advance
            if isVoice {
                try updateAudioKeysFromMls(channelId: channelId)
            } else {
                await MainActor.run { self.refreshTextSenderKeys(channelId: channelId) }
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
            
            // Update keys now that we're in the group
            if isVoice {
                try updateAudioKeysFromMls(channelId: channelId)
            } else {
                await MainActor.run { self.refreshTextSenderKeys(channelId: channelId) }
            }
        } catch {
            print("[QuicClient] Failed to process MLS welcome: \(error)")
        }
    }
    
    /// Handle the server asking us (as group founder) to advance the epoch.
    ///
    /// This is the batched PFS ratchet: an empty commit rotates the group
    /// secret so keys from the previous epoch cannot decrypt future traffic.
    /// Drop our state and keys for a channel's voice and text groups.
    private func forgetChannelGroups(_ channelId: String) {
        guard let mls = mlsWrapper else { return }
        do {
            try mls.forgetGroup(channelId: channelId, isVoice: true)
            try mls.forgetGroup(channelId: channelId, isVoice: false)
        } catch {
            print("[QuicClient] Failed to forget groups for \(channelId): \(error)")
        }
    }

    /// The server lists who should still be in a group we founded (someone
    /// left the channel). Commit their removal and hand the commit back on
    /// the ratchet-commit path; nothing is sent when nobody needs removing.
    private func handleMlsSyncMembers(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let sync = try decodeMlsSyncMembers(data: payload)
            let channelId = sync.channelId
            let isVoice = sync.isVoice

            guard let mls = mlsWrapper, let controlStream else { return }
            guard mls.isMember(channelId: channelId, isVoice: isVoice) else {
                print("[QuicClient] Member sync for a group we're not in: \(channelId)")
                return
            }

            guard let commit = try mls.removeMembersExcept(
                channelId: channelId,
                isVoice: isVoice,
                keepUserIds: sync.memberUuids
            ) else { return }

            let envelopeOut = MlsEnvelopeRecord(
                senderId: sessionId ?? userId,
                channelId: channelId,
                groupType: isVoice ? .voice : .text,
                targetSessionId: 0,
                targetUuid: "",
                epoch: 0,
                keyPackage: nil,
                commit: commit,
                welcome: nil,
                commitWelcome: nil
            )

            let responsePayload = encodeMlsEnvelope(envelope: envelopeOut)
            var msg = Data([Self.MSG_MLS_RATCHET_COMMIT])
            let len = UInt32(responsePayload.count).littleEndian
            msg.append(withUnsafeBytes(of: len) { Data($0) })
            msg.append(responsePayload)

            try await send(data: msg, on: controlStream)
            print("[QuicClient] Removed departed members from \(isVoice ? "voice" : "text") group \(channelId)")

            if isVoice {
                try updateAudioKeysFromMls(channelId: channelId)
            } else {
                await MainActor.run { self.refreshTextSenderKeys(channelId: channelId) }
            }
        } catch {
            print("[QuicClient] Failed to handle MLS member sync: \(error)")
        }
    }

    private func handleMlsRatchetRequest(stream: NWConnection) async {
        do {
            let payload = try await receiveHardenedPayload(maxLen: Self.MAX_CONTROL_PACKET_SIZE, on: stream)
            let envelope = try decodeMlsEnvelope(data: payload)

            let channelId = envelope.channelId
            let isVoice = envelope.groupType == .voice

            guard let mls = mlsWrapper, let controlStream else { return }
            guard mls.isMember(channelId: channelId, isVoice: isVoice) else {
                print("[QuicClient] Ratchet requested for a group we're not in: \(channelId)")
                return
            }

            let commit = try mls.selfUpdate(channelId: channelId, isVoice: isVoice)

            let envelopeOut = MlsEnvelopeRecord(
                senderId: sessionId ?? userId,
                channelId: channelId,
                groupType: envelope.groupType,
                targetSessionId: 0,
                targetUuid: "",
                epoch: 0,
                keyPackage: nil,
                commit: commit,
                welcome: nil,
                commitWelcome: nil
            )

            let responsePayload = encodeMlsEnvelope(envelope: envelopeOut)
            var msg = Data([Self.MSG_MLS_RATCHET_COMMIT])
            let len = UInt32(responsePayload.count).littleEndian
            msg.append(withUnsafeBytes(of: len) { Data($0) })
            msg.append(responsePayload)

            try await send(data: msg, on: controlStream)

            let newEpoch = try mls.currentEpoch(channelId: channelId, isVoice: isVoice)
            print("[QuicClient] Ratcheted \(isVoice ? "voice" : "text") group \(channelId) to epoch \(newEpoch)")

            if isVoice {
                try updateAudioKeysFromMls(channelId: channelId)
            } else {
                await MainActor.run { self.refreshTextSenderKeys(channelId: channelId) }
            }
        } catch {
            print("[QuicClient] Failed to handle MLS ratchet request: \(error)")
        }
    }

    // MARK: - Text Key Retention

    /// Warm a retained text key for one sender at the group's current epoch.
    ///
    /// Deriving a key is what causes the core to retain it across the next
    /// epoch change, so doing this eagerly is what keeps a member's first
    /// message decryptable when their own join bumps the epoch.
    private func registerTextSender(channelId: String, sessionId: UInt32) {
        guard let mls = mlsWrapper,
              mls.isMember(channelId: channelId, isVoice: false) else { return }
        do {
            try mls.registerTextSender(channelId: channelId, senderSessionId: sessionId)
        } catch {
            print("[QuicClient] Failed to register text sender \(sessionId): \(error)")
        }
    }

    /// Warm retained text keys for ourselves and everyone currently visible in
    /// the channel. Called whenever the text group's epoch changes.
    private func refreshTextSenderKeys(channelId: String) {
        guard let mls = mlsWrapper,
              mls.isMember(channelId: channelId, isVoice: false) else { return }

        if let session = sessionId {
            registerTextSender(channelId: channelId, sessionId: session)
        }
        for user in usersByChannel[channelId] ?? [] {
            registerTextSender(channelId: channelId, sessionId: user.id)
        }
    }

    /// Update audio sender/receiver keys from MLS
    private func updateAudioKeysFromMls(channelId: String) throws {
        guard let mls = mlsWrapper, let session = sessionId else { return }

        // Only the channel we are in may set our sending key. MLS state for a
        // channel we left lingers, and the server still routes that group's
        // commits to us (e.g. as its founder adding a newcomer) — without
        // this, our audio in the new room would go out under the old room's
        // key, where nobody can decrypt it.
        guard channelId == currentVoiceChannelId else {
            print("[QuicClient] Ignoring voice key update for \(channelId); we are in \(currentVoiceChannelId ?? "no channel")")
            return
        }

        // Get our own key for sending
        let myKey = try mls.exportAudioKey(channelId: channelId, senderSessionId: session)
        let epoch = try mls.currentEpoch(channelId: channelId, isVoice: true)

        // Receiver keys for everyone we know in this channel. A key that
        // can't be derived is logged and left out.
        var receivers: [VoiceReceiverKey] = []
        if isVoiceAttached {
            for user in usersByChannel[channelId] ?? [] {
                do {
                    let userKey = try mls.exportAudioKey(channelId: channelId, senderSessionId: user.id)
                    receivers.append(VoiceReceiverKey(
                        sessionId: user.id,
                        key: Data(userKey),
                        prefs: mixerPrefs(forSession: user.id) ?? VoiceMixerPrefs()
                    ))
                } catch {
                    print("[QuicClient] Failed to update/add audio key for user \(user.id): \(error)")
                }
            }
        }

        // The audio domain applies these in one step, between two frames:
        // the sender (updateKey keeps sequence numbers and encoder state),
        // then each receiver key, adding senders the join race missed.
        voice.post(.rotateKeys(VoiceKeyRotation(
            channelId: channelId,
            sendKey: Data(myKey),
            epoch: epoch,
            receivers: receivers
        )))
    }
    
    /// Handle incoming audio packet from server
    private func handleAudioPacket(stream: NWConnection, legacy: Bool = false) async {
        do {
            let packetData = try await receiveHardenedPayload(maxLen: Self.MAX_AUDIO_PACKET_SIZE, on: stream)
            
            print("[QuicClient] Received audio packet: \(packetData.count) bytes")
            
            // Decrypt, decode and play in the audio domain.
            if legacy {
                voice.submitLegacyReceived(packetData)
            } else {
                voice.submitReceived(packetData)
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

    /// Hand a captured 20 ms frame to the audio domain, which meters, encodes,
    /// encrypts and sends it. Called on the capture tap's thread.
    public nonisolated func sendAudioFrame(_ floatPcm: [Float]) {
        voice.submitCaptured(floatPcm)
    }

    /// Drop the local talking signal and input meter immediately — used when
    /// capture stops so the lane does not keep a live segment open for
    /// another 500ms.
    public func clearLocalSpeaking() {
        localSpeechGeneration &+= 1
        voice.post(.clearLocalSpeech(generation: localSpeechGeneration))
        isLocalSpeaking = false
        inputLevelDb = Self.levelFloorDb
    }

    /// A meter snapshot from the audio domain.
    private func applyVoiceMeters(_ meters: VoiceMeters) {
        if meters.activeSpeakers != activeSpeakers {
            print("[QuicClient] Active speakers changed: \(activeSpeakers) -> \(meters.activeSpeakers)")
            activeSpeakers = meters.activeSpeakers
            NotificationCenter.default.post(name: .activeSpeakersChanged, object: meters.activeSpeakers)
        }
        if meters.outputLevelDb != outputLevelDb { outputLevelDb = meters.outputLevelDb }

        // Taken before the engine saw our latest clearLocalSpeaking(): its
        // local fields would undo that.
        guard meters.localSpeechGeneration == localSpeechGeneration else { return }
        if meters.inputLevelDb != inputLevelDb { inputLevelDb = meters.inputLevelDb }
        if meters.isLocalSpeaking != isLocalSpeaking { isLocalSpeaking = meters.isLocalSpeaking }
    }
    
    /// Start audio capture and send frames to network
    private func startAudioCapture() async {
        print("[QuicClient] Starting audio capture...")
        
        // Import AudioCapture if needed
        let capture = AudioCapture()
        
        capture.start { [voice] pcmData in
            voice.submitCaptured(pcmData)
        }
        
        print("[QuicClient] Audio capture started and sending packets")
    }
    
    // MARK: - Channel
    
    public func joinChannel(_ channelId: String) async throws {
        try await traced(.joinChannel) { try await self.performJoinChannel(channelId) }
    }

    private func performJoinChannel(_ channelId: String) async throws {
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

        // Leaving a channel leaves its groups: drop our copy of their state
        // and keys now. The server has the remaining founder remove our leaf.
        if let previous = currentChannelId, previous != channelId {
            forgetChannelGroups(previous)
        }
        currentChannelId = channelId
        currentVoiceChannelId = channelId
        let channelName = channels.first(where: { $0.id == channelId })?.name ?? "channel \(channelId)"
        connectionStatus = "In #\(channelName)"
        
        // Send MLS join with key package for E2EE (both voice and text groups)
        await sendMlsJoin(channelId: channelId, isVoice: true)
        await sendMlsJoin(channelId: channelId, isVoice: false)
    }
    
    // MARK: - User Status
    
    private static func mapPresence(_ record: PresenceStatusRecord) -> AuraPresenceStatus {
        switch record {
        case .online: return .online
        case .idle: return .idle
        case .dnd: return .dnd
        }
    }

    private static func mapPresenceRecord(_ status: AuraPresenceStatus) -> PresenceStatusRecord {
        switch status {
        case .online: return .online
        case .idle: return .idle
        case .dnd: return .dnd
        }
    }

    /// Update own mute/deafen/presence/custom-status
    public func updateStatus(isMuted: Bool, isDeafened: Bool) async {
        guard let sessionId = self.sessionId, let stream = controlStream else { return }

        let appSettings = AppSettings.shared
        let presence = Self.mapPresenceRecord(appSettings.presenceStatus)
        let customStatus = String(appSettings.customStatusText.prefix(128))

        let update = UserStatusUpdate(
            sessionId: sessionId,
            isMuted: isMuted,
            isDeafened: isDeafened,
            presence: presence,
            customStatus: customStatus,
            isServerMuted: nil  // set by the server only
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
                    if let idx = self.usersByChannel[channelId]?.firstIndex(where: { self.uuidForSession($0.id) == profile.userId }),
                       let existing = self.usersByChannel[channelId]?[idx] {
                        self.usersByChannel[channelId]?[idx] = ChannelUser(
                            sessionId: existing.id,
                            displayName: profile.displayName,
                            bio: profile.bio,
                            avatarData: profile.avatarData.isEmpty ? nil : profile.avatarData,
                            isMuted: existing.isMuted,
                            isDeafened: existing.isDeafened,
                            isDisconnected: existing.isDisconnected,
                            presence: existing.presence,
                            customStatus: existing.customStatus,
                            isServerMuted: existing.isServerMuted
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
                let mappedPresence = update.presence.map(Self.mapPresence)
                let normalizedCustomStatus = update.customStatus.map { $0.isEmpty ? nil : $0 }

                // Our own update: this is how a non-admin learns a moderator
                // muted them.
                if update.sessionId == self.sessionId, let serverMuted = update.isServerMuted {
                    self.isServerMuted = serverMuted
                }

                if let uuid = self.uuidForSession(update.sessionId) {
                    if let serverMuted = update.isServerMuted {
                        self.userServerMutedByUuid[uuid] = serverMuted
                    }
                    if let mappedPresence {
                        self.userPresenceByUuid[uuid] = mappedPresence
                    }
                    if let normalizedCustomStatus {
                        if let text = normalizedCustomStatus {
                            self.userCustomStatusByUuid[uuid] = text
                        } else {
                            self.userCustomStatusByUuid.removeValue(forKey: uuid)
                        }
                    }
                }

                // Update usersByChannel mapping
                for channelId in usersByChannel.keys {
                    if let index = usersByChannel[channelId]?.firstIndex(where: { $0.id == update.sessionId }) {
                        usersByChannel[channelId]?[index].isMuted = update.isMuted
                        usersByChannel[channelId]?[index].isDeafened = update.isDeafened
                        if let mappedPresence {
                            usersByChannel[channelId]?[index].presence = mappedPresence
                        }
                        if let normalizedCustomStatus {
                            usersByChannel[channelId]?[index].customStatus = normalizedCustomStatus
                        }
                        if let serverMuted = update.isServerMuted {
                            usersByChannel[channelId]?[index].isServerMuted = serverMuted
                        }
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
                // nil epoch = derive at the group's current epoch, which is
                // what we stamp on the outgoing packet below.
                myKey = try mls.exportTextKey(channelId: channelId, senderSessionId: senderSessionId, epoch: nil)
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

    // MARK: - E2EE File Transfer

    /// Encrypts `url` with a fresh XChaCha20-Poly1305 key, announces its
    /// `FileManifest` inside an E2EE text message (`MSG_TEXT_PACKET`), and
    /// streams the encrypted chunks over a dedicated QUIC bidirectional stream.
    public func sendFileAttachment(
        url: URL,
        caption: String,
        messageId: String,
        replyToId: String? = nil
    ) async throws {
        guard let stream = controlStream, let group = quicGroup else {
            throw QuicClientError.notConnected
        }
        guard let channelId = currentChannelId else {
            throw QuicClientError.protocolError("Not in a channel")
        }
        guard let mls = mlsWrapper else {
            throw QuicClientError.protocolError("MLS not initialized")
        }

        let accessed = url.startAccessingSecurityScopedResource()
        defer {
            if accessed { url.stopAccessingSecurityScopedResource() }
        }

        let fileData = try Data(contentsOf: url, options: .mappedIfSafe)
        let maxSize = maxFilePlaintextSize()
        guard UInt64(fileData.count) <= maxSize else {
            throw QuicClientError.protocolError("File exceeds the \(maxSize / (1024 * 1024)) MiB limit")
        }

        let sanitizedName = FileAttachmentInfo.sanitizeFileName(url.lastPathComponent)
        let mimeType = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
        let chunkSize = Int(fileChunkSize())

        let hasher = FileHasherWrapper()
        var offset = 0
        while offset < fileData.count {
            let end = min(offset + chunkSize, fileData.count)
            hasher.update(data: fileData.subdata(in: offset..<end))
            offset = end
        }
        let sha256 = hasher.finish()

        let encryptor = try FileEncryptorWrapper(
            fileName: sanitizedName,
            mimeType: mimeType,
            size: UInt64(fileData.count),
            sha256: sha256
        )
        let manifest = encryptor.manifest()
        let attachmentInfo = FileAttachmentInfo(manifest: manifest)
        let fileIdHex = attachmentInfo.fileIdHex
        fileTransferStates[fileIdHex] = .transferring(progress: 0)

        // Update the optimistic message with the real manifest fileId so progress & ready states match.
        if let idx = receivedMessages.firstIndex(where: { $0.id == messageId }) {
            let existing = receivedMessages[idx]
            receivedMessages[idx] = ReceivedTextMessage(
                id: existing.id,
                senderSessionId: existing.senderSessionId,
                senderName: existing.senderName,
                channelId: existing.channelId,
                content: existing.content,
                timestamp: existing.timestamp,
                rawPacket: existing.rawPacket,
                replyToId: existing.replyToId,
                attachment: attachmentInfo
            )
        }

        let senderSessionId = sessionId ?? userId
        let myKey = try mls.exportTextKey(channelId: channelId, senderSessionId: senderSessionId, epoch: nil)
        let epoch = try mls.currentEpoch(channelId: channelId, isVoice: false)

        let imageExtensions: Set<String> = ["png", "jpg", "jpeg", "gif", "webp", "heic", "tiff", "bmp"]
        let mediaType: Int32 = imageExtensions.contains(url.pathExtension.lowercased()) ? 1 : 2

        let textMsg = TextMessageRecord(
            senderUuid: "user-\(senderSessionId)",
            timestamp: UInt64(Date().timeIntervalSince1970 * 1000),
            content: caption,
            replyToId: replyToId ?? "",
            messageId: messageId,
            mediaType: mediaType,
            fileSize: manifest.size,
            sha256Hash: manifest.sha256.map { String(format: "%02x", $0) }.joined()
        )

        let encryptedPacket = try encryptTextWithAttachment(
            daveKey: myKey,
            epoch: epoch,
            channelId: channelId,
            senderSessionId: senderSessionId,
            message: textMsg,
            manifest: manifest
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

        // Open a client-initiated bidirectional stream for the encrypted chunks.
        guard let uploadStream = NWConnection(from: group) else {
            fileTransferStates[fileIdHex] = .failed("Failed to open upload stream")
            throw QuicClientError.notConnected
        }
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            let resumed = OSAllocatedUnfairLock(initialState: false)
            uploadStream.stateUpdateHandler = { state in
                guard !resumed.withLock({ $0 }) else { return }
                switch state {
                case .ready:
                    resumed.withLock { $0 = true }
                    cont.resume()
                case .failed(let error):
                    resumed.withLock { $0 = true }
                    cont.resume(throwing: error)
                case .cancelled:
                    resumed.withLock { $0 = true }
                    cont.resume(throwing: QuicClientError.connectionClosed)
                default:
                    break
                }
            }
            uploadStream.start(queue: DispatchQueue.global(qos: .userInitiated))
        }

        do {
            let header = try encryptor.streamHeader(channelId: channelId)
            try await send(data: header, on: uploadStream)

            var uploadOffset = 0
            let totalBytes = max(1, fileData.count)
            while let nextLen = encryptor.nextPlaintextLen() {
                let n = Int(nextLen)
                let end = min(uploadOffset + n, fileData.count)
                let slice = fileData.subdata(in: uploadOffset..<end)
                let framed = try encryptor.encryptNextFramed(plaintext: slice)
                try await send(data: framed, on: uploadStream)
                uploadOffset = end
                fileTransferStates[fileIdHex] = .transferring(progress: Double(uploadOffset) / Double(totalBytes))
            }
            try encryptor.finish()

            // Finish our send side so the server knows the stream is complete.
            try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
                uploadStream.send(content: nil, contentContext: .finalMessage, isComplete: true, completion: .contentProcessed { error in
                    if let error {
                        cont.resume(throwing: error)
                    } else {
                        cont.resume()
                    }
                })
            }

            // Read 8-byte server status: [code: u32 LE][recipients: u32 LE]
            let statusData = try await receiveExact(8, on: uploadStream)
            let code = statusData.subdata(in: 0..<4).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            if code != 0 {
                fileTransferStates[fileIdHex] = .failed("Upload rejected (code \(code))")
                uploadStream.cancel()
                throw QuicClientError.protocolError("File upload failed with code \(code)")
            }

            let destURL = try Self.saveDecryptedFile(fileData, fileIdHex: fileIdHex, fileName: sanitizedName)
            fileTransferStates[fileIdHex] = .ready(url: destURL)
            uploadStream.cancel()
        } catch {
            fileTransferStates[fileIdHex] = .failed(error.localizedDescription)
            uploadStream.cancel()
            throw error
        }
    }

    private func receiveFileDeliveryStream(_ connection: NWConnection) {
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready:
                Task { @MainActor [weak self] in
                    await self?.readFileDelivery(on: connection)
                }
            case .failed(let error):
                print("[QuicClient] File delivery stream failed: \(error)")
                connection.cancel()
            default:
                break
            }
        }
        connection.start(queue: DispatchQueue.global(qos: .userInitiated))
    }

    private func readFileDelivery(on connection: NWConnection) async {
        var fileIdHexForFailure: String?
        do {
            let fixedLen = Int(fileStreamHeaderFixedLen())
            let prefix = try await receiveExact(fixedLen, on: connection)
            let channelIdLen = Int(try fileStreamHeaderChannelIdLen(prefix: prefix))
            let suffix = channelIdLen > 0 ? try await receiveExact(channelIdLen, on: connection) : Data()
            var headerBytes = prefix
            headerBytes.append(suffix)
            let header = try decodeFileStreamHeader(data: headerBytes)
            let fileIdHex = header.fileId.map { String(format: "%02x", $0) }.joined()
            fileIdHexForFailure = fileIdHex

            let chunkPlaintextSize = Int(fileChunkSize())
            let maxChunks = UInt32((maxFilePlaintextSize() + UInt64(chunkPlaintextSize) - 1) / UInt64(chunkPlaintextSize))
            let maxChunkCiphertext = chunkPlaintextSize + 16
            guard header.chunkCount >= 1, header.chunkCount <= maxChunks else {
                fileTransferStates[fileIdHex] = .failed("Invalid chunk count")
                connection.cancel()
                return
            }

            if fileTransferStates[fileIdHex] == nil {
                fileTransferStates[fileIdHex] = .transferring(progress: 0)
            }
            scheduleTransferTimeout(fileIdHex: fileIdHex)

            // If manifest already arrived, verify sender and decrypt incrementally chunk-by-chunk.
            var streamingDecryptor: FileDecryptorWrapper?
            var streamingPlaintext: Data?
            var streamingManifest: FileManifestRecord?
            if let pendingManifest = pendingManifests[fileIdHex] {
                guard header.senderSessionId == pendingManifest.senderSessionId else {
                    fileTransferStates[fileIdHex] = .failed("Sender mismatch")
                    connection.cancel()
                    return
                }
                let decryptor = try FileDecryptorWrapper(manifest: pendingManifest.manifest)
                try decryptor.checkHeader(header: headerBytes)
                streamingDecryptor = decryptor
                streamingPlaintext = Data()
                streamingPlaintext?.reserveCapacity(Int(pendingManifest.manifest.size))
                streamingManifest = pendingManifest.manifest
            }

            var bufferedChunks: [Data] = []
            if streamingDecryptor == nil {
                bufferedChunks.reserveCapacity(Int(header.chunkCount))
            }

            var receivedCiphertext: UInt64 = 0
            for _ in 0..<header.chunkCount {
                let lenData = try await receiveExact(4, on: connection)
                let chunkLen = Int(lenData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian })
                guard chunkLen >= 16, chunkLen <= maxChunkCiphertext else {
                    fileTransferStates[fileIdHex] = .failed("Invalid chunk length")
                    connection.cancel()
                    return
                }
                let chunk = try await receiveExact(chunkLen, on: connection)
                scheduleTransferTimeout(fileIdHex: fileIdHex)
                if let decryptor = streamingDecryptor {
                    let decrypted = try decryptor.decryptNext(ciphertext: chunk)
                    streamingPlaintext?.append(decrypted)
                } else {
                    bufferedChunks.append(chunk)
                }
                receivedCiphertext += UInt64(chunkLen)
                if header.totalCiphertextLen > 0 {
                    let progress = min(1.0, Double(receivedCiphertext) / Double(header.totalCiphertextLen))
                    fileTransferStates[fileIdHex] = .transferring(progress: progress)
                }
            }

            connection.cancel()

            if let decryptor = streamingDecryptor, let plaintext = streamingPlaintext, let manifest = streamingManifest {
                try decryptor.finish()
                let sanitized = FileAttachmentInfo.sanitizeFileName(manifest.fileName)
                let destURL = try Self.saveDecryptedFile(plaintext, fileIdHex: fileIdHex, fileName: sanitized)
                fileTransferStates[fileIdHex] = .ready(url: destURL)
                cancelTransferTimeout(fileIdHex: fileIdHex)
                pendingManifests.removeValue(forKey: fileIdHex)
                pendingDeliveries.removeValue(forKey: fileIdHex)
                print("[QuicClient] ✓ Decrypted and verified file \(sanitized) (\(plaintext.count) bytes)")
            } else {
                pendingDeliveries[fileIdHex] = PendingDelivery(header: header, chunks: bufferedChunks)
                scheduleTransferTimeout(fileIdHex: fileIdHex)
                processPendingDelivery(fileIdHex: fileIdHex)
            }
        } catch {
            print("[QuicClient] File delivery error: \(error)")
            if let fileIdHex = fileIdHexForFailure {
                cancelTransferTimeout(fileIdHex: fileIdHex)
                fileTransferStates[fileIdHex] = .failed("Transfer failed")
                pendingDeliveries.removeValue(forKey: fileIdHex)
            }
            connection.cancel()
        }
    }

    private func scheduleTransferTimeout(fileIdHex: String) {
        transferTimeoutTasks[fileIdHex]?.cancel()
        transferTimeoutTasks[fileIdHex] = Task { [weak self] in
            try? await Task.sleep(for: .seconds(60))
            guard !Task.isCancelled, let self else { return }
            self.transferTimeoutTasks.removeValue(forKey: fileIdHex)
            let stillPending = self.pendingManifests[fileIdHex] != nil || self.pendingDeliveries[fileIdHex] != nil
            guard stillPending else { return }
            self.pendingManifests.removeValue(forKey: fileIdHex)
            self.pendingDeliveries.removeValue(forKey: fileIdHex)
            if case .ready = self.fileTransferStates[fileIdHex] {
                return
            }
            self.fileTransferStates[fileIdHex] = .failed("Transfer timed out")
        }
    }

    private func cancelTransferTimeout(fileIdHex: String) {
        transferTimeoutTasks.removeValue(forKey: fileIdHex)?.cancel()
    }

    private func processPendingDelivery(fileIdHex: String) {
        guard let pendingManifest = pendingManifests[fileIdHex],
              let delivery = pendingDeliveries[fileIdHex]
        else { return }

        // Per protocol §3.5: delivery header's sender_session_id must match the announcing message.
        guard delivery.header.senderSessionId == pendingManifest.senderSessionId else {
            print("[QuicClient] Rejecting file delivery \(fileIdHex): sender mismatch")
            pendingDeliveries.removeValue(forKey: fileIdHex)
            fileTransferStates[fileIdHex] = .failed("Sender mismatch")
            return
        }

        do {
            let decryptor = try FileDecryptorWrapper(manifest: pendingManifest.manifest)
            let headerBytes = try encodeFileStreamHeader(header: delivery.header)
            try decryptor.checkHeader(header: headerBytes)

            var plaintext = Data()
            plaintext.reserveCapacity(Int(pendingManifest.manifest.size))
            for chunk in delivery.chunks {
                let decrypted = try decryptor.decryptNext(ciphertext: chunk)
                plaintext.append(decrypted)
            }
            try decryptor.finish()

            let sanitized = FileAttachmentInfo.sanitizeFileName(pendingManifest.manifest.fileName)
            let destURL = try Self.saveDecryptedFile(plaintext, fileIdHex: fileIdHex, fileName: sanitized)
            fileTransferStates[fileIdHex] = .ready(url: destURL)
            cancelTransferTimeout(fileIdHex: fileIdHex)
            pendingManifests.removeValue(forKey: fileIdHex)
            pendingDeliveries.removeValue(forKey: fileIdHex)
            print("[QuicClient] ✓ Decrypted and verified file \(sanitized) (\(plaintext.count) bytes)")
        } catch {
            print("[QuicClient] File decryption failed for \(fileIdHex): \(error)")
            cancelTransferTimeout(fileIdHex: fileIdHex)
            fileTransferStates[fileIdHex] = .failed("Decryption failed")
            pendingDeliveries.removeValue(forKey: fileIdHex)
        }
    }

    private func receiveExact(_ length: Int, on connection: NWConnection) async throws -> Data {
        if length == 0 { return Data() }
        var accumulated = Data()
        accumulated.reserveCapacity(length)
        while accumulated.count < length {
            let needed = length - accumulated.count
            let chunk: Data = try await withCheckedThrowingContinuation { cont in
                connection.receive(minimumIncompleteLength: 1, maximumLength: needed) { data, _, isComplete, error in
                    if let error {
                        cont.resume(throwing: error)
                    } else if let data, !data.isEmpty {
                        cont.resume(returning: data)
                    } else if isComplete {
                        cont.resume(throwing: QuicClientError.connectionClosed)
                    } else {
                        cont.resume(returning: Data())
                    }
                }
            }
            if chunk.isEmpty {
                throw QuicClientError.connectionClosed
            }
            accumulated.append(chunk)
        }
        return accumulated
    }

    nonisolated private static func downloadsSessionDirectory() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("AuraDownloads", isDirectory: true)
            .appendingPathComponent(String(ProcessInfo.processInfo.processIdentifier), isDirectory: true)
    }

    nonisolated static func cleanupDownloadsCache() {
        try? FileManager.default.removeItem(at: downloadsSessionDirectory())
    }

    private static func saveDecryptedFile(_ data: Data, fileIdHex: String, fileName: String) throws -> URL {
        let dir = downloadsSessionDirectory()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let dest = dir.appendingPathComponent("\(fileIdHex.prefix(8))_\(fileName)")
        try data.write(to: dest, options: .atomic)
        return dest
    }
    
    // MARK: - Account

    /// Delete this account on the server (MSG_DELETE_USER, 0x44): the server
    /// releases the name, removes the profile and drops our session. Throws
    /// the server's error message on refusal.
    ///
    /// On success this client stops reconnecting — the session is gone, and
    /// logging in again would create a new account — so call `disconnect()`
    /// next.
    public func deleteOwnAccount() async throws {
        guard let stream = controlStream, isAuthenticated, let uuid = userUuid else {
            throw QuicClientError.notConnected
        }
        guard accountDeletion == nil else {
            throw QuicClientError.protocolError("Account deletion already in progress")
        }

        let payload = encodeDeleteUserRequest(userUuid: uuid)
        var msg = Data([Self.MSG_DELETE_USER])
        let len = UInt32(payload.count).littleEndian
        msg.append(withUnsafeBytes(of: len) { Data($0) })
        msg.append(payload)

        let token = UUID()
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            accountDeletion = continuation
            accountDeletionToken = token
            Task {
                do {
                    try await self.send(data: msg, on: stream)
                    print("[QuicClient] Delete account request sent")
                } catch {
                    self.finishAccountDeletion(.failure(error))
                }
            }
            Task {
                try? await Task.sleep(for: .seconds(15))
                guard self.accountDeletionToken == token else { return }
                self.finishAccountDeletion(.failure(QuicClientError.accountDeletionUnconfirmed))
            }
        }
    }

    private func finishAccountDeletion(_ result: Result<Void, Error>) {
        guard let continuation = accountDeletion else { return }
        accountDeletion = nil
        accountDeletionToken = nil
        if case .success = result {
            // The server has dropped our session; nothing may reconnect.
            isIntentionalDisconnect = true
            reconnectTask?.cancel()
            reconnectTask = nil
        }
        continuation.resume(with: result)
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
        isServerMuted = false
        currentChannelId = nil
        usersByChannel = [:]
        userPresenceByUuid = [:]
        userCustomStatusByUuid = [:]
        userServerMutedByUuid = [:]
        // Avatars are re-sent after the next login; dropping the cache means
        // one removed while we were away doesn't linger.
        profiles = [:]
        resetModeration()
        
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
    
    /// User-initiated retry from the connection banner: skip the remaining
    /// backoff, or start over after automatic retries gave up.
    public func retryConnectionNow() {
        guard !isConnected, !isIntentionalDisconnect, savedHost != nil else { return }
        reconnectTask?.cancel()
        reconnectTask = nil
        retryCount = 0
        isRetrying = true
        connectionStatus = "Reconnecting…"
        reconnectTask = Task { [weak self] in
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

                // The new session starts in no channel, but currentChannelId
                // still names the one we dropped out of, so the UI would show
                // us in a room the server has us out of and every send would
                // fail. Rejoin it. authenticate() built a fresh MLS client, so
                // this negotiates new groups rather than reusing stale ones. If
                // the channel was deleted meanwhile, ContentView's auto-join
                // moves us to the lobby once the new channel list lands.
                if let channelId = currentChannelId {
                    do {
                        try await joinChannel(channelId)
                    } catch {
                        print("[QuicClient] Rejoin of \(channelId) after reconnect failed: \(error)")
                    }
                }

                // Post notification for UI
                NotificationCenter.default.post(name: .connectionRestored, object: nil)
            } else {
                print("[QuicClient] No saved identity, cannot re-authenticate")
                isRetrying = false
            }
        } catch {
            print("[QuicClient] Reconnect attempt failed: \(error)")
            if case QuicClientError.authenticationFailed(let message) = error,
               ModerationLogic.isBannedRefusal(message) {
                handleBanned()
                return
            }
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
        
        if closedWithBanned() {
            handleBanned()
            return
        }

        print("[QuicClient] Connection lost, cleaning up...")

        // Mid-deletion, a reconnect would sign this key up again as a fresh
        // account. Stop here and let the user decide.
        let wasDeletingAccount = accountDeletion != nil

        // Clean up connection state
        stopKeepalive()
        stopListening()
        controlStream?.cancel()
        quicGroup?.cancel()
        controlStream = nil
        quicGroup = nil
        isConnected = false
        isAuthenticated = false
        resetModeration()
        
        connectionStatus = "Disconnected"

        if wasDeletingAccount {
            finishAccountDeletion(.failure(QuicClientError.accountDeletionUnconfirmed))
            return
        }

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
        
        // An all-default protobuf message encodes to zero bytes.
        guard length > 0 else { return Data() }
        
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
    /// The server refused to delete the account; its message.
    case accountDeletionFailed(String)
    /// No answer to a delete-account request before the connection closed
    /// or the wait ran out.
    case accountDeletionUnconfirmed
    /// The server refused a profile update, or didn't answer; the reason.
    case profileUpdateFailed(String)

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
        case .accountDeletionFailed(let message):
            return message
        case .accountDeletionUnconfirmed:
            return "The server didn't confirm the deletion. Your account may still exist."
        case .profileUpdateFailed(let message):
            return message
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
    public var presence: AuraPresenceStatus = .online
    public var customStatus: String? = nil
    public var isServerMuted: Bool = false
    
    public init(
        sessionId: UInt32,
        displayName: String,
        bio: String = "",
        avatarData: Data? = nil,
        isMuted: Bool = false,
        isDeafened: Bool = false,
        isDisconnected: Bool = false,
        presence: AuraPresenceStatus = .online,
        customStatus: String? = nil,
        isServerMuted: Bool = false
    ) {
        self.id = sessionId
        self.displayName = displayName
        self.bio = bio
        self.avatarData = avatarData
        self.isMuted = isMuted
        self.isDeafened = isDeafened
        self.isDisconnected = isDisconnected
        self.presence = presence
        self.customStatus = customStatus
        self.isServerMuted = isServerMuted
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

// MARK: - File Transfer Models

public struct FileAttachmentInfo: Equatable, Sendable {
    public let fileId: Data
    public let fileIdHex: String
    public let fileName: String
    public let mimeType: String
    public let size: UInt64

    public init(fileId: Data, fileName: String, mimeType: String, size: UInt64) {
        self.fileId = fileId
        self.fileIdHex = fileId.map { String(format: "%02x", $0) }.joined()
        self.fileName = Self.sanitizeFileName(fileName)
        self.mimeType = mimeType
        self.size = size
    }

    init(manifest: FileManifestRecord) {
        self.init(
            fileId: manifest.fileId,
            fileName: manifest.fileName,
            mimeType: manifest.mimeType,
            size: manifest.size
        )
    }

    /// Strips path separators, control characters, and bidi overrides so
    /// untrusted sender-chosen filenames cannot escape the download directory.
    public static func sanitizeFileName(_ raw: String) -> String {
        let stripped = raw.unicodeScalars.filter { scalar in
            if scalar == "/" || scalar == "\\" || scalar == "\0" { return false }
            if CharacterSet.controlCharacters.contains(scalar) { return false }
            let v = scalar.value
            if (0x202A...0x202E).contains(v) || (0x2066...0x2069).contains(v) { return false }
            return true
        }
        var name = String(String.UnicodeScalarView(stripped))
            .trimmingCharacters(in: .whitespacesAndNewlines)
        while name.hasPrefix(".") {
            name.removeFirst()
        }
        if name.isEmpty { return "attachment" }
        if name.count > 200 {
            name = String(name.prefix(200))
        }
        return name
    }
}

public enum FileTransferState: Equatable {
    case pending
    case transferring(progress: Double)
    case ready(url: URL)
    case failed(String)
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
    public let attachment: FileAttachmentInfo?

    public init(
        id: String,
        senderSessionId: UInt32,
        senderName: String,
        channelId: String,
        content: String,
        timestamp: Date,
        rawPacket: Data,
        replyToId: String? = nil,
        attachment: FileAttachmentInfo? = nil
    ) {
        self.id = id
        self.senderSessionId = senderSessionId
        self.senderName = senderName
        self.channelId = channelId
        self.content = content
        self.timestamp = timestamp
        self.rawPacket = rawPacket
        self.replyToId = replyToId
        self.attachment = attachment
    }
    
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

// MARK: - Audio settings notification

/// The values an `.audioSettingsChanged` notification can carry, copied out of
/// its `[String: Any]` payload. Each is nil when the payload doesn't set it.
nonisolated struct AudioSettingsChange: Sendable {
    let noiseSuppression: Bool?
    let aecEnabled: Bool?
    let webrtcNsEnabled: Bool?
    let webrtcAgcEnabled: Bool?
    let jitterBuffer: Int?
    let masterVolume: Float?
    let vadEnabled: Bool?
    let vadThresholdDb: Float?
    let opusBitrateKbps: Int?

    /// Nil when `payload` isn't a settings dictionary.
    init?(_ payload: Any?) {
        guard let settings = payload as? [String: Any] else { return nil }
        noiseSuppression = settings["noiseSuppression"] as? Bool
        aecEnabled = settings["aecEnabled"] as? Bool
        webrtcNsEnabled = settings["webrtcNsEnabled"] as? Bool
        webrtcAgcEnabled = settings["webrtcAgcEnabled"] as? Bool
        jitterBuffer = settings["jitterBuffer"] as? Int
        masterVolume = settings["masterVolume"] as? Float
        vadEnabled = settings["vadEnabled"] as? Bool
        vadThresholdDb = settings["vadThresholdDb"] as? Float
        opusBitrateKbps = settings["opusBitrateKbps"] as? Int
    }
}

// MARK: - Certificate pin

/// Records, from the TLS verify block's queue, whether the server's
/// certificate failed an invite's fingerprint pin.
nonisolated final class CertificatePinCheck: Sendable {
    private let didMismatch = OSAllocatedUnfairLock(initialState: false)

    var mismatched: Bool {
        didMismatch.withLock { $0 }
    }

    func recordMismatch() {
        didMismatch.withLock { $0 = true }
    }
}
