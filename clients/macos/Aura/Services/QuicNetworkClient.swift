import Foundation
import Combine
import Network

/// Native QUIC client for Aura server using Apple's Network framework.
/// Uses NWConnectionGroup to handle server-initiated streams for Apple/Quinn interop.
@MainActor
public class QuicNetworkClient: ObservableObject {
    
    // MARK: - Published State
    
    @Published public var isConnected = false
    @Published public var isAuthenticated = false
    @Published public var connectionStatus = "Disconnected"
    @Published public var userId: UInt32 = 0
    @Published public var sessionToken: String?
    
    // MARK: - Private Properties
    
    /// The QUIC connection group (manages the tunnel and accepts streams)
    private var connectionGroup: NWConnectionGroup?
    
    /// Control stream from server (for auth and signaling)
    private var controlStream: NWConnection?
    
    /// Continuation for waiting on server stream
    private var streamContinuation: CheckedContinuation<NWConnection, Error>?
    
    private var sequenceNumber: UInt16 = 0
    
    // Protocol message types (matching server)
    private static let MSG_CHALLENGE_REQUEST: UInt8 = 0x01
    private static let MSG_CHALLENGE_RESPONSE: UInt8 = 0x02
    private static let MSG_AUTH_REQUEST: UInt8 = 0x03
    private static let MSG_AUTH_RESPONSE: UInt8 = 0x04
    private static let MSG_JOIN_CHANNEL: UInt8 = 0x10
    
    // ALPN protocol identifier
    private static let ALPN = "aura-dave"
    
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
    }
    
    // MARK: - Audio (via Control Stream for now)
    
    /// Send audio frame via control stream.
    /// TODO: Switch to proper QUIC datagrams once API is figured out.
    public func sendAudioDatagram(_ encryptedBytes: Data) async throws {
        guard isAuthenticated else { return }
        guard let stream = controlStream else { return }
        
        // Build audio packet header
        var packet = Data()
        
        // Message type for audio (0x20)
        packet.append(0x20)
        
        // session_id: u32 (4 bytes)
        packet.append(contentsOf: withUnsafeBytes(of: userId.littleEndian) { Data($0) })
        
        // sequence: u16 (2 bytes)  
        packet.append(contentsOf: withUnsafeBytes(of: sequenceNumber.littleEndian) { Data($0) })
        sequenceNumber &+= 1
        
        // payload length: u16 (2 bytes)
        let payloadLen = UInt16(encryptedBytes.count)
        packet.append(contentsOf: withUnsafeBytes(of: payloadLen.littleEndian) { Data($0) })
        
        // audio payload
        packet.append(encryptedBytes)
        
        // Send via control stream
        try await send(data: packet, on: stream)
        
        if sequenceNumber % 50 == 1 {
            print("[QuicClient] Sent audio packet #\(sequenceNumber - 1) (\(packet.count) bytes)")
        }
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
        connectionStatus = "Joined channel \(channelId)"
    }
    
    // MARK: - Disconnect
    
    public func disconnect() {
        controlStream?.cancel()
        connectionGroup?.cancel()
        controlStream = nil
        connectionGroup = nil
        isConnected = false
        isAuthenticated = false
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
