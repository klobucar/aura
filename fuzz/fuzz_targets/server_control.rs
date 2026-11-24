//! Server control-frame handling, as connection.rs drives it.
//!
//! Input: `[message type][actor][payload...]`, where the type is the wire byte
//! (0x10 join, 0x30 text, 0x40..=0x4C channel/profile/moderation, 0x50..=0x57
//! MLS, 0x20 audio datagram) and the payload is the frame body. The body is
//! decoded exactly as connection.rs decodes it, then handed to the same
//! `ServerState` / `AuthService` calls, against an in-memory database with an
//! admin (session 1), a plain user (session 2) and a second plain user
//! (session 3), all in the lobby's MLS groups.
//!
//! Not covered: the QUIC stream reader (`read_frame_payload`), which needs a
//! live `RecvStream`; its length checks are exercised by the connection tests.
#![no_main]

use aura_protocol as pb;
use aura_server::config::Config;
use aura_server::db::{AdminPermissions, Database};
use aura_server::state::{ServerState, ServiceMessage};
use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use prost::Message;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;

thread_local! {
    static RT: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
}

struct World {
    state: ServerState,
    /// Keep the receivers alive: a dropped one makes sends fail, which is a
    /// path production never takes while the connection is up.
    _rx: Vec<UnboundedReceiver<ServiceMessage>>,
    sessions: [u32; 3],
    users: [String; 3],
    lobby: String,
}

async fn world() -> World {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let users: [String; 3] = [0u8, 1, 2].map(|i| {
        db.create_user(&[i + 1; 32], None, &format!("user{i}"))
            .unwrap()
    });
    db.grant_admin(&users[0], &AdminPermissions::full(), None)
        .unwrap();
    let state = ServerState::new(db, Config::default());
    let lobby = state.lobby_channel_id().unwrap_or_default();
    let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut rxs = Vec::new();
    let mut sessions = [0u32; 3];
    for (i, user) in users.iter().enumerate() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        sessions[i] = state.register_session(user.clone(), addr, tx);
        rxs.push(rx);
        state.add_to_voice_group(lobby.clone(), sessions[i]).await;
        state.add_to_text_group(lobby.clone(), sessions[i]).await;
    }
    World {
        state,
        _rx: rxs,
        sessions,
        users,
        lobby,
    }
}

/// A one-byte string stands for one of the known users, so the fuzzer can
/// name real targets without guessing a UUID.
fn user_ref(w: &World, s: &str) -> String {
    match s.as_bytes() {
        [b] => w.users[(*b % 3) as usize].clone(),
        _ => s.to_string(),
    }
}

fn channel_ref(w: &World, s: &str) -> String {
    if s.is_empty() {
        w.lobby.clone()
    } else {
        s.to_string()
    }
}

