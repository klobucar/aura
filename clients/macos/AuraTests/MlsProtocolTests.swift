//
//  MlsProtocolTests.swift
//  AuraTests
//
//  MLS E2EE Protocol Integration Tests
//

import Foundation
import Testing
@testable import Aura

struct MlsProtocolTests {

    /// An MLS client for a fresh hybrid identity, as the app makes one after
    /// login: `userId` is the account's `U_…` id.
    private func makeWrapper(_ userId: String) throws -> MlsWrapper {
        try MlsWrapper(userId: userId, identitySecret: generateIdentitySecret())
    }
    
    // MARK: - MLS Wrapper Tests
    
    @Test func testMlsWrapperCreation() async throws {
        // Test that MlsWrapper can be created with an identity
        let wrapper = try makeWrapper("U_test-user-1")
        #expect(wrapper != nil)
    }
    
    @Test func testKeyPackageGeneration() async throws {
        // Test key package generation
        let wrapper = try makeWrapper("U_test-user-1")
        let keyPackage = try wrapper.createKeyPackage()
        
        // Key package should be non-empty
        #expect(keyPackage.count > 0)
        print("[Test] Generated key package: \(keyPackage.count) bytes")
    }
    
    @Test func testGroupCreation() async throws {
        // Test MLS group creation (first-joiner scenario)
        let wrapper = try makeWrapper("U_founder-user")
        
        // Create voice and text groups for channel 1
        try wrapper.createGroup(channelId: "1", isVoice: true)
        try wrapper.createGroup(channelId: "1", isVoice: false)
        
        // Should be able to export audio key
        let audioKey = try wrapper.exportAudioKey(channelId: "1", senderSessionId: 1)
        #expect(audioKey.count == 32) // ChaCha20 key size
        
        // Should be member of group
        let isMember = wrapper.isMember(channelId: "1", isVoice: true)
        #expect(isMember == true)
        
        // Epoch should be 0 for new group
        let epoch = try wrapper.currentEpoch(channelId: "1", isVoice: true)
        #expect(epoch == 0)
    }
    
    @Test func testTwoPartyMlsGroup() async throws {
        // Test complete two-party MLS scenario
        let founder = try makeWrapper("U_alice")
        let joiner = try makeWrapper("U_bob")
        
        // 1. Founder creates group
        try founder.createGroup(channelId: "1", isVoice: true)
        
        // 2. Joiner generates key package
        let keyPackage = try joiner.createKeyPackage()
        
        // 3. Founder adds joiner, gets commit + welcome
        let result = try founder.addMember(channelId: "1", isVoice: true, keyPackageBytes: keyPackage)
        #expect(result.commit.count > 0)
        #expect(result.welcome.count > 0)
        
        // 4. Joiner processes welcome to join group
        try joiner.joinGroup(welcomeBytes: result.welcome)
        
        // 5. Both should now be members
        #expect(founder.isMember(channelId: "1", isVoice: true) == true)
        #expect(joiner.isMember(channelId: "1", isVoice: true) == true)
        
        // 6. Founder epoch should have advanced
        let founderEpoch = try founder.currentEpoch(channelId: "1", isVoice: true)
        #expect(founderEpoch == 1)
        
        // 7. Both should be able to derive the same group key
        let founderKey = try founder.exportAudioKey(channelId: "1", senderSessionId: 1)
        let joinerKey = try joiner.exportAudioKey(channelId: "1", senderSessionId: 1)
        #expect(founderKey == joinerKey)
        
        print("[Test] Two-party MLS group established successfully")
    }
    
    @Test func testThreePartyMlsGroup() async throws {
        // Test three-party scenario with commit processing
        let alice = try makeWrapper("U_alice")
        let bob = try makeWrapper("U_bob")
        let charlie = try makeWrapper("U_charlie")
        
        // 1. Alice creates group
        try alice.createGroup(channelId: "1", isVoice: true)
        
        // 2. Bob joins
        let bobKp = try bob.createKeyPackage()
        let addBob = try alice.addMember(channelId: "1", isVoice: true, keyPackageBytes: bobKp)
        try bob.joinGroup(welcomeBytes: addBob.welcome)
        
        // 3. Charlie joins. Bob joined from the welcome, already at Alice's
        // epoch 1, so only the commit adding Charlie is new to him.
        
        let charlieKp = try charlie.createKeyPackage()
        let addCharlie = try alice.addMember(channelId: "1", isVoice: true, keyPackageBytes: charlieKp)
        try charlie.joinGroup(welcomeBytes: addCharlie.welcome)
        _ = try bob.processCommit(channelId: "1", isVoice: true, commitBytes: addCharlie.commit)
        
        // 4. All three should be at the same epoch
        let aliceEpoch = try alice.currentEpoch(channelId: "1", isVoice: true)
        let bobEpoch = try bob.currentEpoch(channelId: "1", isVoice: true)
        let charlieEpoch = try charlie.currentEpoch(channelId: "1", isVoice: true)
        
        #expect(aliceEpoch == 2)
        #expect(bobEpoch == 2)
        #expect(charlieEpoch == 2)
        
        // 5. All three should derive consistent keys
        let aliceKey = try alice.exportAudioKey(channelId: "1", senderSessionId: 42)
        let bobKey = try bob.exportAudioKey(channelId: "1", senderSessionId: 42)
        let charlieKey = try charlie.exportAudioKey(channelId: "1", senderSessionId: 42)
        
        #expect(aliceKey == bobKey)
        #expect(bobKey == charlieKey)
        
        print("[Test] Three-party MLS group with commit processing succeeded")
    }
    
