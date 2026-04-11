using System;
using System.Buffers.Binary;
using System.IO;
using System.Net;
using System.Net.Quic;
using System.Net.Security;
using System.Security.Cryptography.X509Certificates;
using System.Collections.Generic;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

using Aura.V1Alpha1;

namespace Aura.Desktop.Services;

/// <summary>
/// QUIC-based client for Aura server communication.
/// Handles authentication and audio streaming.
/// </summary>
public class AuraNetworkClient : IAsyncDisposable
{
    // Protocol message types
    private const byte MSG_MLS_JOIN = 0x50;           // Client sends key package
    private const byte MSG_MLS_COMMIT_WELCOME = 0x51; // Client sends commit + welcome
    private const byte MSG_MLS_CREATE_GROUP = 0x52;   // Server tells client to create group
    private const byte MSG_MLS_ADD_MEMBER_REQ = 0x53; // Server forwards key package
    private const byte MSG_MLS_COMMIT = 0x54;         // Server broadcasts commit
    private const byte MSG_MLS_WELCOME = 0x55;        // Server sends welcome to new member
    private const byte MSG_UPDATE_STATUS = 0x45;      // User mute/deafen sync
    
    private QuicConnection? _connection;
    private QuicStream? _controlStream;
    private uint _userId;
    private string? _sessionToken;
    private string? _userUuid;          // Stable user UUID derived from Ed25519 public key hex
    private ushort _sequenceNumber;
    private TextCryptoService? _textCrypto;
    private RustAudioEngine? _audioEngine;
    private AudioManager? _audioManager;
    private MlsWrapper? _mlsWrapper;
    private uint _currentChannelId;
    
    public void SetAudioEngine(RustAudioEngine engine) => _audioEngine = engine;
    public void SetAudioManager(AudioManager manager) => _audioManager = manager;
    
    public uint UserId => _userId;
    public string? SessionToken => _sessionToken;
    public bool IsConnected => _connection != null;
    
    public event Action<string>? OnStatusChanged;
    public event Action<string>? OnError;
    public event Action<uint, byte[]>? OnAudioReceived;
    public event Action<uint, bool, bool>? OnUserStatusUpdated; // sessionId, isMuted, isDeafened
    
    /// <summary>
    /// Connect to the Aura server via QUIC.
    /// </summary>
    public async Task ConnectAsync(string host, int port = 8443, CancellationToken ct = default)
    {
        Console.WriteLine($"[AuraClient] Connecting to {host}:{port}...");
        OnStatusChanged?.Invoke("Connecting...");
        
        // Resolve hostname if needed
        IPAddress ip;
        if (!IPAddress.TryParse(host, out ip!))
        {
            Console.WriteLine($"[AuraClient] Resolving hostname {host}...");
            var addresses = await Dns.GetHostAddressesAsync(host, ct);
            ip = addresses[0];
            Console.WriteLine($"[AuraClient] Resolved to {ip}");
        }
        
        var endpoint = new IPEndPoint(ip, port);
        Console.WriteLine($"[AuraClient] Endpoint: {endpoint}");
        
        // QUIC connection options
        var options = new QuicClientConnectionOptions
        {
            RemoteEndPoint = endpoint,
            DefaultStreamErrorCode = 0,
            DefaultCloseErrorCode = 0,
            MaxInboundUnidirectionalStreams = 10,
            MaxInboundBidirectionalStreams = 10,
            ClientAuthenticationOptions = new SslClientAuthenticationOptions
            {
                ApplicationProtocols = [new SslApplicationProtocol("aura-dave")],
                TargetHost = host,
                // Accept self-signed certificates for POC
                RemoteCertificateValidationCallback = (sender, cert, chain, errors) => 
                {
                    Console.WriteLine($"[AuraClient] TLS cert validation: errors={errors}");
                    return true; // Accept all certs for dev
                }
            }
        };
        
        Console.WriteLine("[AuraClient] Calling QuicConnection.ConnectAsync...");
        _connection = await QuicConnection.ConnectAsync(options, ct);
        Console.WriteLine("[AuraClient] QUIC connection established!");
        
        // Wait for server to open control stream (server-first protocol)
        Console.WriteLine("[AuraClient] Waiting for server to open bidirectional control stream...");
        _controlStream = await _connection.AcceptInboundStreamAsync(ct);
        Console.WriteLine("[AuraClient] Control stream received from server!");
        
        OnStatusChanged?.Invoke("Connected (unauthenticated)");
    }
    
