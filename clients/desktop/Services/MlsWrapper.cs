using System;
using System.Collections.Generic;

namespace Aura.Desktop.Services;

/// <summary>
/// Mock MlsWrapper providing MLS E2EE group key management.
/// This stub can be replaced by a real UniFFI-generated binding when available.
/// </summary>
public class MlsWrapper
{
    private readonly string _identity;
    private readonly Dictionary<(uint, bool), byte[]> _groups = new();
    private readonly Dictionary<(uint, bool), ulong> _epochs = new();

    public MlsWrapper(string identityName)
    {
        _identity = identityName;
    }

    public byte[] CreateKeyPackage()
    {
        var kp = new byte[256];
        new Random().NextBytes(kp);
        return kp;
    }

    public void CreateGroup(uint channelId, bool isVoice)
    {
        var key = (channelId, isVoice);
        _groups[key] = new byte[32];
        new Random().NextBytes(_groups[key]);
        _epochs[key] = 0;
    }

    public MlsCommitWelcome AddMember(uint channelId, bool isVoice, byte[] keyPackage)
    {
        var key = (channelId, isVoice);
        _epochs[key]++;
        return new MlsCommitWelcome
        {
            Commit = new byte[] { 0x01, 0x02 },
            Welcome = new byte[] { 0x03, 0x04 }
        };
    }

    public void JoinGroup(byte[] welcomeBytes)
    {
        // Stub: accept and track group membership
    }

    public ulong ProcessCommit(uint channelId, bool isVoice, byte[] commitBytes)
    {
        var key = (channelId, isVoice);
        _epochs[key]++;
        return _epochs[key];
    }

    public bool IsMember(uint channelId, bool isVoice)
        => _groups.ContainsKey((channelId, isVoice));

    public ulong CurrentEpoch(uint channelId, bool isVoice)
        => _epochs.TryGetValue((channelId, isVoice), out var epoch) ? epoch : 0;

    public byte[] ExportAudioKey(uint channelId, uint senderSessionId)
    {
        var key = new byte[32];
        var seed = (int)(channelId * 1000 + senderSessionId);
        new Random(seed).NextBytes(key);
        return key;
    }

    public void UpdateRemoteSenderKey(uint senderSessionId, byte[] key, ushort epoch)
    {
        // Stub: would update the decryption key for a remote sender
    }

    public byte[] ExportTextKey(uint channelId, uint senderSessionId)
    {
        var key = new byte[32];
        var seed = (int)(channelId * 1000 + senderSessionId + 500000);
        new Random(seed).NextBytes(key);
        return key;
    }
}

public class MlsCommitWelcome
{
    public byte[] Commit { get; set; } = Array.Empty<byte>();
    public byte[] Welcome { get; set; } = Array.Empty<byte>();
}
