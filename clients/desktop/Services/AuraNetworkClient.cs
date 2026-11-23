using System;
using System.Buffers.Binary;
using System.IO;
using System.Net;
using System.Net.Quic;
using System.Net.Security;
using System.Security.Cryptography.X509Certificates;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Security.Authentication;

using Aura.V1Alpha1;
using Google.Protobuf;
using System.Collections.Concurrent;
using uniffi.aura_core;

namespace Aura.Desktop.Services;

public class ProtocolException : Exception
{
    public ProtocolException(string message) : base(message) { }
}

public class UntrustedCertificateException : Exception
{
    public string Host { get; }
    public string Fingerprint { get; }
    public X509Certificate Certificate { get; }
    
    public UntrustedCertificateException(string host, string fingerprint, X509Certificate certificate) 
        : base($"Untrusted certificate from {host} (SHA256: {fingerprint})")
    {
        Host = host;
        Fingerprint = fingerprint;
        Certificate = certificate;
    }
}

/// <summary>
/// The server's certificate is not the one an invite link pinned. Never
/// offered as trust-and-connect: the link is the out-of-band check.
/// </summary>
public class CertificateMismatchException : Exception
{
    public string Host { get; }
    public string Fingerprint { get; }

    public CertificateMismatchException(string host, string fingerprint)
        : base("Server certificate doesn't match the invite")
    {
        Host = host;
        Fingerprint = fingerprint;
    }
}

/// <summary>
/// QUIC-based client for Aura server communication.
/// </summary>
public class AuraNetworkClient : IAsyncDisposable
{
    // Protocol message types
    private const byte MSG_KEEPALIVE = 0x00;
    private const byte MSG_AUTH_RESPONSE = 0x04;
    private const byte MSG_AUDIO = 0x20;
    private const byte MSG_CREATE_CHANNEL = 0x40;
    private const byte MSG_UPDATE_CHANNEL = 0x41;
    private const byte MSG_UPDATE_PROFILE = 0x42;
    private const byte MSG_DELETE_CHANNEL = 0x43;
    private const byte MSG_DELETE_USER = 0x44;
    private const byte MSG_PROFILE_UPDATED = 0x46;
    private const byte MSG_JOIN_CHANNEL = 0x10;
    private const byte MSG_USER_JOINED = 0x11;
    private const byte MSG_USER_LEFT = 0x12;
    private const byte MSG_CHANNEL_STATE = 0x13;
    private const byte MSG_TEXT_PACKET = 0x30;
    private const byte MSG_UPDATE_STATUS = 0x45;
    private const byte MSG_CHANNEL_DELETED = 0x47;
    private const byte MSG_MLS_RATCHET_REQUEST = 0x56;
    private const byte MSG_MLS_RATCHET_COMMIT = 0x57; // a commit with no Welcome: ratchet or removals
    private const byte MSG_MLS_SYNC_MEMBERS = 0x58;   // server tells us (founder) who should remain
    private const byte MSG_MLS_JOIN = 0x50;           
    private const byte MSG_MLS_COMMIT_WELCOME = 0x51; 
    private const byte MSG_MLS_CREATE_GROUP = 0x52;   
    private const byte MSG_MLS_ADD_MEMBER_REQ = 0x53; 
    private const byte MSG_MLS_COMMIT = 0x54;         
    private const byte MSG_MLS_WELCOME = 0x55;        
    
    private QuicConnection? _connection;
    private QuicStream? _controlStream;
    private uint _userId;
    private string? _sessionToken;
    private string? _userUuid;          // Server-assigned account id (U_…) from the login response
    private readonly ConcurrentDictionary<string, TaskCompletionSource> _textGroupReady = new();
    private readonly ConcurrentDictionary<string, TaskCompletionSource> _voiceGroupReady = new();
    private RustAudioEngine? _audioEngine;
    private AudioManager? _audioManager;
    private MlsWrapper? _mlsWrapper;
    private string _currentChannelId = "";

    /// <summary>
    /// One writer at a time on the control stream. Audio frames are sent from
    /// the capture thread ~50×/s while signalling, chat and keepalives go out
    /// from elsewhere; QuicStream does not allow overlapping writes, and two
    /// frames interleaving would corrupt the stream for the server.
    /// </summary>
    private readonly SemaphoreSlim _writeLock = new(1, 1);

    private CancellationTokenSource? _keepaliveCts;

    /// <summary>Set once we tear down on purpose, so the loss handler stays quiet.</summary>
    private volatile bool _disposed;

    /// <summary>
    /// The server drops a session after 30s with no application-level traffic
    /// (crates/aura-server/src/connection.rs, keepalive_timeout). An idle user —
    /// in a channel, not talking, not typing — would otherwise be disconnected
    /// every half minute.
    /// </summary>
    private static readonly TimeSpan KeepaliveInterval = TimeSpan.FromSeconds(10);

    /// <summary>
    /// Constructor key for the audio sender before any group exists. It is a
    /// public constant, which is exactly why <see cref="AudioManager"/> refuses
    /// to send until an MLS-exported key replaces it.
    /// </summary>
    private static readonly byte[] BootstrapAudioKey = Enumerable.Repeat((byte)0x42, 32).ToArray();

    public void SetAudioEngine(RustAudioEngine engine) => _audioEngine = engine;
    public void SetAudioManager(AudioManager manager) => _audioManager = manager;

    /// <summary>
    /// SHA-256 (hex) of the leaf certificate an invite link pinned. When set,
    /// the server is accepted only if its certificate matches — in every build,
    /// including Debug, and whether or not a CA vouches for it.
    /// </summary>
    public string? ExpectedCertificateFingerprint { get; set; }

    /// <summary>The certificate SHA-256 the server presented on this connection.</summary>
    public string? ServerCertificateFingerprint { get; private set; }

    public uint UserId => _userId;
    public string? SessionToken => _sessionToken;
    public bool IsConnected => _connection != null;

    /// <summary>Whether the login response granted admin (channel management).</summary>
    public bool IsAdmin { get; private set; }

    /// <summary>
    /// Our own stable user id — the same value our MLS credential carries, so
    /// it matches what <see cref="Mls"/> reports for our own leaf.
    /// </summary>
    public string? UserUuid => _userUuid;

    /// <summary>
    /// The MLS client, exposed for reading group membership during identity
    /// verification. Member keys must come from here — the authenticated
    /// ratchet tree — and never from the server's user list.
    /// </summary>
    internal MlsWrapper? Mls => _mlsWrapper;

    /// <summary>
    /// The channel's current MLS text-group epoch, for the `E2EE · epoch N`
    /// badge. Null when there is no group yet (not joined, or MLS unavailable).
    /// </summary>
    public ulong? CurrentTextEpoch(string channelId)
    {
        if (_mlsWrapper == null || string.IsNullOrEmpty(channelId)) return null;
        try
        {
            return _mlsWrapper.CurrentEpoch(channelId, isVoice: false);
        }
        catch
        {
            // No group for this channel yet — the badge simply stays hidden.
            return null;
        }
    }

