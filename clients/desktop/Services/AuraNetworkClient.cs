using System;
using System.Buffers.Binary;
using System.IO;
using System.Net;
using System.Net.Quic;
using System.Net.Security;
using System.Security.Cryptography.X509Certificates;
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
        
        OnStatusChanged?.Invoke($"Authenticated as user {userId}" + (verified ? " (verified)" : ""));
    }
    
    private QuicStream? _audioStream;

    /// <summary>
    /// Send audio frame to server.
    /// </summary>
    public async Task SendAudioFrameAsync(byte[] pcmData, CancellationToken ct = default)
    {
        if (_connection == null)
            return;
        
        // Open audio stream on first use
        if (_audioStream == null)
        {
            Console.WriteLine("[AuraClient] Opening unidirectional audio stream...");
            _audioStream = await _connection.OpenOutboundStreamAsync(QuicStreamType.Unidirectional, ct);
        }
        
        // Build FastAudioPacket header (32 bytes) + payload
        var packet = new byte[32 + pcmData.Length];
        
        // session_id: u32 (4 bytes)
        BinaryPrimitives.WriteUInt32LittleEndian(packet.AsSpan(0, 4), _userId);
        
        // epoch_hint: u16 (2 bytes) - POC: 0
        BinaryPrimitives.WriteUInt16LittleEndian(packet.AsSpan(4, 2), 0);
        
        // sequence: u16 (2 bytes)
        BinaryPrimitives.WriteUInt16LittleEndian(packet.AsSpan(6, 2), _sequenceNumber++);
        
        // nonce: [u8; 24] - POC: zeros
        // packet[8..32] = zeros (already initialized)
        
        // payload
        pcmData.CopyTo(packet, 32);
        
        // Write length prefix + packet to stream
        var lengthPrefix = new byte[4];
        BinaryPrimitives.WriteInt32LittleEndian(lengthPrefix, packet.Length);
        
        try
        {
            await _audioStream.WriteAsync(lengthPrefix, ct);
            await _audioStream.WriteAsync(packet, ct);
        }
        catch (Exception ex)
        {
            OnError?.Invoke($"Audio send failed: {ex.Message}");
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
}

public class AuthenticationException : Exception
{
    public AuthenticationException(string message) : base(message) { }
}

public class ProtocolException : Exception
{
    public ProtocolException(string message) : base(message) { }
}
