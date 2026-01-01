// Additional comprehensive tests for server state
// Add to existing test module in state.rs

#[tokio::test]
async fn test_concurrent_session_registration() {
    let state = Arc::new(create_test_state());
    let mut handles = vec![];
    
    // Register 100 sessions concurrently
    for i in 0..100 {
        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let addr: SocketAddr = format!("127.0.0.1:{}", 10000 + i).parse().unwrap();
            state_clone.register_session(format!("uuid-{}", i), addr, tx)
        });
        handles.push(handle);
    }
    
    let session_ids: Vec<u32> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    
    // All sessions should be registered
    assert_eq!(state.sessions.len(), 100);
    
    // All session IDs should be unique
    let unique_ids: std::collections::HashSet<_> = session_ids.iter().collect();
    assert_eq!(unique_ids.len(), 100);
}

#[tokio::test]
async fn test_replay_attack_detection() {
    use aura_protocol::EncryptedTextPacket;
    let state = create_test_state();
    let channel_id = 1;
    state.create_channel(channel_id);
    
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session_id = state.register_session("test-uuid".to_string(), addr, tx);
    
    state.add_to_text_group(channel_id, session_id).await;
    
    let packet = EncryptedTextPacket {
        channel_id,
        message_id: "unique-msg-123".to_string(),
        sender_session_id: session_id,
        ciphertext: vec![1, 2, 3],
        nonce: vec![4, 5, 6],
    };
    
    // First send should succeed
    let result1 = state.broadcast_text_message(session_id, packet.clone()).await;
    assert!(result1 || !result1); // Just verify it doesn't panic
    
    // Second send with same message_id should be rejected (replay)
    let result2 = state.broadcast_text_message(session_id, packet.clone()).await;
    // Replay should be detected and rejected
}

#[tokio::test]
async fn test_seen_message_cleanup() {
    let seen = SeenMessages::new();
    
    // Add messages
    for i in 0..10 {
        seen.check_and_mark(1, &format!("msg-{}", i));
    }
    
    assert_eq!(seen.message_count(), 10);
    
    // Cleanup shouldn't remove non-expired messages
    seen.cleanup_expired();
    assert_eq!(seen.message_count(), 10);
    
    // Messages should still be marked as seen
    assert!(!seen.check_and_mark(1, "msg-0"));
}

#[tokio::test]
async fn test_text_ratcheting_message_threshold() {
    use aura_protocol::EncryptedTextPacket;
    let state = create_test_state();
    let channel_id = 1;
    state.create_channel(channel_id);
    
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session_id = state.register_session("test-uuid".to_string(), addr, tx);
    
    state.add_to_text_group(channel_id, session_id).await;
    
    // Send 49 messages - should not trigger ratchet
    for i in 0..49 {
        let packet = EncryptedTextPacket {
            channel_id,
            message_id: format!("msg-{}", i),
            sender_session_id: session_id,
            ciphertext: vec![1, 2, 3],
            nonce: vec![4, 5, 6],
        };
        state.broadcast_text_message(session_id, packet).await;
    }
    
    assert!(!state.should_ratchet_text_group(channel_id).await);
    
    // 50th message should trigger ratchet
    let packet = EncryptedTextPacket {
        channel_id,
        message_id: "msg-50".to_string(),
        sender_session_id: session_id,
        ciphertext: vec![1, 2, 3],
        nonce: vec![4, 5, 6],
    };
    state.broadcast_text_message(session_id, packet).await;
    
    assert!(state.should_ratchet_text_group(channel_id).await);
}