    /// <summary>
    /// Authenticate using TOFU with Ed25519 signature.
    /// </summary>
    public async Task AuthenticateAsync(UserIdentity identity, string? serverPassword = null, CancellationToken ct = default)
    {
        if (_controlStream == null)
            throw new InvalidOperationException("Not connected");
        
        Console.WriteLine($"[AuraClient] Authenticating as '{identity.DisplayName}'...");
        OnStatusChanged?.Invoke("Authenticating...");
        
        // 1. Receive challenge (Server-first protocol: server sends challenge immediately on connection)
        Console.WriteLine("[AuraClient] Waiting for challenge response (ServerHello)...");
        var challenge = await ReceiveChallengeResponseAsync(ct);
        Console.WriteLine($"[AuraClient] Received challenge: {Convert.ToHexString(challenge[..8])}...");
        
        // 3. Sign challenge
        Console.WriteLine("[AuraClient] Signing challenge...");
        var signature = identity.Sign(challenge);
        Console.WriteLine($"[AuraClient] Signature: {Convert.ToHexString(signature[..8])}...");
        
        // 4. Send auth request  
        Console.WriteLine("[AuraClient] Sending auth request...");
        await SendAuthRequestAsync(identity.PublicKey, identity.DisplayName, signature, challenge, serverPassword, ct);
        
        // 5. Receive auth response
        Console.WriteLine("[AuraClient] Waiting for auth response...");
        var (success, userId, sessionToken, verified, errorMessage) = await ReceiveAuthResponseAsync(ct);
        Console.WriteLine($"[AuraClient] Auth response: success={success}, userId={userId}, error={errorMessage}");
        
        if (!success)
        {
            throw new AuthenticationException(errorMessage ?? "Authentication failed");
        }
        
        _userId = userId;
        _sessionToken = sessionToken;
        // Store the public key hex as the stable user UUID — this is what the server
        // stores in its database (derived from the Ed25519 public key on first TOFU auth).
        _userUuid = identity.PublicKeyHex;
        
        // Initialize MLS wrapper for E2EE
        try
        {
            _mlsWrapper = new MlsWrapper(sessionToken ?? userId.ToString());
            Console.WriteLine("[AuraClient] MLS wrapper initialized for E2EE");
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to initialize MLS: {ex.Message} - E2EE will not be available");
        }
        
        // Initialize text crypto with temporary DAVE key (will be updated with MLS-derived key on channel join)
        var tempDaveKey = new byte[32];
        for (int i = 0; i < 32; i++) tempDaveKey[i] = 0x42;
        _textCrypto = new TextCryptoService(tempDaveKey);
        
        // Initialize audio crypto with temporary DAVE key
        _audioManager?.Initialize(userId, tempDaveKey);
        
        OnStatusChanged?.Invoke($"Authenticated as user {userId}" + (verified ? " (verified)" : ""));
        
        // Start listening for server messages (presence, chat, etc.)
        StartListening();
    }
    
    private QuicStream? _audioStream;

    public async Task SendAudioFrameAsync(short[] pcmData, CancellationToken ct = default)
    {
        if (_controlStream == null) return;
        
        // Use AudioManager for Opus encoding + encryption (Opus 1.6 + DRED + DAVE)
        byte[]? encodedPacket = null;
        if (_audioManager != null)
        {
            encodedPacket = _audioManager.ProcessCapture(pcmData);
        }
        
        if (encodedPacket == null)
        {
            // Fallback: Send raw PCM if AudioManager not available
            var rawPacket = new byte[pcmData.Length * 2];
            Buffer.BlockCopy(pcmData, 0, rawPacket, 0, rawPacket.Length);
            encodedPacket = rawPacket;
        }
        
        // Send as 0x20 Audio Message
        // [type 0x20][len 4][packet]
        var frame = new byte[1 + 4 + encodedPacket.Length];
        frame[0] = 0x20;
        BinaryPrimitives.WriteInt32LittleEndian(frame.AsSpan(1, 4), encodedPacket.Length);
        encodedPacket.CopyTo(frame, 5);
        
        try 
        {
            await _controlStream.WriteAsync(frame, ct);
        }
        catch (Exception ex)
        {
            OnError?.Invoke($"Audio send error: {ex.Message}");
        }
    }
    
    /// <summary>
    /// Legacy method for backward compatibility with RustAudioEngine
    /// </summary>
    public async Task SendAudioFrameAsync(byte[] rawPcmBytes, CancellationToken ct = default)
    {
        // Convert bytes back to shorts
        var pcmData = new short[rawPcmBytes.Length / 2];
        Buffer.BlockCopy(rawPcmBytes, 0, pcmData, 0, rawPcmBytes.Length);
        await SendAudioFrameAsync(pcmData, ct);
    }
    
    /// <summary>
    /// Join a voice channel.
    /// </summary>
    public async Task JoinChannelAsync(uint channelId, CancellationToken ct = default)
    {
        if (_controlStream == null)
            throw new InvalidOperationException("Not authenticated");
        
        Console.WriteLine($"[AuraClient] Joining channel {channelId}...");
        
        // Send join channel message
        var buffer = new byte[5];
        buffer[0] = 0x10; // JoinChannel message type
        BinaryPrimitives.WriteUInt32LittleEndian(buffer.AsSpan(1, 4), channelId);
        
        await _controlStream.WriteAsync(buffer, ct);
        _currentChannelId = channelId;
        
        OnStatusChanged?.Invoke($"Joined channel {channelId}");
        
        // Send MLS join with key package for E2EE (both voice and text groups)
        await SendMlsJoinAsync(channelId, isVoice: true, ct);
        await SendMlsJoinAsync(channelId, isVoice: false, ct);
    }
    
