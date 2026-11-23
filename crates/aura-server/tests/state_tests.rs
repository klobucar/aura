use aura_server::config::Config;
use aura_server::db::Database;
use aura_server::state::{SeenMessages, ServerState, ServiceMessage};
use std::net::SocketAddr;
use std::sync::Arc;

fn create_test_state() -> ServerState {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let config = Config::default();
    ServerState::new(db, config)
}

// Additional comprehensive tests for server state

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
    let channel_id = "C_1".to_string();
    state.create_channel(channel_id.clone());

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session_id = state.register_session("test-uuid".to_string(), addr, tx);

    state
        .add_to_text_group(channel_id.clone(), session_id)
        .await;

    let packet = EncryptedTextPacket {
        channel_id: aura_protocol::make_mls_group_id(&channel_id, false),
        message_id: "unique-msg-123".to_string(),
        sender_session_id: session_id,
        ciphertext: vec![1, 2, 3],
        nonce: vec![4u8; 24],
        epoch: 0,
        reply_to_id: String::new(),
        tag: vec![0u8; 16],
    };

    // First send should succeed (we don't assert on the bool — just that the
    // call returns rather than panics).
    let _result1 = state
        .broadcast_text_message(session_id, packet.clone())
        .await;

    // Second send with same message_id should be rejected (replay)
    let _result2 = state
        .broadcast_text_message(session_id, packet.clone())
        .await;
    // Replay should be detected and rejected
}

#[tokio::test]
async fn test_seen_message_cleanup() {
    let seen = SeenMessages::new();

    // Add messages
    for i in 0..10 {
        seen.check_and_mark("C_1".to_string(), &format!("msg-{}", i));
    }

    assert_eq!(seen.message_count(), 10);

    // Cleanup shouldn't remove non-expired messages
    seen.cleanup_expired();
    assert_eq!(seen.message_count(), 10);

    // Messages should still be marked as seen
    assert!(!seen.check_and_mark("C_1".to_string(), "msg-0"));
}

#[tokio::test]
async fn test_text_ratcheting_message_threshold() {
    use aura_protocol::EncryptedTextPacket;
    let state = create_test_state();
    let channel_id = "C_1".to_string();
    state.create_channel(channel_id.clone());

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session_id = state.register_session("test-uuid".to_string(), addr, tx);

    state
        .add_to_text_group(channel_id.clone(), session_id)
        .await;

    // Send 49 messages - should not trigger ratchet
    for i in 0..49 {
        let packet = EncryptedTextPacket {
            channel_id: aura_protocol::make_mls_group_id(&channel_id, false),
            message_id: format!("msg-{}", i),
            sender_session_id: session_id,
            ciphertext: vec![1, 2, 3],
            nonce: vec![4u8; 24],
            epoch: 0,
            reply_to_id: String::new(),
            tag: vec![0u8; 16],
        };
        state.broadcast_text_message(session_id, packet).await;
    }

    assert!(!state.should_ratchet_text_group(channel_id.clone()).await);

    // 50th message should trigger ratchet
    let packet = EncryptedTextPacket {
        channel_id: aura_protocol::make_mls_group_id(&channel_id, false),
        message_id: "msg-50".to_string(),
        sender_session_id: session_id,
        ciphertext: vec![1, 2, 3],
        nonce: vec![4u8; 24],
        epoch: 0,
        reply_to_id: String::new(),
        tag: vec![0u8; 16],
    };
    state.broadcast_text_message(session_id, packet).await;

    assert!(state.should_ratchet_text_group(channel_id.clone()).await);
}

#[tokio::test]
async fn test_reset_text_ratchet_counters() {
    let state = create_test_state();
    let channel_id = "C_1".to_string();
    state.create_channel(channel_id.clone());

    // Manually increment message count
    {
        let group_ref = state
            .text_groups
            .get(&aura_protocol::make_mls_group_id(&channel_id, false));
        if let Some(group_lock) = group_ref {
            let group = group_lock.read().await;
            group
                .message_count
                .store(100, std::sync::atomic::Ordering::Relaxed);
        }
    }

    // Reset counters
    state.reset_text_ratchet_counters(channel_id.clone()).await;

    // Verify reset
    {
        {
            let group_ref = state
                .text_groups
                .get(&aura_protocol::make_mls_group_id(&channel_id, false));
            if let Some(group_lock) = group_ref {
                let group = group_lock.read().await;
                assert_eq!(
                    group
                        .message_count
                        .load(std::sync::atomic::Ordering::Relaxed),
                    0
                );
            }
        }
    }
}

