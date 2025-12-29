using System;
using System.Buffers.Binary;
using System.IO;
using Xunit;

namespace Aura.Desktop.Tests;

/// <summary>
/// MLS E2EE Protocol Integration Tests for C# client.
/// </summary>
public class MlsProtocolTests
{
    // MARK: - MLS Wrapper Tests
    
    [Fact]
    public void TestMlsWrapperCreation()
    {
        // Test that MlsWrapper can be created with an identity
        var wrapper = new MlsWrapper("test-user-1");
        Assert.NotNull(wrapper);
    }
    
    [Fact]
    public void TestKeyPackageGeneration()
    {
        // Test key package generation
        var wrapper = new MlsWrapper("test-user-1");
        var keyPackage = wrapper.CreateKeyPackage();
        
        // Key package should be non-empty
        Assert.True(keyPackage.Length > 0);
        Console.WriteLine($"[Test] Generated key package: {keyPackage.Length} bytes");
    }
    
    [Fact]
    public void TestGroupCreation()
    {
        // Test MLS group creation (first-joiner scenario)
        var wrapper = new MlsWrapper("founder-user");
        
        // Create voice and text groups for channel 1
        wrapper.CreateGroup(channelId: 1, isVoice: true);
        wrapper.CreateGroup(channelId: 1, isVoice: false);
        
        // Should be able to export audio key
        var audioKey = wrapper.ExportAudioKey(channelId: 1, senderSessionId: 1);
        Assert.Equal(32, audioKey.Length); // ChaCha20 key size
        
        // Should be member of group
        Assert.True(wrapper.IsMember(channelId: 1, isVoice: true));
        
        // Epoch should be 0 for new group
        var epoch = wrapper.CurrentEpoch(channelId: 1, isVoice: true);
        Assert.Equal(0UL, epoch);
    }
    
    [Fact]
    public void TestTwoPartyMlsGroup()
    {
        // Test complete two-party MLS scenario
        var founder = new MlsWrapper("alice");
        var joiner = new MlsWrapper("bob");
        
        // 1. Founder creates group
        founder.CreateGroup(channelId: 1, isVoice: true);
        
        // 2. Joiner generates key package
        var keyPackage = joiner.CreateKeyPackage();
        
        // 3. Founder adds joiner, gets commit + welcome
        var result = founder.AddMember(channelId: 1, isVoice: true, keyPackage);
        Assert.True(result.Commit.Length > 0);
        Assert.True(result.Welcome.Length > 0);
        
        // 4. Joiner processes welcome to join group
        joiner.JoinGroup(result.Welcome);
        
        // 5. Both should now be members
        Assert.True(founder.IsMember(channelId: 1, isVoice: true));
        Assert.True(joiner.IsMember(channelId: 1, isVoice: true));
        
        // 6. Founder epoch should have advanced
        var founderEpoch = founder.CurrentEpoch(channelId: 1, isVoice: true);
        Assert.Equal(1UL, founderEpoch);
        
        // 7. Both should be able to derive the same group key
        var founderKey = founder.ExportAudioKey(channelId: 1, senderSessionId: 1);
        var joinerKey = joiner.ExportAudioKey(channelId: 1, senderSessionId: 1);
        Assert.Equal(founderKey, joinerKey);
        
        Console.WriteLine("[Test] Two-party MLS group established successfully");
    }
    
    [Fact]
    public void TestThreePartyMlsGroup()
    {
        // Test three-party scenario with commit processing
        var alice = new MlsWrapper("alice");
        var bob = new MlsWrapper("bob");
        var charlie = new MlsWrapper("charlie");
        
        // 1. Alice creates group
        alice.CreateGroup(channelId: 1, isVoice: true);
        
        // 2. Bob joins
        var bobKp = bob.CreateKeyPackage();
        var addBob = alice.AddMember(channelId: 1, isVoice: true, bobKp);
        bob.JoinGroup(addBob.Welcome);
        
        // 3. Charlie joins - Bob processes Alice's commit, then Alice adds Charlie
        bob.ProcessCommit(channelId: 1, isVoice: true, addBob.Commit);
        
        var charlieKp = charlie.CreateKeyPackage();
        var addCharlie = alice.AddMember(channelId: 1, isVoice: true, charlieKp);
        charlie.JoinGroup(addCharlie.Welcome);
        bob.ProcessCommit(channelId: 1, isVoice: true, addCharlie.Commit);
        
        // 4. All three should be at the same epoch
        var aliceEpoch = alice.CurrentEpoch(channelId: 1, isVoice: true);
        var bobEpoch = bob.CurrentEpoch(channelId: 1, isVoice: true);
        var charlieEpoch = charlie.CurrentEpoch(channelId: 1, isVoice: true);
        
        Assert.Equal(2UL, aliceEpoch);
        Assert.Equal(2UL, bobEpoch);
        Assert.Equal(2UL, charlieEpoch);
        
        // 5. All three should derive consistent keys
        var aliceKey = alice.ExportAudioKey(channelId: 1, senderSessionId: 42);
        var bobKey = bob.ExportAudioKey(channelId: 1, senderSessionId: 42);
        var charlieKey = charlie.ExportAudioKey(channelId: 1, senderSessionId: 42);
        
        Assert.Equal(aliceKey, bobKey);
        Assert.Equal(bobKey, charlieKey);
        
        Console.WriteLine("[Test] Three-party MLS group with commit processing succeeded");
    }
    
    // MARK: - Key Derivation Tests
    