    /// <summary>
    /// Send MLS join with key package when joining a channel.
    /// </summary>
    private async Task SendMlsJoinAsync(uint channelId, bool isVoice, CancellationToken ct = default)
    {
        if (_controlStream == null || _mlsWrapper == null)
        {
            Console.WriteLine("[AuraClient] MLS not initialized, cannot join with E2EE");
            return;
        }
        
        try
        {
            var keyPackage = _mlsWrapper.CreateKeyPackage();
            
            // [0x50] [channel_id: u32] [is_voice: u8] [kp_len: u32] [key_package]
            using var ms = new MemoryStream();
            ms.WriteByte(MSG_MLS_JOIN);
            var buf = new byte[4];
            BinaryPrimitives.WriteUInt32LittleEndian(buf, channelId);
            ms.Write(buf);
            ms.WriteByte((byte)(isVoice ? 1 : 0));
            BinaryPrimitives.WriteUInt32LittleEndian(buf, (uint)keyPackage.Length);
            ms.Write(buf);
            ms.Write(keyPackage);
            
            await _controlStream.WriteAsync(ms.ToArray(), ct);
            Console.WriteLine($"[AuraClient] Sent MLS join for {(isVoice ? "voice" : "text")} channel {channelId} ({keyPackage.Length} bytes)");
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to send MLS join: {ex.Message}");
        }
    }
    
    /// <summary>
    /// Update own mute/deafen status.
    /// </summary>
    public async Task UpdateStatusAsync(bool isMuted, bool isDeafened, CancellationToken ct = default)
    {
        if (_controlStream == null) return;
        
        var update = new UserStatusUpdate
        {
            SessionId = _userId,
            IsMuted = isMuted,
            IsDeafened = isDeafened
        };
        
        try
        {
            using var ms = new MemoryStream();
            ms.WriteByte(MSG_UPDATE_STATUS);
            
            var payload = update.ToByteArray();
            var lenBuf = new byte[4];
            BinaryPrimitives.WriteUInt32LittleEndian(lenBuf, (uint)payload.Length);
            ms.Write(lenBuf);
            ms.Write(payload);
            
            await _controlStream.WriteAsync(ms.ToArray(), ct);
            Console.WriteLine($"[AuraClient] Sent status update: muted={isMuted}, deafened={isDeafened}");
        }
        catch (Exception ex)
        {
            OnError?.Invoke($"Status update send error: {ex.Message}");
        }
    }
    
    // ========================================================================
    // Protocol Serialization (simplified for POC - not using protobuf yet)
    // ========================================================================
    
    private async Task SendChallengeRequestAsync(byte[] publicKey, CancellationToken ct)
    {
        // Simple format: [1 byte type][32 bytes pubkey]
        var buffer = new byte[33];
        buffer[0] = 0x01; // ChallengeRequest type
        publicKey.CopyTo(buffer, 1);
        
        await _controlStream!.WriteAsync(buffer, ct);
    }
    
    private async Task<byte[]> ReceiveChallengeResponseAsync(CancellationToken ct)
    {
        var buffer = new byte[33]; // [1 byte type][32 bytes challenge]
        await ReadExactAsync(buffer, ct);
        Console.WriteLine($"[AuraClient] ReceiveChallengeResponse: type={buffer[0]}");
        
        if (buffer[0] != 0x02) // ChallengeResponse type
        {
            throw new ProtocolException($"Invalid challenge response: type={buffer[0]}");
        }
        
        var challenge = new byte[32];
        Array.Copy(buffer, 1, challenge, 0, 32);
        return challenge;
    }
    
    private async Task SendAuthRequestAsync(byte[] publicKey, string displayName, byte[] signature, 
        byte[] challenge, string? serverPassword, CancellationToken ct)
    {
        // Fixed format to match server expectations:
        // [1 byte type][1 byte keylen][key][1 byte namelen][name bytes][1 byte siglen][sig][1 byte challen][chal][1 byte pwlen][pw]
        using var ms = new MemoryStream();
        
        ms.WriteByte(0x03); // AuthRequest type
        
        ms.WriteByte((byte)publicKey.Length);
        ms.Write(publicKey, 0, publicKey.Length);
        
        var nameBytes = System.Text.Encoding.UTF8.GetBytes(displayName);
        ms.WriteByte((byte)nameBytes.Length);
        ms.Write(nameBytes, 0, nameBytes.Length);
        
        ms.WriteByte((byte)signature.Length);
        ms.Write(signature, 0, signature.Length);
        
        ms.WriteByte((byte)challenge.Length);
        ms.Write(challenge, 0, challenge.Length);
        
        var pwBytes = string.IsNullOrEmpty(serverPassword) ? Array.Empty<byte>() : System.Text.Encoding.UTF8.GetBytes(serverPassword);
        ms.WriteByte((byte)pwBytes.Length);
        if (pwBytes.Length > 0)
        {
            ms.Write(pwBytes, 0, pwBytes.Length);
        }
        
        var data = ms.ToArray();
        Console.WriteLine($"[AuraClient] SendAuthRequest: {data.Length} bytes");
        await _controlStream!.WriteAsync(data, ct);
    }
    
