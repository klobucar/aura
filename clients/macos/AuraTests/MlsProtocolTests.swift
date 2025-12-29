//
//  MlsProtocolTests.swift
//  AuraTests
//
//  MLS E2EE Protocol Integration Tests
//

import Testing
@testable import Aura

struct MlsProtocolTests {
    
    // MARK: - MLS Wrapper Tests
    
    @Test func testMlsWrapperCreation() async throws {
        // Test that MlsWrapper can be created with an identity
        let wrapper = try MlsWrapper(identityName: "test-user-1")
        #expect(wrapper != nil)
    }
    
    @Test func testKeyPackageGeneration() async throws {
        // Test key package generation
        let wrapper = try MlsWrapper(identityName: "test-user-1")
        let keyPackage = try wrapper.createKeyPackage()
        
        // Key package should be non-empty
        #expect(keyPackage.count > 0)
        print("[Test] Generated key package: \(keyPackage.count) bytes")
    }
    
    @Test func testGroupCreation() async throws {
        // Test MLS group creation (first-joiner scenario)
        let wrapper = try MlsWrapper(identityName: "founder-user")
        
        // Create voice and text groups for channel 1
        try wrapper.createGroup(channelId: 1, isVoice: true)
        try wrapper.createGroup(channelId: 1, isVoice: false)
        
        // Should be able to export audio key
        let audioKey = try wrapper.exportAudioKey(channelId: 1, senderSessionId: 1)
        #expect(audioKey.count == 32) // ChaCha20 key size
        
        // Should be member of group
        let isMember = wrapper.isMember(channelId: 1, isVoice: true)
        #expect(isMember == true)
        
        // Epoch should be 0 for new group
        let epoch = try wrapper.currentEpoch(channelId: 1, isVoice: true)
        #expect(epoch == 0)
    }
    
    @Test func testTwoPartyMlsGroup() async throws {
        // Test complete two-party MLS scenario
        let founder = try MlsWrapper(identityName: "alice")
        let joiner = try MlsWrapper(identityName: "bob")
        
        // 1. Founder creates group
        try founder.createGroup(channelId: 1, isVoice: true)
        
        // 2. Joiner generates key package
        let keyPackage = try joiner.createKeyPackage()
        
        // 3. Founder adds joiner, gets commit + welcome
        let result = try founder.addMember(channelId: 1, isVoice: true, keyPackageBytes: keyPackage)
        #expect(result.commit.count > 0)
        #expect(result.welcome.count > 0)
        
        // 4. Joiner processes welcome to join group
        try joiner.joinGroup(welcomeBytes: result.welcome)
        
        // 5. Both should now be members
        #expect(founder.isMember(channelId: 1, isVoice: true) == true)
        #expect(joiner.isMember(channelId: 1, isVoice: true) == true)
        
        // 6. Founder epoch should have advanced
        let founderEpoch = try founder.currentEpoch(channelId: 1, isVoice: true)
        #expect(founderEpoch == 1)
        
        // 7. Both should be able to derive the same group key
        let founderKey = try founder.exportAudioKey(channelId: 1, senderSessionId: 1)
        let joinerKey = try joiner.exportAudioKey(channelId: 1, senderSessionId: 1)
        #expect(founderKey == joinerKey)
        
        print("[Test] Two-party MLS group established successfully")
    }
    