    @Test func testGroupMembersCarryStableIdentityKeys() async throws {
        // Trust decisions key on the identity key, so it must be the one the
        // member logged in with, and the same on every connection.
        let aliceSecret = generateIdentitySecret()
        let founder = try MlsWrapper(userId: "U_alice", identitySecret: aliceSecret)
        let joiner = try makeWrapper("U_bob")
        
        try founder.createGroup(channelId: "1", isVoice: true)
        let added = try founder.addMember(channelId: "1", isVoice: true, keyPackageBytes: try joiner.createKeyPackage())
        try joiner.joinGroup(welcomeBytes: added.welcome)
        
        let seenByBob = try joiner.groupMembers(channelId: "1", isVoice: true)
        let alice = seenByBob.first { $0.identity == "U_alice" }
        #expect(seenByBob.count == 2)
        #expect(seenByBob.allSatisfy { $0.identityKey.count == 1_984 })
        #expect(alice?.isSelf == false)
        #expect(alice?.identityKey == (try identityPublicKey(identitySecret: aliceSecret)))
        
        // A new connection with the same identity presents the same key
        let reconnected = try MlsWrapper(userId: "U_alice", identitySecret: aliceSecret)
        try reconnected.createGroup(channelId: "2", isVoice: true)
        let me = try reconnected.groupMembers(channelId: "2", isVoice: true).first { $0.isSelf }
        #expect(me?.identityKey == alice?.identityKey)
    }
    
    // MARK: - Key Derivation Tests
    
    @Test func testPerSenderKeyDerivation() async throws {
        // Test that different sender IDs produce different keys
        let wrapper = try makeWrapper("U_test-user")
        try wrapper.createGroup(channelId: "1", isVoice: true)
        
        let key1 = try wrapper.exportAudioKey(channelId: "1", senderSessionId: 1)
        let key2 = try wrapper.exportAudioKey(channelId: "1", senderSessionId: 2)
        
        // Keys for different senders should be different
        #expect(key1 != key2)
        
        // Same sender should produce same key
        let key1Again = try wrapper.exportAudioKey(channelId: "1", senderSessionId: 1)
        #expect(key1 == key1Again)
    }
    
    @Test func testSeparateVoiceAndTextGroups() async throws {
        // Test that voice and text groups are independent
        let wrapper = try makeWrapper("U_test-user")
        
        try wrapper.createGroup(channelId: "1", isVoice: true)
        try wrapper.createGroup(channelId: "1", isVoice: false)
        
        // Both voice and text groups should exist for same channel
        #expect(wrapper.isMember(channelId: "1", isVoice: true) == true)
        #expect(wrapper.isMember(channelId: "1", isVoice: false) == true)
        
        // Keys should be different between voice and text
        let voiceKey = try wrapper.exportAudioKey(channelId: "1", senderSessionId: 1)
        let textKey = try wrapper.exportTextKey(channelId: "1", senderSessionId: 1, epoch: nil)

        #expect(voiceKey != textKey)
    }

    @Test func testTextKeyRetainedAcrossEpochChange() async throws {
        // A message encrypted at epoch 0 must stay decryptable once a join has
        // advanced the group to epoch 1.
        let founder = try makeWrapper("U_founder")
        try founder.createGroup(channelId: "1", isVoice: false)

        let senderSessionId: UInt32 = 77
        let keyAtEpoch0 = try founder.exportTextKey(
            channelId: "1", senderSessionId: senderSessionId, epoch: 0)

        // A joiner advances the epoch.
        let joiner = try makeWrapper("U_joiner")
        let keyPackage = try joiner.createKeyPackage()
        _ = try founder.addMember(channelId: "1", isVoice: false, keyPackageBytes: keyPackage)
        #expect(try founder.currentEpoch(channelId: "1", isVoice: false) == 1)

        // The epoch-0 key is still available and differs from epoch 1's.
        let retained = try founder.exportTextKey(
            channelId: "1", senderSessionId: senderSessionId, epoch: 0)
        let keyAtEpoch1 = try founder.exportTextKey(
            channelId: "1", senderSessionId: senderSessionId, epoch: 1)

        #expect(retained == keyAtEpoch0)
        #expect(retained != keyAtEpoch1)
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