    private async Task<(bool success, uint userId, string? sessionToken, bool verified, string? errorMessage)> 
        ReceiveAuthResponseAsync(CancellationToken ct)
    {
        var buffer = new byte[256];
        var read = await _controlStream!.ReadAsync(buffer, ct);
        Console.WriteLine($"[AuraClient] ReceiveAuthResponse: read {read} bytes, type={buffer[0]}");
        
        if (read < 2 || buffer[0] != 0x04) // AuthResponse type
        {
            return (false, 0, null, false, $"Invalid auth response: read={read}, type={buffer[0]}");
        }
        
        // Parse response: [1 type][1 success][4 userId][1 tokenLen][token...][1 verified][1 errorLen][error...]
        int pos = 1;
        var success = buffer[pos++] != 0;
        var userId = BinaryPrimitives.ReadUInt32LittleEndian(buffer.AsSpan(pos, 4));
        pos += 4;
        
        var tokenLen = buffer[pos++];
        var sessionToken = System.Text.Encoding.UTF8.GetString(buffer, pos, tokenLen);
        pos += tokenLen;
        
        var verified = buffer[pos++] != 0;
        
        var errorLen = buffer[pos++];
        var errorMessage = errorLen > 0 ? System.Text.Encoding.UTF8.GetString(buffer, pos, errorLen) : null;
        
        return (success, userId, sessionToken, verified, errorMessage);
    }
    
    public async ValueTask DisposeAsync()
    {
        if (_audioStream != null)
        {
            await _audioStream.DisposeAsync();
        }
        
        if (_controlStream != null)
        {
            await _controlStream.DisposeAsync();
        }
        
        if (_connection != null)
        {
            await _connection.DisposeAsync();
        }
    }
    // ========================================================================
    // Receive Loop & State Handlers
    // ========================================================================

    public event Action<uint, uint, string>? OnUserJoined; // channelId, sessionId, name
    public event Action<uint, uint>? OnUserLeft;           // channelId, sessionId
    public event Action<ServerState>? OnServerSnapshot;

    private CancellationTokenSource? _listenCts;

    public void StartListening()
    {
        _listenCts?.Cancel();
        _listenCts = new CancellationTokenSource();
        _ = ReceiveLoopAsync(_listenCts.Token);
    }

    private async Task ReceiveLoopAsync(CancellationToken ct)
    {
        Console.WriteLine("[AuraClient] Starting Receive Loop...");
        try
        {
            var typeBuf = new byte[1];
            while (!ct.IsCancellationRequested && _controlStream != null)
            {
                // 1. Read Message Type
                int read = await _controlStream.ReadAsync(typeBuf, ct);
                if (read == 0) break; // End of stream

                byte msgType = typeBuf[0];
                switch (msgType)
                {
                    case 0x00: // Keepalive
                        Console.WriteLine("[AuraClient] Received Keepalive");
                        break;
                    case 0x11: // UserJoined
                        await HandleUserJoinedAsync(ct);
                        break;
                    case 0x12: // UserLeft
                        await HandleUserLeftAsync(ct);
                        break;
                    case 0x13: // ChannelState
                        await HandleChannelStateAsync(ct);
                        break;
                    case 0x20: // AudioPacket
                        await HandleAudioPacketAsync(ct);
                        break;
                    case 0x30: // TextPacket
                        await HandleTextPacketAsync(ct);
                        break;
                    
                    // MLS Protocol handlers
                    case MSG_MLS_CREATE_GROUP: // 0x52 - Server tells us to create group
                        await HandleMlsCreateGroupAsync(ct);
                        break;
                    case MSG_MLS_ADD_MEMBER_REQ: // 0x53 - Server forwards key package for us to add
                        await HandleMlsAddMemberRequestAsync(ct);
                        break;
                    case MSG_MLS_COMMIT: // 0x54 - Commit from another member
                        await HandleMlsCommitAsync(ct);
                        break;
                    case MSG_MLS_WELCOME: // 0x55 - Welcome message from founder
                        await HandleMlsWelcomeAsync(ct);
                        break;
                        
                    case MSG_UPDATE_STATUS: // 0x45 - User status update
                        await HandleUserStatusUpdateAsync(ct);
                        break;
                        
                    default:
                        Console.WriteLine($"[AuraClient] Unknown message type: 0x{msgType:X2}");
                        break;
                }
            }
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            OnError?.Invoke($"Receive loop error: {ex.Message}");
        }
    }