async fn handle(kind: u8, actor: u8, payload: &[u8]) {
    let w = world().await;
    let who = (actor % 3) as usize;
    let (session, uuid) = (w.sessions[who], w.users[who].clone());
    let is_admin = who == 0;
    let s = &w.state;

    match kind {
        0x10 => {
            let Ok(req) = pb::JoinChannelRequest::decode(payload) else {
                return;
            };
            let channel = channel_ref(&w, &req.channel_id);
            s.remove_from_voice_group(w.lobby.clone(), session).await;
            s.remove_from_text_group(w.lobby.clone(), session).await;
            if s.channel_exists(&channel) {
                s.add_to_voice_group(channel.clone(), session).await;
                s.add_to_text_group(channel.clone(), session).await;
                s.broadcast_user_joined(channel, session, "x".into()).await;
            }
        }
        0x20 => s.route_audio_packet(Bytes::copy_from_slice(payload)).await,
        0x30 => {
            let Ok(packet) = pb::EncryptedTextPacket::decode(payload) else {
                return;
            };
            if s.broadcast_text_message(session, packet.clone()).await {
                s.request_text_ratchet(packet.channel_id).await;
            }
        }
        0x40 if is_admin => {
            if let Ok(req) = pb::CreateChannelRequest::decode(payload) {
                let _ = s
                    .create_channel_persistent(req.name, req.comment, req.icon)
                    .await;
            }
        }
        0x41 if is_admin => {
            if let Ok(req) = pb::UpdateChannelRequest::decode(payload) {
                let channel = channel_ref(&w, &req.channel_id);
                let _ = s
                    .update_channel_persistent(
                        channel,
                        req.name,
                        req.comment,
                        req.icon,
                        req.position,
                    )
                    .await;
            }
        }
        0x42 => {
            if let Ok(req) = pb::UpdateProfile::decode(payload) {
                if let Some(mut profile) = req.profile {
                    // connection.rs refuses other users' profiles before the
                    // state is touched, so only the actor's own gets through.
                    profile.user_id = uuid;
                    let _ = s.update_profile_persistent(session, profile).await;
                }
            }
        }
        0x43 if is_admin => {
            if let Ok(req) = pb::DeleteChannelRequest::decode(payload) {
                let _ = s
                    .delete_channel_persistent(&channel_ref(&w, &req.channel_id))
                    .await;
            }
        }
        0x44 => {
            if let Ok(req) = pb::DeleteUserRequest::decode(payload) {
                let target = user_ref(&w, &req.user_uuid);
                if s.auth.authorize_delete_user(&uuid, &target).is_ok() {
                    let _ = s.delete_user_persistent(&target).await;
                }
            }
        }
        0x45 => {
            if let Ok(req) = pb::UserStatusUpdate::decode(payload) {
                s.broadcast_user_status(session, req.is_muted, req.is_deafened)
                    .await;
            }
        }
        0x48 => {
            if let Ok(req) = pb::ListUsersRequest::decode(payload) {
                let _ = s.list_users(&uuid, req.offset, req.limit);
            }
        }
        0x49 => {
            if let Ok(req) = pb::VerifyUserRequest::decode(payload) {
                let _ = s
                    .auth
                    .verify_user(&uuid, &user_ref(&w, &req.target_user_uuid));
            }
        }
        0x4A => {
            if let Ok(req) = pb::BanUserRequest::decode(payload) {
                let _ = s.set_user_banned(&uuid, &user_ref(&w, &req.target_user_uuid), req.banned);
            }
        }
        0x4B => {
            if let Ok(req) = pb::GrantAdminRequest::decode(payload) {
                let perms = AdminPermissions {
                    verify_users: req.verify_users,
                    ban_users: req.ban_users,
                    grant_admin: req.grant_admin,
                    manage_channels: req.manage_channels,
                };
                let _ = s
                    .auth
                    .grant_admin(&uuid, &user_ref(&w, &req.target_user_uuid), &perms);
            }
        }
        0x4C => {
            if let Ok(req) = pb::KickUserRequest::decode(payload) {
                let _ = s.kick_user(&uuid, &user_ref(&w, &req.target_user_uuid), &req.reason);
            }
        }
        0x50 | 0x51 | 0x57 => {
            let Ok(env) = pb::MlsEnvelope::decode(payload) else {
                return;
            };
            let channel = channel_ref(&w, &env.channel_id);
            let is_voice = env.group_type() == pb::MlsGroupType::Voice;
            match env.content {
                Some(pb::mls_envelope::Content::KeyPackage(kp)) if kind == 0x50 => {
                    s.handle_mls_join(channel, is_voice, session, uuid, kp)
                        .await
                }
                Some(pb::mls_envelope::Content::CommitWelcome(cw)) if kind == 0x51 => {
                    s.handle_mls_commit_welcome(
                        channel,
                        is_voice,
                        session,
                        cw.new_member_session_id,
                        cw.commit,
                        cw.welcome,
                    )
                    .await
                }
                Some(pb::mls_envelope::Content::Commit(c)) if kind == 0x57 => {
                    s.handle_mls_ratchet_commit(channel, is_voice, session, c)
                        .await
                }
                _ => {}
            }
        }
        0x52..=0x56 | 0x58 => {
            if let Ok(env) = pb::MlsEnvelope::decode(payload) {
                let _ = s.handle_mls_message(env).await;
            }
        }
        _ => {}
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    RT.with(|rt| rt.block_on(handle(data[0], data[1], &data[2..])));
});