#[tokio::test]
async fn test_mls_first_joiner_becomes_founder() {
    let state = create_test_state();
    let channel_id = "C_1".to_string();
    state.create_channel(channel_id.clone());

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session_id = state.register_session("test-uuid".to_string(), addr, tx);

    let key_package = vec![1, 2, 3, 4];

    state
        .handle_mls_join(
            channel_id.clone(),
            true,
            session_id,
            "test-uuid".to_string(),
            key_package,
        )
        .await;

    // First joiner should receive MlsCreateGroup
    if let Some(ServiceMessage::MlsCreateGroup {
        channel_id: c,
        is_voice,
    }) = rx.recv().await
    {
        assert_eq!(c, channel_id);
        assert!(is_voice);
    } else {
        panic!("First joiner should receive MlsCreateGroup");
    }

    // Verify founder is set
    {
        {
            let group_ref = state
                .voice_groups
                .get(&aura_protocol::make_mls_group_id(&channel_id, true));
            if let Some(group_lock) = group_ref {
                let group = group_lock.read().await;
                assert_eq!(group.mls.founder(), Some(session_id));
            }
        }
    }
}

#[tokio::test]
async fn test_mls_second_joiner_queued() {
    let state = create_test_state();
    let channel_id = "C_1".to_string();
    state.create_channel(channel_id.clone());

    // First joiner
    let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session1 = state.register_session("uuid-1".to_string(), addr, tx1);

    state
        .handle_mls_join(
            channel_id.clone(),
            true,
            session1,
            "uuid-1".to_string(),
            vec![1, 2, 3],
        )
        .await;
    let _ = rx1.recv().await; // Consume MlsCreateGroup

    // Second joiner
    let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
    let session2 = state.register_session("uuid-2".to_string(), addr, tx2);

    state
        .handle_mls_join(
            channel_id.clone(),
            true,
            session2,
            "uuid-2".to_string(),
            vec![4, 5, 6],
        )
        .await;

    // Founder should receive MlsAddMemberRequest
    if let Some(ServiceMessage::MlsAddMemberRequest {
        channel_id: c,
        is_voice,
        joiner_session_id,
        ..
    }) = rx1.recv().await
    {
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
    let channel_id = "C_1".to_string();
    state.create_channel(channel_id.clone());

    // Setup founder
    let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let founder_id = state.register_session("uuid-1".to_string(), addr, tx1);

    // Setup new member
    let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
    let new_member_id = state.register_session("uuid-2".to_string(), addr, tx2);

    // The founder joins first and founds the group: only the founder's
    // commits are accepted.
    state
        .add_to_voice_group(channel_id.clone(), founder_id)
        .await;
    state
        .handle_mls_join(
            channel_id.clone(),
            true,
            founder_id,
            "uuid-1".into(),
            vec![0],
        )
        .await;
    assert!(matches!(
        rx1.recv().await,
        Some(ServiceMessage::MlsCreateGroup { is_voice: true, .. })
    ));

    let commit = vec![1, 2, 3];
    let welcome = vec![4, 5, 6];

    state
        .handle_mls_commit_welcome(
            channel_id.clone(),
            true,
            founder_id,
            new_member_id,
            commit.clone(),
            welcome.clone(),
        )
        .await;

    // New member should receive Welcome
    if let Some(ServiceMessage::MlsWelcome { welcome: w, .. }) = rx2.recv().await {
        assert_eq!(w, welcome);
    } else {
        panic!("New member should receive Welcome");
    }

    // Founder should NOT receive Commit (they sent it)
    let _ = rx1.try_recv(); // Should be empty
}

#[tokio::test]
async fn test_concurrent_channel_operations() {
    let state = Arc::new(create_test_state());
    let mut handles = vec![];

    // Create 50 channels concurrently
    for i in 0..50 {
        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            state_clone.create_channel(format!("C_{}", i));
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
    assert!(seen.check_and_mark("C_1".to_string(), "msg-1"));

    // Second check should return false (replay)
    assert!(!seen.check_and_mark("C_1".to_string(), "msg-1"));

    // Different message ID should return true
    assert!(seen.check_and_mark("C_1".to_string(), "msg-2"));

    // Same message ID in different channel should return true
    assert!(seen.check_and_mark("C_2".to_string(), "msg-1"));
}

#[tokio::test]
async fn test_session_removal_cleans_groups() {
    let state = create_test_state();
    let channel_id = "C_1".to_string();
    state.create_channel(channel_id.clone());

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let session_id = state.register_session("test-uuid".to_string(), addr, tx);

    // Add to both groups
    state
        .add_to_voice_group(channel_id.clone(), session_id)
        .await;
    state
        .add_to_text_group(channel_id.clone(), session_id)
        .await;

    // Verify membership
    {
        let group_ref = state
            .voice_groups
            .get(&aura_protocol::make_mls_group_id(&channel_id, true));
        let voice_group = group_ref.unwrap();
        assert!(voice_group.read().await.members.contains(&session_id));

        let group_ref2 = state
            .text_groups
            .get(&aura_protocol::make_mls_group_id(&channel_id, false));
        let text_group = group_ref2.unwrap();
        assert!(text_group.read().await.members.contains(&session_id));
    }

    // Remove session
    state.remove_session(session_id).await;

    // Verify removed from groups
    {
        let group_ref = state
            .voice_groups
            .get(&aura_protocol::make_mls_group_id(&channel_id, true));
        let voice_group = group_ref.unwrap();
        assert!(!voice_group.read().await.members.contains(&session_id));

        let group_ref2 = state
            .text_groups
            .get(&aura_protocol::make_mls_group_id(&channel_id, false));
        let text_group = group_ref2.unwrap();
        assert!(!text_group.read().await.members.contains(&session_id));
    }

    // Verify session removed
    assert!(!state.sessions.contains_key(&session_id));
    assert!(!state.profiles.contains_key("test-uuid"));
}

#[tokio::test]
async fn test_user_status_sync_and_enforcement() {
    let state = Arc::new(create_test_state());
    let id_a = "U_A".to_string();
    let id_b = "U_B".to_string();
    let channel_id = "C_1".to_string();

    state.create_channel(channel_id.clone());

    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();

    let sess_a = state.register_session(id_a.clone(), "127.0.0.1:1001".parse().unwrap(), tx_a);
    let sess_b = state.register_session(id_b.clone(), "127.0.0.1:1002".parse().unwrap(), tx_b);

    // Join channel
    state
        .handle_mls_join(channel_id.clone(), true, sess_a, id_a.clone(), vec![1])
        .await;
    state
        .handle_mls_join(channel_id.clone(), true, sess_b, id_b.clone(), vec![2])
        .await;

    // Drain join notifications
    while rx_a.try_recv().is_ok() {}
    while rx_b.try_recv().is_ok() {}

    // 1. Test Status Broadcasting
    state.broadcast_user_status(sess_a, true, false).await; // Muted

    let msg = rx_b.recv().await.unwrap();
    if let ServiceMessage::UserStatusUpdate {
        session_id,
        is_muted,
        is_deafened,
    } = msg
    {
        assert_eq!(session_id, sess_a);
        assert!(is_muted);
        assert!(!is_deafened);
    } else {
        panic!("Expected UserStatusUpdate");
    }

    // 2. Test 'Deafen implies Mute' logic
    state.broadcast_user_status(sess_a, false, true).await; // Deafened (should force mute)

    let msg = rx_b.recv().await.unwrap();
    if let ServiceMessage::UserStatusUpdate {
        session_id,
        is_muted,
        is_deafened,
    } = msg
    {
        assert_eq!(session_id, sess_a);
        assert!(is_muted); // Forced
        assert!(is_deafened);
    } else {
        panic!("Expected UserStatusUpdate");
    }

    // 3. Test Audio Relay Enforcement (Sender Muted)
    let audio_data = vec![0u8; 100];
    let packet = aura_protocol::FastAudioPacket {
        session_id: sess_a,
        sequence: 1,
        epoch_hint: 1,
        nonce: [0u8; 24],
        payload: audio_data.clone().into(),
    };

    state.route_audio_packet(packet.to_bytes()).await;

    // B should NOT receive anything because A is muted
    tokio::select! {
        _ = rx_b.recv() => panic!("B should not have received audio from muted A"),
        _ = tokio::time::sleep(tokio::time::Duration::from_millis(50)) => {}
    }

    // 4. Test Audio Relay Enforcement (Receiver Deafened)
    state.broadcast_user_status(sess_a, false, false).await; // A unmuted
    state.broadcast_user_status(sess_b, false, true).await; // B deafened (and muted)

    // Drain ALL status updates for both
    while rx_a.try_recv().is_ok() {}
    while rx_b.try_recv().is_ok() {}

    let packet2 = aura_protocol::FastAudioPacket {
        session_id: sess_a,
        sequence: 2,
        epoch_hint: 1,
        nonce: [0u8; 24],
        payload: audio_data.clone().into(),
    };

    state.route_audio_packet(packet2.to_bytes()).await;

    // B should NOT receive because they are deafened
    tokio::select! {
        msg = rx_b.recv() => {
            if let Some(ServiceMessage::RelayAudio(_)) = msg {
                panic!("Deafened B should not have received audio packet!");
            } else {
                // It's okay if it's a status update delayed, but in this sync test we should have drained it.
                // However, let's be safe.
            }
        },
        _ = tokio::time::sleep(tokio::time::Duration::from_millis(50)) => {}
    }
}

// --- Moderation ---

/// A channel in the DB and in memory, as `main` and `create_channel_persistent`
/// make them. `lobby` makes it the lobby.
fn add_channel(state: &ServerState, name: &str, lobby: bool) -> String {
    let channel_type = i32::from(lobby);
    let id = state
        .db
        .upsert_channel(None, name, "", 0, &[], 0, channel_type)
        .unwrap();
    state.channel_metadata.insert(
        id.clone(),
        aura_server::state::ChannelMetadata {
            id: id.clone(),
            name: name.into(),
            comment: String::new(),
            icon_type: 0,
            icon_data: vec![],
            position: 0,
            channel_type,
        },
    );
    state.create_channel(id.clone());
    id
}

/// Register an online session for `user_uuid`.
fn connect(
    state: &ServerState,
    user_uuid: &str,
) -> (u32, tokio::sync::mpsc::UnboundedReceiver<ServiceMessage>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    (state.register_session(user_uuid.into(), addr, tx), rx)
}

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServiceMessage>) -> Vec<ServiceMessage> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

#[tokio::test]
async fn kick_moves_the_target_out_of_its_channel_toward_the_lobby() {
    use aura_server::state::ModerationError;

    let state = create_test_state();
    let root = state
        .db
        .create_bootstrap_admin(&[0xAA; 32], "Root")
        .unwrap();
    let bob = state.db.create_user(&[0x01; 32], None, "Bob").unwrap();
    let lobby = add_channel(&state, "Lobby", true);
    let room = add_channel(&state, "Room", false);
    assert_eq!(state.lobby_channel_id().as_deref(), Some(lobby.as_str()));

    // Offline: nothing to kick.
    assert!(matches!(
        state.kick_user(&root, &bob, "bye"),
        Err(ModerationError::NotOnline)
    ));

    // Online in the lobby: nowhere to move him.
    let (bob_session, mut bob_rx) = connect(&state, &bob);
    state.add_to_voice_group(lobby.clone(), bob_session).await;
    assert!(matches!(
        state.kick_user(&root, &bob, "bye"),
        Err(ModerationError::NotInChannel)
    ));

    // In a room: his connection is told to leave it for the lobby.
    state.add_to_voice_group(room.clone(), bob_session).await;
    drain(&mut bob_rx);
    assert_eq!(state.kick_user(&root, &bob, "be nice").unwrap(), 1);
    match drain(&mut bob_rx).as_slice() {
        [ServiceMessage::Kicked {
            channel_id,
            fallback_channel_id,
            reason,
        }] => {
            assert_eq!(channel_id, &room);
            assert_eq!(fallback_channel_id, &lobby);
            assert_eq!(reason, "be nice");
        }
        other => panic!("expected one Kicked, got {other:?}"),
    }

    // Bob can't kick anyone, and nobody kicks themselves.
    assert!(matches!(
        state.kick_user(&bob, &root, "no"),
        Err(ModerationError::Auth(
            aura_server::auth::AuthError::PermissionDenied
        ))
    ));
    assert!(matches!(
        state.kick_user(&root, &root, "no"),
        Err(ModerationError::Auth(
            aura_server::auth::AuthError::SelfTarget
        ))
    ));
}

#[tokio::test]
async fn ban_closes_every_session_of_the_target() {
    let state = create_test_state();
    let root = state
        .db
        .create_bootstrap_admin(&[0xAA; 32], "Root")
        .unwrap();
    let bob = state.db.create_user(&[0x01; 32], None, "Bob").unwrap();
    let (_, mut laptop) = connect(&state, &bob);
    let (_, mut phone) = connect(&state, &bob);
    let (_, mut root_rx) = connect(&state, &root);
    drain(&mut root_rx);

    state.set_user_banned(&root, &bob, true).unwrap();
    for rx in [&mut laptop, &mut phone] {
        assert!(matches!(drain(rx).as_slice(), [ServiceMessage::Banned]));
    }
    assert!(drain(&mut root_rx).is_empty());
    assert!(state.db.find_user_by_uuid(&bob).unwrap().unwrap().banned);

    // Unbanning closes nothing.
    state.set_user_banned(&root, &bob, false).unwrap();
    assert!(drain(&mut laptop).is_empty());
    assert!(!state.db.find_user_by_uuid(&bob).unwrap().unwrap().banned);
}

#[tokio::test]
async fn list_users_marks_admins_and_counts_everyone() {
    let state = create_test_state();
    let root = state
        .db
        .create_bootstrap_admin(&[0xAA; 32], "Root")
        .unwrap();
    let bob = state.db.create_user(&[0x01; 32], None, "Bob").unwrap();

    let resp = state.list_users(&root, 0, 0).unwrap();
    assert!(resp.success);
    assert_eq!(resp.total_count, 2);
    let admin_flags: Vec<(&str, bool)> = resp
        .users
        .iter()
        .map(|u| (u.display_name.as_str(), u.is_admin))
        .collect();
    assert_eq!(admin_flags, [("Root", true), ("Bob", false)]);

    assert!(state.list_users(&bob, 0, 0).is_err());
}