    private async Task HandleAudioPacketAsync(CancellationToken ct)
    {
        // 1. Read Length (4 bytes)
        var lenBuf = new byte[4];
        await ReadExactAsync(lenBuf, ct);
        int len = BinaryPrimitives.ReadInt32LittleEndian(lenBuf);
        
        // 2. Read Packet
        var packet = new byte[len];
        await ReadExactAsync(packet, ct);
        
        // 3. Decrypt and decode using AudioManager
        if (_audioManager != null)
        {
            // Feed packet to Rust core for decryption + Opus decoding
            _audioManager.OnPacket(packet);
            
            // Pop mixed audio for playback
            var mixedPcm = _audioManager.PopMixed();
            if (mixedPcm != null)
            {
                // Convert shorts to bytes for RustAudioEngine
                var pcmBytes = new byte[mixedPcm.Length * 2];
                Buffer.BlockCopy(mixedPcm, 0, pcmBytes, 0, pcmBytes.Length);
                _audioEngine?.PlayAudio(pcmBytes);
            }
        }
        else
        {
            // Fallback: Play raw payload (legacy behavior)
            if (len > 32)
            {
                var payload = packet.AsSpan(32).ToArray();
                _audioEngine?.PlayAudio(payload);
            }
        }
    }

    private async Task HandleUserJoinedAsync(CancellationToken ct)
    {
        // Format: channel_id(4) + session_id(4) + name_len(1) + name(...)
        var buf = new byte[9];
        await ReadExactAsync(buf, ct);
        
        uint channelId = BinaryPrimitives.ReadUInt32LittleEndian(buf.AsSpan(0, 4));
        uint sessionId = BinaryPrimitives.ReadUInt32LittleEndian(buf.AsSpan(4, 4));
        int nameLen = buf[8];

        var nameBuf = new byte[nameLen];
        await ReadExactAsync(nameBuf, ct);
        string name = System.Text.Encoding.UTF8.GetString(nameBuf);

        Console.WriteLine($"[AuraClient] UserJoined: {name} (ID: {sessionId}) in Channel {channelId}");
        
        // Register remote sender for audio decryption
        if (_audioManager != null && _mlsWrapper != null)
        {
            if (_mlsWrapper.IsMember(channelId, isVoice: true))
            {
                try {
                    var userKey = _mlsWrapper.ExportAudioKey(channelId, sessionId);
                    var epoch = _mlsWrapper.CurrentEpoch(channelId, isVoice: true);
                    _audioManager.AddRemoteSender(sessionId, userKey);
                    _audioManager.UpdateRemoteSenderKey(sessionId, userKey, (ushort)(epoch & 0xFFFF));
                    Console.WriteLine($"[AuraClient] Added audio sender {sessionId} with MLS key");
                } catch (Exception ex) {
                    Console.WriteLine($"[AuraClient] Failed to derive MLS key for new user {sessionId}: {ex.Message}");
                }
            }
        }
        
        OnUserJoined?.Invoke(channelId, sessionId, name);
    }

    private async Task HandleUserLeftAsync(CancellationToken ct)
    {
        // Format: channel_id(4) + session_id(4)
        var buf = new byte[8];
        await ReadExactAsync(buf, ct);

        uint channelId = BinaryPrimitives.ReadUInt32LittleEndian(buf.AsSpan(0, 4));
        uint sessionId = BinaryPrimitives.ReadUInt32LittleEndian(buf.AsSpan(4, 4));

        Console.WriteLine($"[AuraClient] UserLeft: ID {sessionId} from Channel {channelId}");
        
        // Remove remote sender from audio decryption
        _audioManager?.RemoveRemoteSender(sessionId);
        
        OnUserLeft?.Invoke(channelId, sessionId);
    }

    private async Task HandleChannelStateAsync(CancellationToken ct)
    {
        // 1. Read Length (4 bytes)
        var lenBuf = new byte[4];
        await ReadExactAsync(lenBuf, ct);
        int len = BinaryPrimitives.ReadInt32LittleEndian(lenBuf);
        
        // 2. Read Packet
        var packet = new byte[len];
        await ReadExactAsync(packet, ct);

        try
        {
            // 3. Parse Protobuf ServerState
            var snapshot = ServerState.Parser.ParseFrom(packet);
            Console.WriteLine($"[AuraClient] ServerSnapshot: {snapshot.Channels.Count} channels, {snapshot.Profiles.Count} profiles");
            
            OnServerSnapshot?.Invoke(snapshot);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to parse ServerSnapshot: {ex.Message}");
        }
    }

    private async Task HandleUserStatusUpdateAsync(CancellationToken ct)
    {
        // 1. Read Length (4 bytes)
        var lenBuf = new byte[4];
        await ReadExactAsync(lenBuf, ct);
        int len = BinaryPrimitives.ReadInt32LittleEndian(lenBuf);
        
        // 2. Read Packet
        var packet = new byte[len];
        await ReadExactAsync(packet, ct);

        try
        {
            // 3. Parse Protobuf UserStatusUpdate
            var update = UserStatusUpdate.Parser.ParseFrom(packet);
            Console.WriteLine($"[AuraClient] UserStatusUpdate: User {update.SessionId}, Muted={update.IsMuted}, Deafened={update.IsDeafened}");
            
            OnUserStatusUpdated?.Invoke(update.SessionId, update.IsMuted, update.IsDeafened);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to parse UserStatusUpdate: {ex.Message}");
        }
    }