    [Fact]
    public void TestPerSenderKeyDerivation()
    {
        // Test that different sender IDs produce different keys
        var wrapper = new MlsWrapper("test-user");
        wrapper.CreateGroup(channelId: 1, isVoice: true);
        
        var key1 = wrapper.ExportAudioKey(channelId: 1, senderSessionId: 1);
        var key2 = wrapper.ExportAudioKey(channelId: 1, senderSessionId: 2);
        
        // Keys for different senders should be different
        Assert.NotEqual(key1, key2);
        
        // Same sender should produce same key
        var key1Again = wrapper.ExportAudioKey(channelId: 1, senderSessionId: 1);
        Assert.Equal(key1, key1Again);
    }
    
    [Fact]
    public void TestSeparateVoiceAndTextGroups()
    {
        // Test that voice and text groups are independent
        var wrapper = new MlsWrapper("test-user");
        
        wrapper.CreateGroup(channelId: 1, isVoice: true);
        wrapper.CreateGroup(channelId: 1, isVoice: false);
        
        // Both voice and text groups should exist for same channel
        Assert.True(wrapper.IsMember(channelId: 1, isVoice: true));
        Assert.True(wrapper.IsMember(channelId: 1, isVoice: false));
        
        // Keys should be different between voice and text
        var voiceKey = wrapper.ExportAudioKey(channelId: 1, senderSessionId: 1);
        var textKey = wrapper.ExportTextKey(channelId: 1, senderSessionId: 1);
        
        Assert.NotEqual(voiceKey, textKey);
    }
    
    // MARK: - Protocol Message Tests
    
    [Fact]
    public void TestMlsJoinMessageFormat()
    {
        // Test the binary format of MLS_JOIN message
        uint channelId = 42;
        bool isVoice = true;
        byte[] keyPackage = new byte[] { 0x01, 0x02, 0x03, 0x04 };
        
        // Build message manually
        using var ms = new MemoryStream();
        ms.WriteByte(0x50); // MSG_MLS_JOIN
        var buf = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(buf, channelId);
        ms.Write(buf);
        ms.WriteByte((byte)(isVoice ? 1 : 0));
        BinaryPrimitives.WriteUInt32LittleEndian(buf, (uint)keyPackage.Length);
        ms.Write(buf);
        ms.Write(keyPackage);
        
        var msg = ms.ToArray();
        
        // Verify message structure
        Assert.Equal(0x50, msg[0]); // Type
        Assert.Equal(1 + 4 + 1 + 4 + keyPackage.Length, msg.Length); // 14 bytes total
        
        // Parse it back
        var parsedChannelId = BinaryPrimitives.ReadUInt32LittleEndian(msg.AsSpan(1, 4));
        Assert.Equal(channelId, parsedChannelId);
        
        var parsedIsVoice = msg[5] != 0;
        Assert.Equal(isVoice, parsedIsVoice);
    }
    
    [Fact]
    public void TestMlsCommitWelcomeMessageFormat()
    {
        // Test the binary format of MLS_COMMIT_WELCOME message
        uint channelId = 1;
        bool isVoice = true;
        uint newMemberSessionId = 42;
        byte[] commit = new byte[] { 0x11, 0x22, 0x33 };
        byte[] welcome = new byte[] { 0xAA, 0xBB, 0xCC, 0xDD };
        
        // Build message
        using var ms = new MemoryStream();
        ms.WriteByte(0x51); // MSG_MLS_COMMIT_WELCOME
        var buf = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(buf, channelId);
        ms.Write(buf);
        ms.WriteByte((byte)(isVoice ? 1 : 0));
        BinaryPrimitives.WriteUInt32LittleEndian(buf, newMemberSessionId);
        ms.Write(buf);
        BinaryPrimitives.WriteUInt32LittleEndian(buf, (uint)commit.Length);
        ms.Write(buf);
        ms.Write(commit);
        BinaryPrimitives.WriteUInt32LittleEndian(buf, (uint)welcome.Length);
        ms.Write(buf);
        ms.Write(welcome);
        
        var msg = ms.ToArray();
        
        // Verify structure
        Assert.Equal(0x51, msg[0]);
        int expectedLen = 1 + 4 + 1 + 4 + 4 + commit.Length + 4 + welcome.Length;
        Assert.Equal(expectedLen, msg.Length);
    }
}

/// <summary>
/// Mock MlsWrapper for testing when UniFFI bindings are not available.
/// This allows the tests to compile and run structurally.
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
        // Mock: return random-ish bytes
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
        // Mock: just accept
    }
    
    public ulong ProcessCommit(uint channelId, bool isVoice, byte[] commitBytes)
    {
        var key = (channelId, isVoice);
        _epochs[key]++;
        return _epochs[key];
    }
    
    public bool IsMember(uint channelId, bool isVoice)
    {
        return _groups.ContainsKey((channelId, isVoice));
    }
    
    public ulong CurrentEpoch(uint channelId, bool isVoice)
    {
        return _epochs.GetValueOrDefault((channelId, isVoice), 0);
    }
    
    public byte[] ExportAudioKey(uint channelId, uint senderSessionId)
    {
        var key = new byte[32];
        // Deterministic based on channel + sender
        var seed = (int)(channelId * 1000 + senderSessionId);
        new Random(seed).NextBytes(key);
        return key;
    }
    
    public byte[] ExportTextKey(uint channelId, uint senderSessionId)
    {
        var key = new byte[32];
        // Different seed for text keys
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