    public event Action<string>? OnStatusChanged;
    public event Action<string>? OnError;
    public event Action<uint, bool, bool>? OnUserStatusUpdated; // sessionId, isMuted, isDeafened

    /// <summary>
    /// The control stream ended or failed while we still meant to be connected.
    /// Never raised for a disconnect we asked for. Carries a short reason.
    /// </summary>
    public event Action<string>? OnConnectionLost;

    /// <summary>Someone (possibly us) changed their profile.</summary>
    public event Action<Aura.V1Alpha1.UserProfile>? OnProfileUpdated;

    /// <summary>
    /// Result of an admin/profile request: action ("create channel", …),
    /// success, and the server's error text on failure.
    /// </summary>
    public event Action<string, bool, string>? OnRequestResult;
    
    /// <summary>
    /// Run an app-level operation inside an opt-in telemetry span (aura-core
    /// <c>TelemetrySpan</c>). The span carries only the operation and its
    /// outcome, and does nothing unless telemetry was started.
    /// </summary>
    private static async Task TracedAsync(ClientOperation operation, Func<Task> body)
    {
        using var span = new TelemetrySpan(operation);
        try
        {
            await body();
            span.Finish(OperationOutcome.Ok);
        }
        catch (OperationCanceledException)
        {
            span.Finish(OperationOutcome.Cancelled);
            throw;
        }
        catch
        {
            span.Finish(OperationOutcome.Failed);
            throw;
        }
    }

    /// <summary>
    /// Connect to the Aura server via QUIC.
    /// </summary>
    public Task ConnectAsync(string host, int port = 8443, CancellationToken ct = default)
        => TracedAsync(ClientOperation.Connect, () => ConnectCoreAsync(host, port, ct));