    public async Task SendTextMessageAsync(uint channelId, string content, string messageId, string? replyToId = null)
    {
        if (_controlStream == null) return;
        if (_textCrypto == null)
        {
            Console.WriteLine("[AuraClient] Text crypto not initialized");
            return;
        }
        
        Console.WriteLine($"[AuraClient] Sending encrypted text message to channel {channelId}: {content.Substring(0, Math.Min(30, content.Length))}...");
        
        // Determine the current text group epoch from MLS, falling back to 0 if not in group.
        ulong textEpoch = 0;
        if (_mlsWrapper != null)
        {
            try { textEpoch = _mlsWrapper.CurrentEpoch(_currentChannelId, isVoice: false); }
            catch { /* not in a text group yet — epoch stays 0 */ }
        }
        
        // Encrypt the message using DAVE
        var encryptedPacket = _textCrypto.Encrypt(
            epoch: textEpoch,
            channelId: channelId,
            senderSessionId: UserId,
            senderUuid: _userUuid ?? $"user-{UserId}",
            content: content,
            messageId: messageId,
            replyToId: replyToId ?? ""
        );
        
        using var ms = new MemoryStream();
        
        // Serialize encrypted packet to binary format
        // Format: sender_session_id(4) + channel_id(4) + epoch(8) + message_id_len(1) + message_id + content_len(4) + ciphertext + nonce(24) + tag(16) + reply_len(1) + reply_id
        
        // sender_session_id(4)
        var senderBytes = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(senderBytes, encryptedPacket.SenderSessionId);
        ms.Write(senderBytes);
        
        // channel_id(4)
        var chanBytes = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(chanBytes, encryptedPacket.ChannelId);
        ms.Write(chanBytes);
        
        // epoch(8)
        var epochBytes = new byte[8];
        BinaryPrimitives.WriteUInt64LittleEndian(epochBytes, encryptedPacket.Epoch);
        ms.Write(epochBytes);
        
        // message_id_len(1) + message_id
        var msgIdBytes = Encoding.UTF8.GetBytes(messageId);
        ms.WriteByte((byte)msgIdBytes.Length);
        ms.Write(msgIdBytes);
        
        // ciphertext_len(4) + ciphertext
        var ciphertextLenBytes = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(ciphertextLenBytes, (uint)encryptedPacket.Ciphertext.Length);
        ms.Write(ciphertextLenBytes);
        ms.Write(encryptedPacket.Ciphertext);
        
        // nonce(24)
        ms.Write(encryptedPacket.Nonce);
        
        // tag(16)
        ms.Write(encryptedPacket.Tag);
        
        // reply_to(1 + bytes)
        if (!string.IsNullOrEmpty(replyToId))
        {
            var replyBytes = Encoding.UTF8.GetBytes(replyToId);
            ms.WriteByte((byte)replyBytes.Length);
            ms.Write(replyBytes);
        }
        else
        {
            ms.WriteByte(0);
        }
        
        var packet = ms.ToArray();
        
        // Send: [type 0x30][len 4][packet]
        var frame = new byte[1 + 4 + packet.Length];
        frame[0] = 0x30;
        BinaryPrimitives.WriteInt32LittleEndian(frame.AsSpan(1, 4), packet.Length);
        packet.CopyTo(frame, 5);
        
        await _controlStream.WriteAsync(frame);
        Console.WriteLine($"[AuraClient] Sent encrypted text message ({frame.Length} bytes)");
    }

    public event Action<string, uint, uint, string, string?>? OnTextMessage; // msgId, senderId, channelId, content, replyToId