    @Test func testThreePartyMlsGroup() async throws {
        // Test three-party scenario with commit processing
        let alice = try MlsWrapper(identityName: "alice")
        let bob = try MlsWrapper(identityName: "bob")
        let charlie = try MlsWrapper(identityName: "charlie")
        
        // 1. Alice creates group
        try alice.createGroup(channelId: 1, isVoice: true)
        
        // 2. Bob joins
        let bobKp = try bob.createKeyPackage()
        let addBob = try alice.addMember(channelId: 1, isVoice: true, keyPackageBytes: bobKp)
        try bob.joinGroup(welcomeBytes: addBob.welcome)
        
        // 3. Charlie joins - Bob processes Alice's commit, then Alice adds Charlie
        _ = try bob.processCommit(channelId: 1, isVoice: true, commitBytes: addBob.commit)
        
        let charlieKp = try charlie.createKeyPackage()
        let addCharlie = try alice.addMember(channelId: 1, isVoice: true, keyPackageBytes: charlieKp)
        try charlie.joinGroup(welcomeBytes: addCharlie.welcome)
        _ = try bob.processCommit(channelId: 1, isVoice: true, commitBytes: addCharlie.commit)
        
        // 4. All three should be at the same epoch
        let aliceEpoch = try alice.currentEpoch(channelId: 1, isVoice: true)
        let bobEpoch = try bob.currentEpoch(channelId: 1, isVoice: true)
        let charlieEpoch = try charlie.currentEpoch(channelId: 1, isVoice: true)
        
        #expect(aliceEpoch == 2)
        #expect(bobEpoch == 2)
        #expect(charlieEpoch == 2)
        
        // 5. All three should derive consistent keys
        let aliceKey = try alice.exportAudioKey(channelId: 1, senderSessionId: 42)
        let bobKey = try bob.exportAudioKey(channelId: 1, senderSessionId: 42)
        let charlieKey = try charlie.exportAudioKey(channelId: 1, senderSessionId: 42)
        
        #expect(aliceKey == bobKey)
        #expect(bobKey == charlieKey)
        
        print("[Test] Three-party MLS group with commit processing succeeded")
    }
    
    // MARK: - Key Derivation Tests
    
    @Test func testPerSenderKeyDerivation() async throws {
        // Test that different sender IDs produce different keys
        let wrapper = try MlsWrapper(identityName: "test-user")
        try wrapper.createGroup(channelId: 1, isVoice: true)
        
        let key1 = try wrapper.exportAudioKey(channelId: 1, senderSessionId: 1)
        let key2 = try wrapper.exportAudioKey(channelId: 1, senderSessionId: 2)
        
        // Keys for different senders should be different
        #expect(key1 != key2)
        
        // Same sender should produce same key
        let key1Again = try wrapper.exportAudioKey(channelId: 1, senderSessionId: 1)
        #expect(key1 == key1Again)
    }
    
    @Test func testSeparateVoiceAndTextGroups() async throws {
        // Test that voice and text groups are independent
        let wrapper = try MlsWrapper(identityName: "test-user")
        
        try wrapper.createGroup(channelId: 1, isVoice: true)
        try wrapper.createGroup(channelId: 1, isVoice: false)
        
        // Both voice and text groups should exist for same channel
        #expect(wrapper.isMember(channelId: 1, isVoice: true) == true)
        #expect(wrapper.isMember(channelId: 1, isVoice: false) == true)
        
        // Keys should be different between voice and text
        let voiceKey = try wrapper.exportAudioKey(channelId: 1, senderSessionId: 1)
        let textKey = try wrapper.exportTextKey(channelId: 1, senderSessionId: 1)
        
        #expect(voiceKey != textKey)
    }
    
    // MARK: - Protocol Message Tests
    
    @Test func testMlsJoinMessageFormat() async throws {
        // Test the binary format of MLS_JOIN message
        let channelId: UInt32 = 42
        let isVoice: Bool = true
        let keyPackage: [UInt8] = [0x01, 0x02, 0x03, 0x04]
        
        // Build message manually
        var msg = Data([0x50]) // MSG_MLS_JOIN
        msg.append(withUnsafeBytes(of: channelId.littleEndian) { Data($0) })
        msg.append(isVoice ? 1 : 0)
        msg.append(withUnsafeBytes(of: UInt32(keyPackage.count).littleEndian) { Data($0) })
        msg.append(Data(keyPackage))
        
        // Verify message structure
        #expect(msg[0] == 0x50) // Type
        #expect(msg.count == 1 + 4 + 1 + 4 + keyPackage.count) // 14 bytes total
        
        // Parse it back
        let parsedChannelId = msg.subdata(in: 1..<5).withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
        #expect(parsedChannelId == channelId)
        
        let parsedIsVoice = msg[5] != 0
        #expect(parsedIsVoice == isVoice)
    }
}