    private async Task ConnectCoreAsync(string host, int port, CancellationToken ct)
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
                RemoteCertificateValidationCallback = (sender, cert, chain, errors) => 
                {
                    if (cert != null)
                    {
                        using var leaf = new X509Certificate2(cert);
                        ServerCertificateFingerprint = Convert.ToHexString(
                            System.Security.Cryptography.SHA256.HashData(leaf.RawData)).ToLowerInvariant();
                    }

                    // An invite's pin is checked before anything else and in
                    // every build: it is the whole point of carrying one.
                    if (ExpectedCertificateFingerprint is { } expected)
                    {
                        if (ServerCertificateFingerprint != null &&
                            ServerCertificateFingerprint.Equals(expected, StringComparison.OrdinalIgnoreCase))
                        {
                            return true;
                        }
                        throw new CertificateMismatchException(host, ServerCertificateFingerprint ?? "");
                    }
#if DEBUG
                    Console.WriteLine($"[AuraClient] DEBUG: TLS cert validation: errors={errors} (Accepting all)");
                    return true;
#else
                    if (errors == SslPolicyErrors.None)
                    {
                        return true;
                    }

                    if (cert == null) return false;

                    // Calculate SHA256 fingerprint
                    using var cert2 = new X509Certificate2(cert);
                    var fingerprint = cert2.GetCertHashString(System.Security.Cryptography.HashAlgorithmName.SHA256);
                    
                    Console.WriteLine($"[AuraClient] TLS cert validation failure: errors={errors}");
                    Console.WriteLine($"[AuraClient] Fingerprint (SHA256): {fingerprint}");

                    // Check if trusted via TOFU
                    if (KnownServers.IsTrusted(host, fingerprint))
                    {
                        Console.WriteLine("[AuraClient] Fingerprint matches known trusted list. Proceeding.");
                        return true;
                    }

                    // Not trusted - we throw an exception that the UI can catch
                    // Note: This callback is inside QuicConnection.ConnectAsync
                    throw new UntrustedCertificateException(host, fingerprint, cert);
#endif
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
    /// Log in with the hybrid identity (login v2).
    ///
    /// The server opens with <c>[0x02][32-byte challenge]</c>. We answer with the
    /// frame aura-core builds and signs (<c>[0x03][u32 LE length][AuthRequest]</c>)
    /// and read back <c>[0x04][u32 LE length][AuthResponse]</c>, which has the
    /// same shape on success and failure.
    /// </summary>
    public Task AuthenticateAsync(UserIdentity identity, string? serverPassword = null, CancellationToken ct = default)
        => TracedAsync(ClientOperation.Login, () => AuthenticateCoreAsync(identity, serverPassword, ct));

    private async Task AuthenticateCoreAsync(UserIdentity identity, string? serverPassword, CancellationToken ct)
    {
        if (_controlStream == null)
            throw new InvalidOperationException("Not connected");
        
        Console.WriteLine($"[AuraClient] Authenticating as '{identity.DisplayName}'...");
        OnStatusChanged?.Invoke("Authenticating...");
        
        // 1. Receive challenge (server-first: it is sent as soon as the stream opens)
        Console.WriteLine("[AuraClient] Waiting for challenge (ServerHello)...");
        var challenge = await ReceiveChallengeResponseAsync(ct);
        Console.WriteLine($"[AuraClient] Received challenge: {Convert.ToHexString(challenge[..8])}...");
        
        // 2. Send the signed login frame
        var request = identity.BuildAuthRequest(
            challenge, string.IsNullOrEmpty(serverPassword) ? null : serverPassword);
        Console.WriteLine($"[AuraClient] Sending auth request ({request.Length} bytes)...");
        // Never cancel a QUIC write: a cancelled pending write aborts the
        // stream's write side for good.
        await _controlStream.WriteAsync(request, CancellationToken.None);
        
        // 3. Receive auth response
        Console.WriteLine("[AuraClient] Waiting for auth response...");
        var response = await ReceiveAuthResponseAsync(ct);
        Console.WriteLine($"[AuraClient] Auth response: success={response.Success}, sessionId={response.SessionId}, " +
                          $"userUuid={response.UserUuid}, isAdmin={response.IsAdmin}, error={response.ErrorMessage}");
        
        if (!response.Success)
        {
            throw new AuthenticationException(
                string.IsNullOrEmpty(response.ErrorMessage) ? "Authentication failed" : response.ErrorMessage);
        }
        if (string.IsNullOrEmpty(response.UserUuid))
        {
            throw new ProtocolException("Auth response has no user id");
        }
        
        _userId = response.SessionId;
        _sessionToken = response.SessionToken;
        IsAdmin = response.IsAdmin;
        // The server-assigned account id (U_…). It is also our MLS credential
        // identity, so it matches what Mls reports for our own leaf.
        _userUuid = response.UserUuid;
        
        // Initialize MLS wrapper for E2EE
        try
        {
            _mlsWrapper = identity.CreateMlsWrapper(_userUuid);
            Console.WriteLine("[AuraClient] MLS wrapper initialized for E2EE");
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to initialize MLS: {ex.Message} - E2EE will not be available");
        }
        
        OnStatusChanged?.Invoke($"Authenticated as user {response.SessionId}" + (response.Verified ? " (verified)" : ""));

        // Create the audio sender/receiver now that we have a session id. The
        // sender stays silent until joining a channel installs a key exported
        // from that channel's MLS voice group (UpdateAudioKeysFromMls).
        try
        {
            _audioManager?.Initialize(_userId, BootstrapAudioKey);
        }
        catch (Exception ex)
        {
            OnError?.Invoke($"Audio unavailable: {ex.Message}");
        }

        // Start listening for server messages (presence, chat, etc.)
        StartListening();
        StartKeepalive();
    }

    /// <summary>
    /// Send a bare <c>0x00</c> on the control stream every
    /// <see cref="KeepaliveInterval"/>. The server reads it as a keepalive with
    /// no payload; it exists only to reset the server's idle timer.
    /// </summary>
    private void StartKeepalive()
    {
        _keepaliveCts?.Cancel();
        var cts = new CancellationTokenSource();
        _keepaliveCts = cts;
        _ = Task.Run(async () =>
        {
            try
            {
                while (!cts.IsCancellationRequested)
                {
                    await Task.Delay(KeepaliveInterval, cts.Token);
                    await WriteControlAsync(new[] { MSG_KEEPALIVE }, cts.Token);
                }
            }
            catch (OperationCanceledException)
            {
            }
            catch (Exception ex)
            {
                // A failed write means the stream is gone; the receive loop
                // notices the same thing and owns raising OnConnectionLost.
                Console.WriteLine($"[AuraClient] Keepalive stopped: {ex.Message}");
            }
        });
    }

    /// <summary>Serialised write of one complete frame to the control stream.</summary>
    private async Task WriteControlAsync(byte[] frame, CancellationToken ct)
    {
        var stream = _controlStream ?? throw new InvalidOperationException("Not connected");
        // The token only bounds the wait for our turn. The write itself is
        // never cancelled: cancelling a pending QuicStream write aborts the
        // stream's write side permanently, which would kill the session (e.g.
        // releasing push-to-talk mid-frame).
        await _writeLock.WaitAsync(ct);
        try
        {
            await stream.WriteAsync(frame, CancellationToken.None);
        }
        finally
        {
            _writeLock.Release();
        }
    }

    public async Task SendAudioFrameAsync(short[] pcmData, CancellationToken ct = default)
    {
        if (_controlStream == null || _audioManager == null) return;

        // Opus encode + DAVE encrypt. Null means "send nothing": no group key
        // yet, or the VAD gate held a silent frame. There is deliberately no
        // fallback — sending the raw PCM would put plaintext audio on the wire.
        var encodedPacket = _audioManager.ProcessCapture(pcmData);
        if (encodedPacket == null) return;

        try
        {
            await WriteControlAsync(ControlFrames.Frame(MSG_AUDIO, encodedPacket), ct);
        }
        catch (OperationCanceledException)
        {
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
    public Task JoinChannelAsync(string channelId, CancellationToken ct = default)
        => TracedAsync(ClientOperation.JoinChannel, () => JoinChannelCoreAsync(channelId, ct));

    private async Task JoinChannelCoreAsync(string channelId, CancellationToken ct)
    {
        if (_controlStream == null)
            throw new InvalidOperationException("Not authenticated");
        
        if (_currentChannelId == channelId)
        {
            Console.WriteLine($"[AuraClient] Already in channel {channelId}, skipping re-join");
            return;
        }

        Console.WriteLine($"[AuraClient] Joining channel {channelId}...");
        
        // Nothing goes to the new room under the old room's key; sending resumes
        // once this channel's voice group hands us a key.
        _audioManager?.ClearGroupKey();

        var req = new JoinChannelRequest { ChannelId = channelId };
        await SendProtoRequestAsync(MSG_JOIN_CHANNEL, req, ct);

        // Leaving a channel leaves its groups: drop our copy of their state and
        // keys now. The server has the remaining founder remove our leaf.
        if (!string.IsNullOrEmpty(_currentChannelId))
            ForgetChannelGroups(_currentChannelId);

        _currentChannelId = channelId;
        OnStatusChanged?.Invoke($"Joined channel {channelId}");
        
        // Reset readiness trackers for this channel
        _textGroupReady.TryRemove(channelId, out _);
        _voiceGroupReady.TryRemove(channelId, out _);
        
        // Send MLS join for E2EE
        await SendMlsJoinAsync(channelId, isVoice: true, ct);
        await SendMlsJoinAsync(channelId, isVoice: false, ct);
    }
    
    /// <summary>
    /// Send MLS join with key package when joining a channel.
    /// </summary>
    private async Task SendMlsJoinAsync(string channelId, bool isVoice, CancellationToken ct = default)
    {
        if (_controlStream == null || _mlsWrapper == null)
        {
            Console.WriteLine("[AuraClient] MLS not initialized, cannot join with E2EE");
            return;
        }
        
        try
        {
            var keyPackage = _mlsWrapper.CreateKeyPackage();
            
            var envelope = new MlsEnvelope {
                SenderId = _userId,
                ChannelId = channelId,
                GroupType = isVoice ? Aura.V1Alpha1.MlsGroupType.Voice : Aura.V1Alpha1.MlsGroupType.Text,
                KeyPackage = Google.Protobuf.ByteString.CopyFrom(keyPackage)
            };
            
            await SendProtoRequestAsync(MSG_MLS_JOIN, envelope, ct);
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
        
        var update = new Aura.V1Alpha1.UserStatusUpdate
        {
            SessionId = _userId,
            IsMuted = isMuted,
            IsDeafened = isDeafened
        };
        
        try
        {
            await SendProtoRequestAsync(MSG_UPDATE_STATUS, update, ct);
            Console.WriteLine($"[AuraClient] Sent status update: muted={isMuted}, deafened={isDeafened}");
        }
        catch (Exception ex)
        {
            OnError?.Invoke($"Status update send error: {ex.Message}");
        }
    }

    // ========================================================================
    // Admin + profile requests. The server answers each with a response frame
    // (0x40–0x43, see the receive loop) and, on success, a fresh snapshot or a
    // ProfileUpdated broadcast — so callers don't patch local state themselves.
    // ========================================================================

    private static ChannelIcon? IconFor(string? emoji) =>
        string.IsNullOrWhiteSpace(emoji) ? null : new ChannelIcon { Emoji = emoji.Trim() };

    /// <summary>Create a channel (admin only). Mirrors macOS createChannel.</summary>
    public Task CreateChannelAsync(string name, string comment, string? emoji, CancellationToken ct = default)
    {
        var req = new CreateChannelRequest { Name = name, Comment = comment };
        if (IconFor(emoji) is { } icon) req.Icon = icon;
        return SendProtoRequestAsync(MSG_CREATE_CHANNEL, req, ct);
    }

    /// <summary>Rename / re-describe a channel (admin only). Mirrors macOS updateChannel.</summary>
    public Task UpdateChannelAsync(string channelId, string name, string comment, string? emoji, CancellationToken ct = default)
    {
        var req = new UpdateChannelRequest { ChannelId = channelId, Name = name, Comment = comment };
        if (IconFor(emoji) is { } icon) req.Icon = icon;
        return SendProtoRequestAsync(MSG_UPDATE_CHANNEL, req, ct);
    }

    /// <summary>Delete a channel (admin only). Occupants are moved by the server.</summary>
    public Task DeleteChannelAsync(string channelId, CancellationToken ct = default) =>
        SendProtoRequestAsync(MSG_DELETE_CHANNEL, new DeleteChannelRequest { ChannelId = channelId }, ct);

    /// <summary>
    /// Delete our own account on this server ("Leave server"). The server
    /// answers with an AdminResponse framed as 0x44 and drops our session.
    /// </summary>
    public Task DeleteOwnAccountAsync(CancellationToken ct = default)
    {
        var uuid = _userUuid ?? throw new InvalidOperationException("Not authenticated");
        return WriteControlAsync(ControlFrames.Frame(MSG_DELETE_USER, AuraCoreMethods.EncodeDeleteUserRequest(uuid)), ct);
    }

    /// <summary>
    /// Update our own profile comment. The server only accepts a profile whose
    /// <c>user_id</c> is our account UUID, and broadcasts it back to everyone
    /// (us included) as ProfileUpdated. Unsigned, like the macOS client today.
    /// </summary>
    public Task UpdateProfileAsync(string displayName, string bio, byte[]? avatar, CancellationToken ct = default)
    {
        var profile = new Aura.V1Alpha1.UserProfile
        {
            UserId = _userUuid ?? throw new InvalidOperationException("Not authenticated"),
            DisplayName = displayName,
            Bio = bio,
            AvatarData = ByteString.CopyFrom(avatar ?? Array.Empty<byte>()),
        };
        return SendProtoRequestAsync(MSG_UPDATE_PROFILE, new UpdateProfile { Profile = profile }, ct);
    }

    // ========================================================================
    // Login framing
    // ========================================================================
    
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
    
    /// <summary>
    /// Read <c>[0x04][u32 LE length][AuthResponse]</c> and decode it.
    /// </summary>
    private async Task<AuthResponseRecord> ReceiveAuthResponseAsync(CancellationToken ct)
    {
        var typeBuf = new byte[1];
        await ReadExactAsync(typeBuf, ct);
        
        if (typeBuf[0] != MSG_AUTH_RESPONSE)
        {
            throw new ProtocolException($"Invalid auth response: type=0x{typeBuf[0]:X2}");
        }
        
        var payload = await ReadHardenedPayloadAsync(MaxAuthResponseSize, ct);
        return AuraCoreMethods.DecodeAuthResponse(payload);
    }

    public async ValueTask DisposeAsync()
    {
        // Mark first: tearing the stream down makes the receive loop fail, and
        // that failure must not be reported as an unexpected connection loss.
        _disposed = true;
        _keepaliveCts?.Cancel();
        _listenCts?.Cancel();

        try
        {
            if (_controlStream != null)
            {
                await _controlStream.DisposeAsync();
            }

            if (_connection != null)
            {
                await _connection.DisposeAsync();
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Error during disconnect: {ex.Message}");
        }
        finally
        {
            _controlStream = null;
            _connection = null;
            _audioManager?.ClearGroupKey();
        }
    }
    // ========================================================================
    // Receive Loop & State Handlers
    // ========================================================================

    private const int MaxAudioPacketSize = 65536;
    private const int MaxControlPacketSize = 2 * 1024 * 1024;

    /// <summary>An AuthResponse is a few short strings and flags.</summary>
    private const int MaxAuthResponseSize = 64 * 1024;

    public event Action<string, uint, string>? OnUserJoined; // channelId, sessionId, name
    public event Action<string, uint>? OnUserLeft;           // channelId, sessionId
    public event Action<ServerState>? OnServerSnapshot;
    public event Action<string, string>? OnChannelDeleted; // deletedChannelId, fallbackChannelId

    private CancellationTokenSource? _listenCts;

    /// <summary>
    /// Session IDs we've seen in each channel, used to warm retained text keys.
    /// </summary>
    private readonly ConcurrentDictionary<string, ConcurrentDictionary<uint, byte>> _channelMembers = new();

    /// <summary>
    /// Session id → stable user UUID. Session ids are reallocated every
    /// connection, so local-only preferences (volume, mute) have to be keyed on
    /// the UUID or they reset whenever someone reconnects.
    /// </summary>
    private readonly ConcurrentDictionary<uint, string> _sessionToUuid = new();

    /// <summary>Local playback gain per user UUID, loaded from settings.</summary>
    private readonly ConcurrentDictionary<string, float> _userVolumes = new();

    /// <summary>Locally-muted user UUIDs, loaded from settings.</summary>
    private readonly ConcurrentDictionary<string, bool> _locallyMuted = new();

    /// <summary>Raised when a preference changes and should be persisted.</summary>
    public event Action? OnLocalMixerPrefsChanged;

    /// <summary>Seed the local mixer preferences from persisted settings.</summary>
    public void LoadLocalMixerPrefs(Dictionary<string, float> volumes, List<string> muted)
    {
        foreach (var (uuid, gain) in volumes) _userVolumes[uuid] = gain;
        foreach (var uuid in muted) _locallyMuted[uuid] = true;
    }

    /// <summary>Current preferences, for persisting.</summary>
    public (Dictionary<string, float> Volumes, List<string> Muted) LocalMixerPrefs() =>
        (new Dictionary<string, float>(_userVolumes),
         _locallyMuted.Where(kv => kv.Value).Select(kv => kv.Key).ToList());

    /// <summary>Stable UUID for a session, or null if not yet known.</summary>
    public string? UuidForSession(uint sessionId) =>
        _sessionToUuid.TryGetValue(sessionId, out var uuid) ? uuid : null;

    /// <summary>This user's local playback gain, 0.0–4.0 (1.0 = unchanged).</summary>
    public float LocalVolumeForSession(uint sessionId)
    {
        var uuid = UuidForSession(sessionId);
        if (uuid != null && _userVolumes.TryGetValue(uuid, out var gain)) return gain;
        return 1.0f;
    }

    public bool IsLocallyMuted(uint sessionId)
    {
        var uuid = UuidForSession(sessionId);
        return uuid != null && _locallyMuted.TryGetValue(uuid, out var m) && m;
    }

    /// <summary>
    /// Set one user's local playback gain. Keyed by UUID so it survives their
    /// reconnect, and applied to the live mixer immediately.
    /// </summary>
    public void SetLocalVolume(uint sessionId, float gain)
    {
        var clamped = Math.Clamp(gain, 0.0f, 4.0f);
        _audioManager?.SetSenderGain(sessionId, clamped);

        if (UuidForSession(sessionId) is { } uuid)
        {
            _userVolumes[uuid] = clamped;
            OnLocalMixerPrefsChanged?.Invoke();
        }
    }

    /// <summary>Toggle local-only mute for a user. Returns the new state.</summary>
    public bool ToggleLocalMute(uint sessionId)
    {
        var muted = !IsLocallyMuted(sessionId);
        _audioManager?.SetSenderMuted(sessionId, muted);

        if (UuidForSession(sessionId) is { } uuid)
        {
            _locallyMuted[uuid] = muted;
            OnLocalMixerPrefsChanged?.Invoke();
        }
        return muted;
    }

    /// <summary>
    /// Learn a session → UUID mapping and immediately honour any preference
    /// already stored for that user. Called on UserJoined and from the server
    /// snapshot, mirroring the macOS client.
    /// </summary>
    private void RegisterSessionIdentity(uint sessionId, string userUuid)
    {
        if (string.IsNullOrEmpty(userUuid)) return;
        _sessionToUuid[sessionId] = userUuid;
        ApplyLocalMixerPrefs(sessionId);
    }

    /// <summary>
    /// Push stored volume/mute into the Rust mixer for a session. Must run after
    /// every AddSender — re-joining a channel or a key rotation rebuilds the
    /// sender state and would otherwise drop the preference silently.
    /// </summary>
    private void ApplyLocalMixerPrefs(uint sessionId)
    {
        if (_audioManager == null) return;
        var uuid = UuidForSession(sessionId);
        if (uuid == null) return;

        if (_userVolumes.TryGetValue(uuid, out var gain))
            _audioManager.SetSenderGain(sessionId, gain);
        if (_locallyMuted.TryGetValue(uuid, out var muted) && muted)
            _audioManager.SetSenderMuted(sessionId, true);
    }

    /// <summary>
    /// Warm a retained text key for one sender at the group's current epoch.
    /// Deriving the key is what makes the core retain it across the next epoch
    /// change, which is what keeps a member's first message decryptable when
    /// their own join bumps the epoch.
    /// </summary>
    private void RegisterTextSender(string channelId, uint sessionId)
    {
        if (_mlsWrapper == null || !_mlsWrapper.IsMember(channelId, isVoice: false)) return;
        try
        {
            _mlsWrapper.RegisterTextSender(channelId, sessionId);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to register text sender {sessionId}: {ex.Message}");
        }
    }

    /// <summary>
    /// Warm retained text keys for ourselves and every member we know about in
    /// the channel. Called whenever the text group's epoch changes.
    /// </summary>
    private void RefreshTextSenderKeys(string channelId)
    {
        if (_mlsWrapper == null || !_mlsWrapper.IsMember(channelId, isVoice: false)) return;

        RegisterTextSender(channelId, _userId);
        if (_channelMembers.TryGetValue(channelId, out var members))
        {
            foreach (var sessionId in members.Keys)
            {
                RegisterTextSender(channelId, sessionId);
            }
        }
    }

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

                    case MSG_CHANNEL_DELETED: // 0x47 - The channel we were in was deleted
                        await HandleChannelDeletedAsync(ct);
                        break;

                    case MSG_MLS_RATCHET_REQUEST: // 0x56 - Server asks us to advance the epoch
                        await HandleMlsRatchetRequestAsync(ct);
                        break;

                    case MSG_MLS_SYNC_MEMBERS: // 0x58 - Remove members who left the channel
                        await HandleMlsSyncMembersAsync(ct);
                        break;

                    case MSG_PROFILE_UPDATED: // 0x46 - Someone (maybe us) edited their profile
                        await HandleProfileUpdatedAsync(ct);
                        break;

                    case MSG_CREATE_CHANNEL: // 0x40 - CreateChannelResponse
                    case MSG_UPDATE_CHANNEL: // 0x41 - MetadataUpdateResponse
                    case MSG_UPDATE_PROFILE: // 0x42 - MetadataUpdateResponse
                    case MSG_DELETE_CHANNEL: // 0x43 - AdminResponse
                    case MSG_DELETE_USER:    // 0x44 - AdminResponse
                        await HandleRequestResultAsync(msgType, ct);
                        break;

                    default:
                        // Every server frame is length-prefixed, so an unknown
                        // type is skipped whole. Reading on from the length
                        // bytes would desync the stream for the whole session.
                        var skipped = await ControlFrames.DiscardPayloadAsync(_controlStream, MaxControlPacketSize, ct);
                        Console.WriteLine($"[AuraClient] Skipped unknown message type 0x{msgType:X2} ({skipped} bytes)");
                        break;
                }
            }

            if (!ct.IsCancellationRequested && !_disposed)
            {
                OnConnectionLost?.Invoke("The server closed the connection");
            }
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            if (!_disposed)
            {
                OnConnectionLost?.Invoke(ex is EndOfStreamException ? "The server closed the connection" : ex.Message);
            }
        }
    }

    private async Task HandleProfileUpdatedAsync(CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);
        try
        {
            OnProfileUpdated?.Invoke(Aura.V1Alpha1.UserProfile.Parser.ParseFrom(packet));
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to parse ProfileUpdated: {ex.Message}");
        }
    }