    private async Task HandleTextPacketAsync(CancellationToken ct)
    {
        // 1. Read Length (4 bytes)
        var lenBuf = new byte[4];
        await ReadExactAsync(lenBuf, ct);
        int len = BinaryPrimitives.ReadInt32LittleEndian(lenBuf);
        
        // 2. Read Packet
        var packet = new byte[len];
        await ReadExactAsync(packet, ct);
        
        // 3. Parse Encrypted Packet
        int offset = 0;
        
        uint senderId = BinaryPrimitives.ReadUInt32LittleEndian(packet.AsSpan(offset, 4));
        offset += 4;
        
        uint channelId = BinaryPrimitives.ReadUInt32LittleEndian(packet.AsSpan(offset, 4));
        offset += 4;
        
        ulong epoch = BinaryPrimitives.ReadUInt64LittleEndian(packet.AsSpan(offset, 8));
        offset += 8;
        
        int msgIdLen = packet[offset++];
        string msgId = Encoding.UTF8.GetString(packet.AsSpan(offset, msgIdLen));
        offset += msgIdLen;
        
        int ciphertextLen = BinaryPrimitives.ReadInt32LittleEndian(packet.AsSpan(offset, 4));
        offset += 4;
        
        var ciphertext = packet.AsSpan(offset, ciphertextLen).ToArray();
        offset += ciphertextLen;
        
        var nonce = packet.AsSpan(offset, 24).ToArray();
        offset += 24;
        
        var tag = packet.AsSpan(offset, 16).ToArray();
        offset += 16;
        
        string? replyToId = null;
        if (offset < packet.Length)
        {
            int replyLen = packet[offset++];
            if (replyLen > 0)
            {
                replyToId = Encoding.UTF8.GetString(packet.AsSpan(offset, replyLen));
            }
        }
        
        // 4. Decrypt the message
        if (_textCrypto == null)
        {
            Console.WriteLine("[AuraClient] Text crypto not initialized, cannot decrypt");
            return;
        }
        
        var encryptedPacket = new EncryptedTextPacket
        {
            SenderSessionId = senderId,
            ChannelId = channelId,
            Epoch = epoch,
            Ciphertext = ciphertext,
            Nonce = nonce,
            Tag = tag,
            MessageId = msgId,
            ReplyToId = replyToId ?? ""
        };
        
        try
        {
            var decryptedMessage = _textCrypto.Decrypt(encryptedPacket);
            Console.WriteLine($"[AuraClient] Decrypted text: {decryptedMessage.Content} from {senderId}");
            OnTextMessage?.Invoke(msgId, senderId, channelId, decryptedMessage.Content, replyToId);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to decrypt text message: {ex.Message}");
        }
    }

    private async Task ReadExactAsync(byte[] buf, CancellationToken ct)
    {
        int offset = 0;
        while (offset < buf.Length)
        {
            int read = await _controlStream!.ReadAsync(buf.AsMemory(offset), ct);
            if (read == 0) throw new EndOfStreamException();
            offset += read;
        }
    }
    
    // ========================================================================
    // MLS Protocol Handlers
    // ========================================================================
    
