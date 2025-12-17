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

namespace Aura.Desktop.Services;

/// <summary>
/// QUIC-based client for Aura server communication.
/// Handles authentication and audio streaming.
/// </summary>
public class AuraNetworkClient : IAsyncDisposable
{
    private QuicConnection? _connection;
    private QuicStream? _controlStream;
    private uint _userId;
    private string? _sessionToken;
    private ushort _sequenceNumber;
    private TextCryptoService? _textCrypto;
    
    public uint UserId => _userId;
    public string? SessionToken => _sessionToken;
    public bool IsConnected => _connection != null;
    
    public event Action<string>? OnStatusChanged;
    public event Action<string>? OnError;
    public event Action<uint, byte[]>? OnAudioReceived;
    
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
                ApplicationProtocols = [new SslApplicationProtocol("aura")],
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
        
        // Open control stream for authentication
        Console.WriteLine("[AuraClient] Opening bidirectional control stream...");
        _controlStream = await _connection.OpenOutboundStreamAsync(QuicStreamType.Bidirectional, ct);
        Console.WriteLine("[AuraClient] Control stream opened!");
        
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
        
        // 1. Send challenge request
        Console.WriteLine("[AuraClient] Sending challenge request...");
        await SendChallengeRequestAsync(identity.PublicKey, ct);
        
        // 2. Receive challenge
        Console.WriteLine("[AuraClient] Waiting for challenge response...");
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
        
        // Initialize text crypto with DAVE key (using hardcoded key for POC)
        var daveKey = new byte[32];
        for (int i = 0; i < 32; i++) daveKey[i] = 0x42;  // TODO: Derive from MLS
        _textCrypto = new TextCryptoService(daveKey);
        
        OnStatusChanged?.Invoke($"Authenticated as user {userId}" + (verified ? " (verified)" : ""));
        
        // Start listening for server messages (presence, chat, etc.)
        StartListening();
    }
    
    private QuicStream? _audioStream;

    /// <summary>
    /// Send audio frame to server.
    /// </summary>
    private AudioPlayback _audioPlayback = new();

    public async Task SendAudioFrameAsync(byte[] pcmData, CancellationToken ct = default)
    {
        if (_controlStream == null) return;
        
        // Build FastAudioPacket header (32 bytes) + payload
        // session_id(4) + epoch_hint(2) + sequence(2) + nonce(24) + payload
        var packet = new byte[32 + pcmData.Length];
        
        BinaryPrimitives.WriteUInt32LittleEndian(packet.AsSpan(0, 4), UserId);
        BinaryPrimitives.WriteUInt16LittleEndian(packet.AsSpan(4, 2), 0); // epoch
        BinaryPrimitives.WriteUInt16LittleEndian(packet.AsSpan(6, 2), _sequenceNumber++);
        // nonce (8..32) is zeros
        
        pcmData.CopyTo(packet, 32);
        
        // Send as 0x20 Control Message
        // [type 0x20][len 4][packet]
        var frame = new byte[1 + 4 + packet.Length];
        frame[0] = 0x20;
        BinaryPrimitives.WriteInt32LittleEndian(frame.AsSpan(1, 4), packet.Length);
        packet.CopyTo(frame, 5);
        
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
    /// Join a voice channel.
    /// </summary>
    public async Task JoinChannelAsync(uint channelId, CancellationToken ct = default)
    {
        if (_controlStream == null)
            throw new InvalidOperationException("Not authenticated");
        
        Console.WriteLine($"[AuraClient] Joining channel {channelId}...");
        
        // Send join channel message (simplified for POC)
        var buffer = new byte[5];
        buffer[0] = 0x10; // JoinChannel message type
        BinaryPrimitives.WriteUInt32LittleEndian(buffer.AsSpan(1, 4), channelId);
        
        await _controlStream.WriteAsync(buffer, ct);
        
        OnStatusChanged?.Invoke($"Joined channel {channelId}");
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
        var read = await _controlStream!.ReadAsync(buffer, ct);
        Console.WriteLine($"[AuraClient] ReceiveChallengeResponse: read {read} bytes, type={buffer[0]}");
        
        if (read < 33 || buffer[0] != 0x02) // ChallengeResponse type
        {
            throw new ProtocolException($"Invalid challenge response: read={read}, type={buffer[0]}");
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
    public event Action<uint, List<(uint, string)>>? OnChannelState; // channelId, users

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
        
        // 3. Process Header (FastAudioPacket)
        // Header size = 32 bytes.
        if (len <= 32) return; // Header only or invalid?
        
        // Extract Payload (skip 32 bytes)
        var payloadDesc = packet.AsSpan(32);
        
        // In POC, payload is Raw PCM?
        // Swift sends EncryptedOpus.
        // Server sends EncryptedOpus.
        // If we receive EncryptedOpus and treat as PCM, it will be static noise.
        // BUT, for unencrypted usage (if any), this works.
        // For now, we queue it. Ideally we would decrypt/decode.
        // Since we lack bindings, we just output it.
        // If the server sends raw PCM (some debug modes?), it works.
        // Otherwise, this completes the transport pipe at least.
        
        var payload = payloadDesc.ToArray();
        _audioPlayback.Enqueue(payload);
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
        OnUserLeft?.Invoke(channelId, sessionId);
    }

    private async Task HandleChannelStateAsync(CancellationToken ct)
    {
        // Format: channel_id(4) + user_count(1) + [session_id(4) + name_len(1) + name(...)]...
        var header = new byte[5];
        await ReadExactAsync(header, ct);

        uint channelId = BinaryPrimitives.ReadUInt32LittleEndian(header.AsSpan(0, 4));
        int userCount = header[4];

        var users = new List<(uint, string)>();
        for (int i = 0; i < userCount; i++)
        {
            var userHeader = new byte[5];
            await ReadExactAsync(userHeader, ct);

            uint sessionId = BinaryPrimitives.ReadUInt32LittleEndian(userHeader.AsSpan(0, 4));
            int nameLen = userHeader[4];

            var nameBuf = new byte[nameLen];
            await ReadExactAsync(nameBuf, ct);
            string name = System.Text.Encoding.UTF8.GetString(nameBuf);

            users.Add((sessionId, name));
        }

        Console.WriteLine($"[AuraClient] ChannelState: {users.Count} users in Channel {channelId}");
        OnChannelState?.Invoke(channelId, users);
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
        
        // Encrypt the message using DAVE
        var encryptedPacket = _textCrypto.Encrypt(
            epoch: 0,  // TODO: Use actual text epoch from MLS
            channelId: channelId,
            senderSessionId: UserId,
            senderUuid: $"user-{UserId}",  // TODO: Use real UUID from identity
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
}

public class AuthenticationException : Exception
{
    public AuthenticationException(string message) : base(message) { }
}

public class ProtocolException : Exception
{
    public ProtocolException(string message) : base(message) { }
}