    private async Task HandleRequestResultAsync(byte msgType, CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);
        try
        {
            var (action, success, error) = msgType switch
            {
                MSG_CREATE_CHANNEL => Result("create channel", CreateChannelResponse.Parser.ParseFrom(packet)),
                MSG_UPDATE_CHANNEL => Result("update channel", MetadataUpdateResponse.Parser.ParseFrom(packet)),
                MSG_UPDATE_PROFILE => Result("update profile", MetadataUpdateResponse.Parser.ParseFrom(packet)),
                MSG_DELETE_USER => Result("delete user", AdminResponse.Parser.ParseFrom(packet)),
                _ => Result("delete channel", AdminResponse.Parser.ParseFrom(packet)),
            };
            OnRequestResult?.Invoke(action, success, error);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to parse response 0x{msgType:X2}: {ex.Message}");
        }

        static (string, bool, string) Result(string action, IMessage message) => message switch
        {
            CreateChannelResponse r => (action, r.Success, r.ErrorMessage),
            MetadataUpdateResponse r => (action, r.Success, r.ErrorMessage),
            AdminResponse r => (action, r.Success, r.ErrorMessage),
            _ => (action, false, "unexpected response"),
        };
    }

    private async Task HandleAudioPacketAsync(CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxAudioPacketSize, ct);
        
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
            if (packet.Length > 32)
            {
                var payload = packet.AsSpan(32).ToArray();
                _audioEngine?.PlayAudio(payload);
            }
        }
    }

    private async Task HandleUserJoinedAsync(CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);
        
        var join = UserJoined.Parser.ParseFrom(packet);
        string channelId = join.ChannelId;
        uint sessionId = join.SessionId;
        string name = join.DisplayName;

        Console.WriteLine($"[AuraClient] UserJoined: {name} (ID: {sessionId}) in Channel {channelId}");

        _channelMembers.GetOrAdd(channelId, _ => new ConcurrentDictionary<uint, byte>())[sessionId] = 0;

        // Learn session → UUID before touching the mixer, so any stored volume
        // or mute for this user is applied as soon as their sender exists.
        RegisterSessionIdentity(sessionId, join.UserUuid);

        // Warm a text key for them before their join advances the epoch.
        RegisterTextSender(channelId, sessionId);

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
                    // AddSender rebuilds the sender's state, so prefs have to be
                    // re-pushed or they are silently lost on rejoin/key rotation.
                    ApplyLocalMixerPrefs(sessionId);
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
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);
        
        var left = UserLeft.Parser.ParseFrom(packet);
        string channelId = left.ChannelId;
        uint sessionId = left.SessionId;
        
        Console.WriteLine($"[AuraClient] UserLeft: ID {sessionId} from Channel {channelId}");

        if (_channelMembers.TryGetValue(channelId, out var members))
        {
            members.TryRemove(sessionId, out _);
        }

        // Drop the session mapping. The UUID-keyed preferences stay, so they
        // re-apply on this user's next reconnect.
        _sessionToUuid.TryRemove(sessionId, out _);

        // Remove remote sender from audio decryption
        _audioManager?.RemoveRemoteSender(sessionId);
        
        OnUserLeft?.Invoke(channelId, sessionId);
    }

    /// <summary>
    /// Handle notification that the channel we were in has been deleted. The
    /// server has already torn down the MLS groups and told everyone we left,
    /// so we just drop local state and let the ViewModel move us elsewhere.
    /// </summary>
    private async Task HandleChannelDeletedAsync(CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);

        try
        {
            var notice = Aura.V1Alpha1.ChannelDeleted.Parser.ParseFrom(packet);
            Console.WriteLine($"[AuraClient] Channel {notice.ChannelId} was deleted");

            _channelMembers.TryRemove(notice.ChannelId, out _);
            _textGroupReady.TryRemove(notice.ChannelId, out _);
            _voiceGroupReady.TryRemove(notice.ChannelId, out _);
            ForgetChannelGroups(notice.ChannelId);

            OnChannelDeleted?.Invoke(notice.ChannelId, notice.FallbackChannelId);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to parse ChannelDeleted: {ex.Message}");
        }
    }

    private async Task HandleChannelStateAsync(CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);

        try
        {
            // 3. Parse Protobuf ServerState
            var snapshot = ServerState.Parser.ParseFrom(packet);
            Console.WriteLine($"[AuraClient] ServerSnapshot: {snapshot.Channels.Count} channels, {snapshot.Profiles.Count} profiles");

            // Learn the full roster so text keys can be warmed for members who
            // were already present before we connected, and so local-only
            // volume/mute prefs attach to stable identities.
            foreach (var channel in snapshot.Channels)
            {
                var members = _channelMembers.GetOrAdd(channel.ChannelId, _ => new ConcurrentDictionary<uint, byte>());
                foreach (var user in channel.Users)
                {
                    members[user.SessionId] = 0;
                    RegisterSessionIdentity(user.SessionId, user.UserUuid);
                }
                RefreshTextSenderKeys(channel.ChannelId);
            }

            OnServerSnapshot?.Invoke(snapshot);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to parse ServerSnapshot: {ex.Message}");
        }
    }

    private async Task HandleUserStatusUpdateAsync(CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);

        try
        {
            // 3. Parse Protobuf UserStatusUpdate
            var update = Aura.V1Alpha1.UserStatusUpdate.Parser.ParseFrom(packet);
            Console.WriteLine($"[AuraClient] UserStatusUpdate: User {update.SessionId}, Muted={update.IsMuted}, Deafened={update.IsDeafened}");

            OnUserStatusUpdated?.Invoke(update.SessionId, update.IsMuted, update.IsDeafened);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to parse UserStatusUpdate: {ex.Message}");
        }
    }

    public async Task SendTextMessageAsync(string channelId, string content, string messageId, string? replyToId = null, CancellationToken ct = default)
    {
        if (_controlStream == null || _mlsWrapper == null)
            throw new InvalidOperationException("Client not fully initialized");
        
        try
        {
            // Wait for text group to be ready (timeout after 5s)
            await WaitMlsGroupAsync(channelId, isVoice: false, 5000, ct);

            var msg = new TextMessage {
                SenderUuid = _userUuid ?? "",
                Timestamp = (ulong)DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
                Content = content,
                MessageId = messageId,
                ReplyToId = replyToId ?? "",
                Type = MediaType.Text
            };
            
            // null epoch = derive at the group's current epoch, which is what
            // gets stamped on the outgoing packet below.
            var key = _mlsWrapper.ExportTextKey(channelId, _userId, null);
            var epoch = _mlsWrapper.CurrentEpoch(channelId, isVoice: false);
            
            // Native Encrypt takes TextMessageRecord and returns EncryptedTextPacketRecord
            var textMsg = new uniffi.aura_core.TextMessageRecord(
                _userUuid ?? "", 
                (ulong)DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(), 
                content, 
                replyToId ?? "", 
                messageId, 
                0, // MediaType.Text
                0, // fileSize
                "" // sha256Hash
            );

            // Use a transient wrapper with the actual MLS-derived key for this session/epoch
            using var transientCrypto = new uniffi.aura_core.TextCryptoWrapper(key);
            var nativePacket = transientCrypto.Encrypt(epoch, channelId, _userId, textMsg);

            var packet = new EncryptedTextPacket {
                SenderSessionId = _userId,
                ChannelId = channelId,
                Epoch = epoch,
                MessageId = messageId,
                Ciphertext = ByteString.CopyFrom(nativePacket.Ciphertext),
                Nonce = ByteString.CopyFrom(nativePacket.Nonce),
                Tag = ByteString.CopyFrom(nativePacket.Tag),
                ReplyToId = replyToId ?? ""
            };
            
            await SendProtoRequestAsync(MSG_TEXT_PACKET, packet, ct);
        }
        catch (TimeoutException)
        {
            // The caller shows this next to the message, so say what it means.
            throw new InvalidOperationException("the channel's encryption isn't ready yet");
        }
    }

    public event Action<string, uint, string, string, string?>? OnTextMessage; // msgId, senderId, channelId, content, replyToId

    private async Task HandleTextPacketAsync(CancellationToken ct)
    {
        var packetBuf = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);
        
        try
        {
            var packet = EncryptedTextPacket.Parser.ParseFrom(packetBuf);
            string channelId = packet.ChannelId;
            
            if (_mlsWrapper != null && _mlsWrapper.IsMember(channelId, isVoice: false))
            {
                // 1. Export the key for the epoch this message was encrypted
                //    under. Deriving at the group's *current* epoch instead
                //    would silently fail for anything in flight across a
                //    membership change.
                byte[] senderKey;
                try
                {
                    senderKey = _mlsWrapper.ExportTextKey(channelId, packet.SenderSessionId, packet.Epoch);
                }
                catch (MlsException.EpochUnavailable ex)
                {
                    // Key material for that epoch has aged out; the message is
                    // unrecoverable rather than merely delayed.
                    Console.WriteLine($"[AuraClient] Dropping message {packet.MessageId}: epoch {ex.epoch} no longer retained (now at {ex.current})");
                    return;
                }

                // 2. Build the native record
                var nativePacket = new uniffi.aura_core.EncryptedTextPacketRecord(
                    packet.SenderSessionId,
                    packet.ChannelId,
                    packet.Epoch,
                    packet.MessageId,
                    packet.Ciphertext.ToByteArray(),
                    packet.Nonce.ToByteArray(),
                    packet.Tag.ToByteArray(),
                    packet.ReplyToId
                );

                // 3. Decrypt using a transient wrapper with the sender-specific key
                using var transientCrypto = new uniffi.aura_core.TextCryptoWrapper(senderKey);
                var decryptedMessage = transientCrypto.Decrypt(nativePacket);
                
                // Only the reply reference inside the ciphertext counts. The
                // packet's plaintext copy is server-controlled, so falling back
                // to it would let a server forge reply quotes.
                var replyTo = decryptedMessage.ReplyToId;
                OnTextMessage?.Invoke(packet.MessageId, packet.SenderSessionId, channelId, decryptedMessage.Content,
                    string.IsNullOrEmpty(replyTo) ? null : replyTo);
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to handle text packet: {ex.Message}");
        }
    }

    private Task<byte[]> ReadHardenedPayloadAsync(int maxLen, CancellationToken ct) =>
        ControlFrames.ReadPayloadAsync(_controlStream ?? throw new EndOfStreamException(), maxLen, ct);

    private Task ReadExactAsync(byte[] buf, CancellationToken ct) =>
        ControlFrames.ReadExactAsync(_controlStream ?? throw new EndOfStreamException(), buf, ct);
    
    // ========================================================================
    // MLS Protocol Handlers
    // ========================================================================
    
    private async Task WaitMlsGroupAsync(string channelId, bool isVoice, int timeoutMs, CancellationToken ct)
    {
        var trackers = isVoice ? _voiceGroupReady : _textGroupReady;
        var tcs = trackers.GetOrAdd(channelId, _ => new TaskCompletionSource());
        
        // Check if already ready
        if (tcs.Task.IsCompleted) return;

        // Wait with timeout
        var timeoutTask = Task.Delay(timeoutMs, ct);
        var completedTask = await Task.WhenAny(tcs.Task, timeoutTask);

        if (completedTask == timeoutTask)
        {
            throw new TimeoutException($"Timed out waiting for MLS {(isVoice ? "voice" : "text")} group in channel {channelId}");
        }
    }

    /// <summary>
    /// Handle server telling us to create a new MLS group (we're the first joiner).
    /// </summary>
    private async Task HandleMlsCreateGroupAsync(CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);
        
        var envelope = MlsEnvelope.Parser.ParseFrom(packet);
        string channelId = envelope.ChannelId;
        bool isVoice = envelope.GroupType == Aura.V1Alpha1.MlsGroupType.Voice;

        if (_mlsWrapper == null)
        {
            Console.WriteLine("[AuraClient] MLS not initialized");
            return;
        }
        
        try
        {
            _mlsWrapper.CreateGroup(channelId, isVoice);
            Console.WriteLine($"[AuraClient] Created MLS {(isVoice ? "voice" : "text")} group for channel {channelId}");
            
            // Mark as ready
            var trackers = isVoice ? _voiceGroupReady : _textGroupReady;
            trackers.GetOrAdd(channelId, _ => new TaskCompletionSource()).TrySetResult();

            // Update audio keys if we're the founder of a voice group
            if (isVoice)
            {
                UpdateAudioKeysFromMls(channelId);
            }
            else
            {
                RefreshTextSenderKeys(channelId);
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
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);
        
        var envelope = MlsEnvelope.Parser.ParseFrom(packet);
        string channelId = envelope.ChannelId;
        bool isVoice = envelope.GroupType == Aura.V1Alpha1.MlsGroupType.Voice;
        uint joinerSessionId = envelope.TargetSessionId;
        byte[] keyPackage = envelope.KeyPackage.ToByteArray();
        
        if (_mlsWrapper == null || _controlStream == null)
        {
            Console.WriteLine("[AuraClient] MLS not initialized");
            return;
        }
        
        try
        {
            // Add the member - returns MlsCommitWelcome record
            var result = _mlsWrapper.AddMember(channelId, isVoice, keyPackage);
            Console.WriteLine($"[AuraClient] Added member {joinerSessionId} to MLS group, sending commit/welcome");
            
            // Send commit + welcome back to server
            var envelopeOut = new MlsEnvelope {
                SenderId = _userId,
                ChannelId = channelId,
                GroupType = isVoice ? Aura.V1Alpha1.MlsGroupType.Voice : Aura.V1Alpha1.MlsGroupType.Text,
                CommitWelcome = new Aura.V1Alpha1.MlsCommitWelcome {
                    Commit = ByteString.CopyFrom(result.Commit),
                    Welcome = ByteString.CopyFrom(result.Welcome),
                    NewMemberSessionId = joinerSessionId
                }
            };
            
            await SendProtoRequestAsync(MSG_MLS_COMMIT_WELCOME, envelopeOut, ct);
            Console.WriteLine($"[AuraClient] Sent commit/welcome for new member {joinerSessionId}");
            
            // Update keys after epoch advance
            if (isVoice)
            {
                UpdateAudioKeysFromMls(channelId);
            }
            else
            {
                RefreshTextSenderKeys(channelId);
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
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);
        
        try
        {
            var envelope = MlsEnvelope.Parser.ParseFrom(packet);
            string channelId = envelope.ChannelId;
            bool isVoice = envelope.GroupType == Aura.V1Alpha1.MlsGroupType.Voice;
            byte[] commit = envelope.Commit.ToByteArray();
            
            if (_mlsWrapper == null) return;
            
            var newEpoch = _mlsWrapper.ProcessCommit(channelId, isVoice, commit);
            Console.WriteLine($"[AuraClient] Processed MLS commit from {envelope.SenderId}, now at epoch {newEpoch}");

            if (isVoice) UpdateAudioKeysFromMls(channelId);
            else RefreshTextSenderKeys(channelId);
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
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);
        
        try
        {
            var envelope = MlsEnvelope.Parser.ParseFrom(packet);
            string channelId = envelope.ChannelId;
            bool isVoice = envelope.GroupType == Aura.V1Alpha1.MlsGroupType.Voice;
            byte[] welcome = envelope.Welcome.ToByteArray();
            
            if (_mlsWrapper == null) return;
            
            _mlsWrapper.JoinGroup(welcome);
            Console.WriteLine($"[AuraClient] Joined MLS {(isVoice ? "voice" : "text")} group via Welcome for channel {channelId}");
            
            // Mark as ready
            var trackers = isVoice ? _voiceGroupReady : _textGroupReady;
            trackers.GetOrAdd(channelId, _ => new TaskCompletionSource()).TrySetResult();

            if (isVoice) UpdateAudioKeysFromMls(channelId);
            else RefreshTextSenderKeys(channelId);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to process MLS welcome: {ex.Message}");
        }
    }
    
    /// <summary>
    /// Handle the server asking us (as group founder) to advance the epoch.
    /// This is the batched PFS ratchet: an empty commit rotates the group
    /// secret so previous-epoch keys cannot decrypt future traffic.
    /// </summary>
    private async Task HandleMlsRatchetRequestAsync(CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);

        try
        {
            var envelope = MlsEnvelope.Parser.ParseFrom(packet);
            string channelId = envelope.ChannelId;
            bool isVoice = envelope.GroupType == Aura.V1Alpha1.MlsGroupType.Voice;

            if (_mlsWrapper == null || _controlStream == null) return;
            if (!_mlsWrapper.IsMember(channelId, isVoice))
            {
                Console.WriteLine($"[AuraClient] Ratchet requested for a group we're not in: {channelId}");
                return;
            }

            var commit = _mlsWrapper.SelfUpdate(channelId, isVoice);

            var envelopeOut = new MlsEnvelope {
                SenderId = _userId,
                ChannelId = channelId,
                GroupType = envelope.GroupType,
                Commit = ByteString.CopyFrom(commit)
            };

            await SendProtoRequestAsync(MSG_MLS_RATCHET_COMMIT, envelopeOut, ct);

            var newEpoch = _mlsWrapper.CurrentEpoch(channelId, isVoice);
            Console.WriteLine($"[AuraClient] Ratcheted {(isVoice ? "voice" : "text")} group {channelId} to epoch {newEpoch}");

            if (isVoice) UpdateAudioKeysFromMls(channelId);
            else RefreshTextSenderKeys(channelId);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to handle MLS ratchet request: {ex.Message}");
        }
    }

    /// <summary>Drop our state and keys for a channel's voice and text groups.</summary>
    private void ForgetChannelGroups(string channelId)
    {
        if (_mlsWrapper == null) return;
        try
        {
            _mlsWrapper.ForgetGroup(channelId, true);
            _mlsWrapper.ForgetGroup(channelId, false);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to forget groups for {channelId}: {ex.Message}");
        }
    }

    /// <summary>
    /// The server lists who should still be in a group we founded (someone left
    /// the channel). Commit their removal and hand the commit back on the
    /// ratchet-commit path; nothing is sent when nobody needs removing.
    /// </summary>
    private async Task HandleMlsSyncMembersAsync(CancellationToken ct)
    {
        var packet = await ReadHardenedPayloadAsync(MaxControlPacketSize, ct);

        try
        {
            var sync = Aura.V1Alpha1.MlsSyncMembers.Parser.ParseFrom(packet);
            string channelId = sync.ChannelId;
            bool isVoice = sync.GroupType == Aura.V1Alpha1.MlsGroupType.Voice;

            if (_mlsWrapper == null || _controlStream == null) return;
            if (!_mlsWrapper.IsMember(channelId, isVoice))
            {
                Console.WriteLine($"[AuraClient] Member sync for a group we're not in: {channelId}");
                return;
            }

            var commit = _mlsWrapper.RemoveMembersExcept(channelId, isVoice, sync.MemberUuids.ToArray());
            if (commit == null) return;

            var envelopeOut = new MlsEnvelope {
                SenderId = _userId,
                ChannelId = channelId,
                GroupType = sync.GroupType,
                Commit = ByteString.CopyFrom(commit)
            };

            await SendProtoRequestAsync(MSG_MLS_RATCHET_COMMIT, envelopeOut, ct);
            Console.WriteLine($"[AuraClient] Removed departed members from {(isVoice ? "voice" : "text")} group {channelId}");

            if (isVoice) UpdateAudioKeysFromMls(channelId);
            else RefreshTextSenderKeys(channelId);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to handle MLS member sync: {ex.Message}");
        }
    }

    /// <summary>Voice keys apply only to the channel we're currently in.</summary>
    public static bool ShouldApplyVoiceKeys(string currentChannelId, string channelId) =>
        !string.IsNullOrEmpty(channelId) && channelId == currentChannelId;

    /// <summary>
    /// Update audio sender/receiver keys from MLS.
    /// </summary>
    private void UpdateAudioKeysFromMls(string channelId)
    {
        if (_mlsWrapper == null) return;

        // MLS state for a channel we've left lingers (we can stay its voice
        // founder server-side and keep processing its commits). Its key must
        // never become our sending key while we're in another room.
        if (!ShouldApplyVoiceKeys(_currentChannelId, channelId))
        {
            Console.WriteLine($"[AuraClient] Ignoring voice key update for {channelId}; we're in {_currentChannelId}");
            return;
        }
        
        try
        {
            // 1. Update our own sender key
            var myKey = _mlsWrapper.ExportAudioKey(channelId, _userId);
            var epoch = _mlsWrapper.CurrentEpoch(channelId, isVoice: true);
            _audioManager?.UpdateSenderKey(myKey, epoch);
            Console.WriteLine($"[AuraClient] Rotated audio sender key from MLS, epoch={epoch}");

            // 2. Every other member's key rotates with the epoch too. Members who
            // were in the room before we joined have never been added at all —
            // UserJoined only covers people who arrive after us — so add any the
            // receiver doesn't know yet (mirrors the macOS client).
            if (_audioManager == null || !_channelMembers.TryGetValue(channelId, out var members)) return;
            var epochHint = (ushort)(epoch & 0xFFFF);
            foreach (var sessionId in members.Keys)
            {
                if (sessionId == _userId) continue;
                try
                {
                    var key = _mlsWrapper.ExportAudioKey(channelId, sessionId);
                    if (!_audioManager.UpdateRemoteSenderKey(sessionId, key, epochHint))
                    {
                        _audioManager.AddRemoteSender(sessionId, key, epochHint);
                        // AddSender rebuilds the sender's state; re-push local prefs.
                        ApplyLocalMixerPrefs(sessionId);
                    }
                }
                catch (Exception ex)
                {
                    Console.WriteLine($"[AuraClient] Failed to refresh audio key for {sessionId}: {ex.Message}");
                }
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[AuraClient] Failed to update audio keys: {ex.Message}");
        }
    }
    /// <summary>
    /// Send a Protobuf-encoded message over the control stream.
    /// Format: [Type: u8] [Length: u32] [Payload]
    /// </summary>
    private async Task SendProtoRequestAsync<T>(byte msgType, T message, CancellationToken ct) where T : IMessage<T>
    {
        if (_controlStream == null) throw new InvalidOperationException("Not connected");
        await WriteControlAsync(ControlFrames.Frame(msgType, message.ToByteArray()), ct);
    }
}