    /// <summary>
    /// Handle server telling us to create a new MLS group (we're the first joiner).
    /// </summary>
    private async Task HandleMlsCreateGroupAsync(CancellationToken ct)
    {
        // [channel_id: u32] [is_voice: u8]
        var buf = new byte[5];
        await ReadExactAsync(buf, ct);
        uint channelId = BinaryPrimitives.ReadUInt32LittleEndian(buf.AsSpan(0, 4));
        bool isVoice = buf[4] != 0;
        
        if (_mlsWrapper == null)
        {
            Console.WriteLine("[AuraClient] MLS not initialized");
            return;
        }
        
        try
        {
            _mlsWrapper.CreateGroup(channelId, isVoice);
            Console.WriteLine($"[AuraClient] Created MLS {(isVoice ? "voice" : "text")} group for channel {channelId}");
            
            // Update audio keys if we're the founder of a voice group
            if (isVoice)
            {
                UpdateAudioKeysFromMls(channelId);
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to create MLS group: {ex.Message}");
        }
    }
    
    /// <summary>
    /// Handle server forwarding a key package for us to add (we're a founder).
    /// </summary>
    private async Task HandleMlsAddMemberRequestAsync(CancellationToken ct)
    {
        // [channel_id: u32] [is_voice: u8] [joiner_session_id: u32] [uuid_len: u8] [uuid] [kp_len: u32] [key_package]
        var headerBuf = new byte[9];
        await ReadExactAsync(headerBuf, ct);
        
        uint channelId = BinaryPrimitives.ReadUInt32LittleEndian(headerBuf.AsSpan(0, 4));
        bool isVoice = headerBuf[4] != 0;
        uint joinerSessionId = BinaryPrimitives.ReadUInt32LittleEndian(headerBuf.AsSpan(5, 4));
        
        var uuidLenBuf = new byte[1];
        await ReadExactAsync(uuidLenBuf, ct);
        var uuidBuf = new byte[uuidLenBuf[0]];
        await ReadExactAsync(uuidBuf, ct);
        
        var kpLenBuf = new byte[4];
        await ReadExactAsync(kpLenBuf, ct);
        uint kpLen = BinaryPrimitives.ReadUInt32LittleEndian(kpLenBuf);
        var keyPackage = new byte[kpLen];
        await ReadExactAsync(keyPackage, ct);
        
        if (_mlsWrapper == null || _controlStream == null)
        {
            Console.WriteLine("[AuraClient] MLS not initialized");
            return;
        }
        
        try
        {
            // Add the member - returns commit and welcome
            var result = _mlsWrapper.AddMember(channelId, isVoice, keyPackage);
            Console.WriteLine($"[AuraClient] Added member {joinerSessionId} to MLS group, sending commit/welcome");
            
            // Send commit + welcome back to server
            // [0x51] [channel_id: u32] [is_voice: u8] [new_member_session_id: u32]
            //        [commit_len: u32] [commit] [welcome_len: u32] [welcome]
            using var ms = new MemoryStream();
            ms.WriteByte(MSG_MLS_COMMIT_WELCOME);
            var buf = new byte[4];
            BinaryPrimitives.WriteUInt32LittleEndian(buf, channelId);
            ms.Write(buf);
            ms.WriteByte((byte)(isVoice ? 1 : 0));
            BinaryPrimitives.WriteUInt32LittleEndian(buf, joinerSessionId);
            ms.Write(buf);
            BinaryPrimitives.WriteUInt32LittleEndian(buf, (uint)result.Commit.Length);
            ms.Write(buf);
            ms.Write(result.Commit);
            BinaryPrimitives.WriteUInt32LittleEndian(buf, (uint)result.Welcome.Length);
            ms.Write(buf);
            ms.Write(result.Welcome);
            
            await _controlStream.WriteAsync(ms.ToArray(), ct);
            Console.WriteLine($"[AuraClient] Sent commit/welcome for new member {joinerSessionId}");
            
            // Update audio keys after epoch advance
            if (isVoice)
            {
                UpdateAudioKeysFromMls(channelId);
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to handle MLS add member: {ex.Message}");
        }
    }
    
    /// <summary>
    /// Handle commit message from another member.
    /// </summary>
    private async Task HandleMlsCommitAsync(CancellationToken ct)
    {
        // [channel_id: u32] [is_voice: u8] [commit_len: u32] [commit]
        var headerBuf = new byte[5];
        await ReadExactAsync(headerBuf, ct);
        
        uint channelId = BinaryPrimitives.ReadUInt32LittleEndian(headerBuf.AsSpan(0, 4));
        bool isVoice = headerBuf[4] != 0;
        
        var lenBuf = new byte[4];
        await ReadExactAsync(lenBuf, ct);
        uint commitLen = BinaryPrimitives.ReadUInt32LittleEndian(lenBuf);
        var commit = new byte[commitLen];
        await ReadExactAsync(commit, ct);
        
        if (_mlsWrapper == null) return;
        
        try
        {
            var newEpoch = _mlsWrapper.ProcessCommit(channelId, isVoice, commit);
            Console.WriteLine($"[AuraClient] Processed MLS commit, now at epoch {newEpoch}");
            
            // Update audio keys after epoch advance
            if (isVoice)
            {
                UpdateAudioKeysFromMls(channelId);
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to process MLS commit: {ex.Message}");
        }
    }
    
    /// <summary>
    /// Handle welcome message (we were just added to a group).
    /// </summary>
    private async Task HandleMlsWelcomeAsync(CancellationToken ct)
    {
        // [channel_id: u32] [is_voice: u8] [welcome_len: u32] [welcome]
        var headerBuf = new byte[5];
        await ReadExactAsync(headerBuf, ct);
        
        uint channelId = BinaryPrimitives.ReadUInt32LittleEndian(headerBuf.AsSpan(0, 4));
        bool isVoice = headerBuf[4] != 0;
        
        var lenBuf = new byte[4];
        await ReadExactAsync(lenBuf, ct);
        uint welcomeLen = BinaryPrimitives.ReadUInt32LittleEndian(lenBuf);
        var welcome = new byte[welcomeLen];
        await ReadExactAsync(welcome, ct);
        
        if (_mlsWrapper == null) return;
        
        try
        {
            _mlsWrapper.JoinGroup(welcome);
            Console.WriteLine($"[AuraClient] Joined MLS {(isVoice ? "voice" : "text")} group via Welcome for channel {channelId}");
            
            // Update audio keys now that we're in the group
            if (isVoice)
            {
                UpdateAudioKeysFromMls(channelId);
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to process MLS welcome: {ex.Message}");
        }
    }
    
    /// <summary>
    /// Update audio sender/receiver keys from MLS.
    /// </summary>
    private void UpdateAudioKeysFromMls(uint channelId)
    {
        if (_mlsWrapper == null) return;
        
        try
        {
            // Get our own key for sending
            var myKey = _mlsWrapper.ExportAudioKey(channelId, _userId);
            var epoch = _mlsWrapper.CurrentEpoch(channelId, isVoice: true);
            
            // Update local sender key
            _audioManager?.UpdateSenderKey(myKey, epoch);
            Console.WriteLine($"[AuraClient] Rotated audio sender key from MLS, epoch={epoch}");

            // Update receiver keys for all known users in this channel
            // Note: AuraNetworkClient doesn't maintain a full user list itself, 
            // but we can trigger it from wherever the user list is managed (e.g. MainWindowViewModel)
            // Or we check if we have any active streams in AudioManager.
            // For now, we rely on HandleUserJoined which is more common.
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to update audio keys: {ex.Message}");
        }
    }
}

public class AuthenticationException : Exception
{
    public AuthenticationException(string message) : base(message) { }
}

public class ProtocolException : Exception
{
    public ProtocolException(string message) : base(message) { }
}