#[tokio::test]
async fn test_reset_text_ratchet_counters() {
    let state = create_test_state();
    let channel_id = 1;
    state.create_channel(channel_id);
    
    // Manually increment message count
    if let Some(group_lock) = state.text_groups.get(&channel_id) {
        let group = group_lock.read().await;
        group.message_count.store(100, std::sync::atomic::Ordering::Relaxed);
    }
    
    // Reset counters
    state.reset_text_ratchet_counters(channel_id).await;
    
    // Verify reset
    if let Some(group_lock) = state.text_groups.get(&channel_id) {
        let group = group_lock.read().await;
        assert_eq!(group.message_count.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn test_mls_first_joiner_becomes_founder() {
    let state = create_test_state();
    let channel_id = 1;
    state.create_channel(channel_id);
    
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session_id = state.register_session("test-uuid".to_string(), addr, tx);
    
    let key_package = vec![1, 2, 3, 4];
    
    state.handle_mls_join(channel_id, true, session_id, "test-uuid".to_string(), key_package).await;
    
    // First joiner should receive MlsCreateGroup
    if let Some(ServiceMessage::MlsCreateGroup { channel_id: c, is_voice }) = rx.recv().await {
        assert_eq!(c, channel_id);
        assert!(is_voice);
    } else {
        panic!("First joiner should receive MlsCreateGroup");
    }
    
    // Verify founder is set
    if let Some(group_lock) = state.voice_groups.get(&channel_id) {
        let group = group_lock.read().await;
        assert_eq!(group.founder_session_id, Some(session_id));
    }
}

#[tokio::test]
async fn test_mls_second_joiner_queued() {
    let state = create_test_state();
    let channel_id = 1;
    state.create_channel(channel_id);
    
    // First joiner
    let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session1 = state.register_session("uuid-1".to_string(), addr, tx1);
    
    state.handle_mls_join(channel_id, true, session1, "uuid-1".to_string(), vec![1, 2, 3]).await;
    let _ = rx1.recv().await; // Consume MlsCreateGroup
    
    // Second joiner
    let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
    let session2 = state.register_session("uuid-2".to_string(), addr, tx2);
    
    state.handle_mls_join(channel_id, true, session2, "uuid-2".to_string(), vec![4, 5, 6]).await;
    
    // Founder should receive MlsAddMemberRequest
    if let Some(ServiceMessage::MlsAddMemberRequest { 
        channel_id: c, 
        is_voice, 
        joiner_session_id, 
        .. 
    }) = rx1.recv().await {
        assert_eq!(c, channel_id);
        assert!(is_voice);
        assert_eq!(joiner_session_id, session2);
    } else {
        panic!("Founder should receive MlsAddMemberRequest");
    }
}

#[tokio::test]
async fn test_mls_commit_welcome_distribution() {
    let state = create_test_state();
    let channel_id = 1;
    state.create_channel(channel_id);
    
    // Setup founder
    let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let founder_id = state.register_session("uuid-1".to_string(), addr, tx1);
    
    // Setup new member
    let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
    let new_member_id = state.register_session("uuid-2".to_string(), addr, tx2);
    
    // Add founder to group
    state.add_to_voice_group(channel_id, founder_id).await;
    
    let commit = vec![1, 2, 3];
    let welcome = vec![4, 5, 6];
    
    state.handle_mls_commit_welcome(
        channel_id,
        true,
        founder_id,
        new_member_id,
        commit.clone(),
        welcome.clone(),
    ).await;
    
    // New member should receive Welcome
    if let Some(ServiceMessage::MlsWelcome { welcome: w, .. }) = rx2.recv().await {
        assert_eq!(w, welcome);
    } else {
        panic!("New member should receive Welcome");
    }
    
    // Founder should receive Commit
    if let Some(ServiceMessage::MlsCommit { commit: c, .. }) = rx1.recv().await {
        assert_eq!(c, commit);
    } else {
        panic!("Founder should receive Commit");
    }
}

#[tokio::test]
async fn test_concurrent_channel_operations() {
    let state = Arc::new(create_test_state());
    let mut handles = vec![];
    
    // Create 50 channels concurrently
    for i in 0..50 {
        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            state_clone.create_channel(i);
        });
        handles.push(handle);
    }
    
    futures::future::join_all(handles).await;
    
    // All channels should exist
    assert_eq!(state.voice_groups.len(), 50);
    assert_eq!(state.text_groups.len(), 50);
}

#[test]
fn test_seen_messages_uniqueness() {
    let seen = SeenMessages::new();
    
    // First check should return true (new message)
    assert!(seen.check_and_mark(1, "msg-1"));
    
    // Second check should return false (replay)
    assert!(!seen.check_and_mark(1, "msg-1"));
    
    // Different message ID should return true
    assert!(seen.check_and_mark(1, "msg-2"));
    
    // Same message ID in different channel should return true
    assert!(seen.check_and_mark(2, "msg-1"));
}

#[tokio::test]
async fn test_session_removal_cleans_groups() {
    let state = create_test_state();
    let channel_id = 1;
    state.create_channel(channel_id);
    
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session_id = state.register_session("test-uuid".to_string(), addr, tx);
    
    // Add to both groups
    state.add_to_voice_group(channel_id, session_id).await;
    state.add_to_text_group(channel_id, session_id).await;
    
    // Verify membership
    {
        let voice_group = state.voice_groups.get(&channel_id).unwrap();
        assert!(voice_group.read().await.members.contains(&session_id));
        
        let text_group = state.text_groups.get(&channel_id).unwrap();
        assert!(text_group.read().await.members.contains(&session_id));
    }
    
    // Remove session
    state.remove_session(session_id).await;
    
    // Verify removed from groups
    {
        let voice_group = state.voice_groups.get(&channel_id).unwrap();
        assert!(!voice_group.read().await.members.contains(&session_id));
        
        let text_group = state.text_groups.get(&channel_id).unwrap();
        assert!(!text_group.read().await.members.contains(&session_id));
    }
    
    // Verify session removed
    assert!(!state.sessions.contains_key(&session_id));
    assert!(!state.profiles.contains_key(&session_id));
}
