//! End-to-end tests: a real `aura-server` and headless clients built from
//! the shared Rust core, talking QUIC over loopback.
//!
//! These cover the whole path the unit and state tests can't: login (hybrid
//! identity challenge), channel join, the MLS join/commit/welcome dance,
//! encrypted text, encrypted voice datagrams, membership changes and
//! moderation. See `tests/common/mod.rs` for the harness.

mod common;

use std::time::Duration;

use aura_core::crypto::DaveCrypto;
use aura_core::uniffi_bindings::AudioSenderWrapper;
use aura_identity::IdentitySecret;
use aura_protocol::{ChannelRemovalReason, FastAudioPacket, AUDIO_DATAGRAM};
use common::*;
use tokio::time::sleep;

/// 20 ms of a 440 Hz tone at 48 kHz, mono.
fn tone(frame: usize) -> Vec<i16> {
    tone_at(frame, 12_000.0)
}

fn tone_at(frame: usize, amplitude: f32) -> Vec<i16> {
    (0..960)
        .map(|i| {
            let t = (frame * 960 + i) as f32 / 48_000.0;
            ((t * 440.0 * std::f32::consts::TAU).sin() * amplitude) as i16
        })
        .collect()
}

fn rms(pcm: &[i16]) -> f64 {
    (pcm.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / pcm.len().max(1) as f64).sqrt()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Founder `a` joins `channel`, then `b` joins and is added to both MLS
/// groups. Returns once both hold epoch 1 of the voice and text groups.
async fn two_in_channel(server: &TestServer, channel: &str) -> (TestClient, TestClient) {
    let mut a = TestClient::connect(server, "alice").await;
    a.join_channel(channel).await;
    a.wait_ready(channel).await;

    let mut b = TestClient::connect(server, "bob").await;
    b.join_channel(channel).await;
    b.wait_ready(channel).await;

    for client in [&a, &b] {
        for is_voice in [true, false] {
            client.wait_epoch(channel, is_voice, 1).await;
        }
    }
    a.wait_sees(channel, b.session_id).await;
    b.wait_sees(channel, a.session_id).await;
    (a, b)
}

// ---------------------------------------------------------------------------
// 1. Login, join, MLS and end-to-end encrypted text
// ---------------------------------------------------------------------------

#[tokio::test]
async fn text_is_end_to_end_encrypted_between_two_clients() {
    let server = TestServer::start().await;
    let channel = server.create_channel("Room").await;
    let (a, mut b) = two_in_channel(&server, &channel).await;
    let mut a = a;

    // Login handed out distinct sessions and accounts.
    assert_ne!(a.session_id, b.session_id);
    assert_ne!(a.user_uuid, b.user_uuid);
    assert!(!a.is_admin && !b.is_admin);

    let secret = "the quick brown fox jumps over the lazy dog";
    let sent = a.send_text(&channel, secret).await;
    let got = b.next_text().await;
    assert_eq!(got.packet.sender_session_id, a.session_id);
    assert_eq!(got.packet.message_id, sent.message_id);
    assert_eq!(got.packet.epoch, sent.epoch);
    assert_eq!(got.message.expect("bob decrypts").content, secret);

    // The server relayed ciphertext only: neither what alice sent nor what
    // bob was handed contains the plaintext, and the relay is byte-for-byte
    // what alice sent.
    assert!(!contains(&sent.wire, secret.as_bytes()));
    assert!(!contains(&got.wire, secret.as_bytes()));
    assert_eq!(got.wire, sent.wire);

    // And back.
    let reply = "reply: sphinx of black quartz, judge my vow";
    let sent = b.send_text(&channel, reply).await;
    let got = a.next_text().await;
    assert_eq!(got.packet.sender_session_id, b.session_id);
    assert_eq!(got.message.expect("alice decrypts").content, reply);
    assert!(!contains(&sent.wire, reply.as_bytes()));
    assert!(!contains(&got.wire, reply.as_bytes()));

    // Replaying a message id is dropped by the relay, not delivered twice.
    let _ = a.send_text(&channel, "first").await;
    let first = b.next_text().await;
    assert_eq!(first.message.unwrap().content, "first");
    b.sync().await;
    assert!(!b.has_event(|e| matches!(e, Event::Text(_))));
}

// ---------------------------------------------------------------------------
// 2. Encrypted voice
// ---------------------------------------------------------------------------

#[tokio::test]
async fn voice_frames_are_encrypted_ordered_and_attributed() {
    let server = TestServer::start().await;
    let channel = server.create_channel("Room").await;
    let (a, b) = two_in_channel(&server, &channel).await;

    const N: usize = 10;
    let mut sent = Vec::new();
    let mut decoded = Vec::new();
    for frame in 0..N {
        sent.push(a.send_audio_frame(&tone(frame)).expect("send audio"));
        sleep(Duration::from_millis(20)).await;
        decoded.extend(b.pop_decoded());
    }

    // Nothing on the wire is a bare Opus frame: every packet is header +
    // XChaCha20-Poly1305 ciphertext, so equal frames never look equal.
    for packet in &sent {
        let parsed = FastAudioPacket::parse(bytes::Bytes::copy_from_slice(packet)).unwrap();
        assert_eq!(parsed.session_id, a.session_id);
        assert!(parsed.payload.len() > 16, "ciphertext carries a tag");
    }

    // Every datagram reached bob, in order, attributed to alice, and passed
    // authentication + decryption.
    let log = b.wait_audio_packets(N).await;
    assert_eq!(log.len(), N, "no extra or duplicated audio");
    for (i, rec) in log.iter().enumerate() {
        assert_eq!(rec.session_id, a.session_id, "frame {i} attribution");
        assert_eq!(rec.sequence as usize, i, "frame {i} sequence");
        assert!(rec.accepted.is_ok(), "frame {i}: {:?}", rec.accepted);
    }

    // The receive path decoded real audio from alice and nobody else.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while decoded.iter().filter(|(_, pcm)| rms(pcm) > 500.0).count() < N / 2
        && std::time::Instant::now() < deadline
    {
        sleep(Duration::from_millis(10)).await;
        decoded.extend(b.pop_decoded());
    }
    assert!(decoded.iter().all(|(s, _)| *s == a.session_id));
    let loud = decoded.iter().filter(|(_, pcm)| rms(pcm) > 500.0).count();
    assert!(loud >= N / 2, "decoded tone frames: {loud} of {N}");

    // The server doesn't echo audio to its sender.
    assert!(a.audio_log().is_empty());
}

#[tokio::test]
async fn receiver_rejects_unencrypted_and_wrongly_keyed_audio() {
    let server = TestServer::start().await;
    let channel = server.create_channel("Room").await;
    let (a, b) = two_in_channel(&server, &channel).await;

    // One good frame first, so the stream is established.
    a.send_audio_frame(&tone(0)).unwrap();
    b.wait_audio_packets(1).await;
    assert!(b.audio_log()[0].accepted.is_ok());

    // 1. A frame carrying plaintext (here: raw PCM bytes standing in for a
    //    bare Opus frame) under alice's session id: the server relays
    //    datagrams without looking inside, so the receiver is the policy.
    let plaintext: Vec<u8> = tone(1)
        .iter()
        .take(100)
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let unencrypted = FastAudioPacket {
        session_id: a.session_id,
        epoch_hint: 1,
        sequence: 1,
        nonce: DaveCrypto::random_nonce(),
        payload: bytes::Bytes::from(plaintext),
    }
    .to_bytes();
    a.send_audio_datagram(&unencrypted).unwrap();

    // 2. A properly formed frame encrypted under a key the group never had.
    let rogue = AudioSenderWrapper::new(a.session_id, &DaveCrypto::random_key()).unwrap();
    let _ = rogue.process(&tone(2)).unwrap(); // burn seq 0
    let wrong_key = rogue.process(&tone(3)).unwrap().unwrap();
    a.send_audio_datagram(&wrong_key).unwrap();

    let log = b.wait_audio_packets(3).await;
    assert!(log[1].accepted.is_err(), "plaintext frame must be rejected");
    assert!(log[2].accepted.is_err(), "wrong-key frame must be rejected");

    // Nothing from those frames is queued for playback.
    sleep(Duration::from_millis(100)).await;
    let decoded = b.pop_decoded();
    assert!(
        decoded.len() <= 1,
        "only the one authentic frame may decode, got {}",
        decoded.len()
    );

    // A sender that has no MLS key yet doesn't send at all.
    let mut c = TestClient::connect(&server, "carol").await;
    assert!(c.send_audio_frame(&tone(0)).is_err());
    c.sync().await;
}

/// Injected, unauthenticated frames must not break the real stream: a
/// forged datagram with an arbitrary sequence number may not move the
/// receiver's replay window.
#[tokio::test]
async fn forged_frame_does_not_poison_the_replay_window() {
    let server = TestServer::start().await;
    let channel = server.create_channel("Room").await;
    let (a, b) = two_in_channel(&server, &channel).await;

    a.send_audio_frame(&tone(0)).unwrap();
    b.wait_audio_packets(1).await;

    let forged = FastAudioPacket {
        session_id: a.session_id,
        epoch_hint: 1,
        sequence: 20_000,
        nonce: DaveCrypto::random_nonce(),
        payload: bytes::Bytes::from(vec![0xAB; 64]),
    }
    .to_bytes();
    a.send_audio_datagram(&forged).unwrap();
    b.wait_audio_packets(2).await;
    assert!(b.audio_log()[1].accepted.is_err());

    a.send_audio_frame(&tone(1)).unwrap();
    let log = b.wait_audio_packets(3).await;
    assert!(
        log[2].accepted.is_ok(),
        "genuine frame after a forged one was dropped: {:?}",
        log[2].accepted
    );
}

/// The relay keys fan-out off the session id inside the datagram, so a client
/// that writes someone else's id there speaks as them: into their channel,
/// past their mute. Datagrams must only be relayed for the session that sent
/// them.
#[tokio::test]
async fn audio_claiming_another_session_is_not_relayed() {
    let server = TestServer::start().await;
    let channel = server.create_channel("Room").await;
    let (a, b) = two_in_channel(&server, &channel).await;

    // Carol is logged in but sits in the lobby, outside alice's channel.
    let mut c = TestClient::connect(&server, "carol").await;
    let forged = FastAudioPacket {
        session_id: a.session_id,
        epoch_hint: 1,
        sequence: 7,
        nonce: DaveCrypto::random_nonce(),
        payload: bytes::Bytes::from(vec![0xCD; 64]),
    }
    .to_bytes();
    c.send_audio_datagram(&forged).unwrap();
    c.sync().await;
    sleep(Duration::from_millis(300)).await;
    assert!(
        b.audio_log().is_empty(),
        "bob was handed a datagram carol sent in alice's name"
    );
}

/// Authentic frames at any sequence number. The header is authenticated, so
/// the key holder (not a relay) opens one genuine frame and seals its audio
/// again under each header.
fn restampable_frame(key: &[u8; 32], amplitude: f32) -> impl Fn(u16) -> Vec<u8> {
    let sender = AudioSenderWrapper::new(9, key).unwrap();
    let template = sender.process(&tone_at(0, amplitude)).unwrap().unwrap();
    let template = FastAudioPacket::parse(bytes::Bytes::from(template)).unwrap();
    let crypto = DaveCrypto::new(key);
    let opus = crypto
        .decrypt_with_aad(&template.payload, &template.nonce, &template.aad())
        .unwrap();
    move |seq| {
        let nonce = DaveCrypto::random_nonce();
        let aad = FastAudioPacket::aad_for(9, template.epoch_hint, seq);
        FastAudioPacket {
            session_id: 9,
            epoch_hint: template.epoch_hint,
            sequence: seq,
            nonce,
            payload: bytes::Bytes::from(crypto.encrypt_with_aad(&opus, &nonce, &aad).unwrap()),
        }
        .to_bytes()
        .to_vec()
    }
}

/// The server is untrusted. One that captures a genuine frame and delivers it
/// again with a rewritten header (a new sequence number, another epoch or
/// session) must not get it played a second time or move the receiver's
/// window. The server's sender check stops any *client* from injecting that
/// datagram, so the rewritten datagrams are handed to the receiving client's
/// datagram handler directly, exactly as the relay would deliver them.
#[tokio::test]
async fn relay_replaying_a_frame_under_a_new_sequence_is_not_played() {
    let server = TestServer::start().await;
    let channel = server.create_channel("Room").await;
    let (a, b) = two_in_channel(&server, &channel).await;

    // Genuine frames reach bob through the real relay and play.
    let mut sent = Vec::new();
    for frame in 0..4 {
        sent.push(a.send_audio_frame(&tone(frame)).unwrap());
        sleep(Duration::from_millis(20)).await;
    }
    let log = b.wait_audio_packets(4).await;
    assert!(log.iter().all(|r| r.accepted.is_ok()));
    let mut played = 0;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while played < 3 && std::time::Instant::now() < deadline {
        sleep(Duration::from_millis(20)).await;
        played += b.pop_decoded().len();
    }
    assert!(played >= 3, "genuine frames should play, got {played}");
    sleep(Duration::from_millis(100)).await;
    let _ = b.pop_decoded();

    // The relay replays the last frame: restamped as the next frame, far
    // ahead, with another epoch hint, and under another session id.
    let mut captured = vec![AUDIO_DATAGRAM];
    captured.extend_from_slice(sent.last().unwrap());
    let before = b.audio_log().len();
    let restamp = |range: std::ops::Range<usize>, value: &[u8]| {
        let mut p = captured.clone();
        p[range].copy_from_slice(value);
        p
    };
    let forged = [
        restamp(7..9, &4u16.to_be_bytes()),
        restamp(7..9, &30_000u16.to_be_bytes()),
        restamp(6..7, &[0x55]),
        restamp(1..5, &(a.session_id + 1).to_be_bytes()),
    ];
    for datagram in &forged {
        b.deliver_datagram(datagram);
    }
    let log = b.audio_log();
    assert_eq!(log.len(), before + forged.len());
    assert!(
        log[before..].iter().all(|r| r.accepted.is_err()),
        "a re-stamped frame was accepted: {:?}",
        &log[before..]
    );
    // (A refused frame is never queued, so it can't play; the decoder may
    // still conceal the gap, which is why output isn't asserted empty.)

    // The window did not move: alice's next real frame still plays.
    a.send_audio_frame(&tone(4)).unwrap();
    let log = b.wait_audio_packets(before + forged.len() + 1).await;
    assert!(log.last().unwrap().accepted.is_ok());
}

/// A client still on the version 1 audio format is relayed under its own tag
/// and reported by the receiver, never decrypted or played.
#[tokio::test]
async fn older_audio_format_is_reported_and_not_played() {
    let server = TestServer::start().await;
    let channel = server.create_channel("Room").await;
    let (a, b) = two_in_channel(&server, &channel).await;

    let frame = FastAudioPacket::with_sequence_nonce(
        a.session_id,
        1,
        0,
        bytes::Bytes::from(vec![0x42; 64]),
    )
    .to_bytes();
    a.send_legacy_audio_datagram(&frame).unwrap();
    let log = b.wait_audio_packets(1).await;
    assert_eq!(log[0].session_id, a.session_id);
    assert!(log[0].accepted.is_err());
    sleep(Duration::from_millis(100)).await;
    assert!(b.pop_decoded().is_empty());

    // And it did not disturb the current-format stream.
    a.send_audio_frame(&tone(0)).unwrap();
    let log = b.wait_audio_packets(2).await;
    assert!(log[1].accepted.is_ok());
}

/// Frame counters are 16-bit and wrap after about 22 minutes of speech
/// (65,536 frames of 20 ms). The receiver has to keep playing across it.
#[tokio::test]
async fn receiver_keeps_playing_across_sequence_wraparound() {
    let key = DaveCrypto::random_key();
    // Quiet before the wrap, loud after: concealment of lost frames can only
    // fade out the quiet audio, so loud output means the post-wrap frames
    // really were played.
    let quiet = restampable_frame(&key, 1_000.0);
    let loud = restampable_frame(&key, 12_000.0);
    let receiver = aura_core::uniffi_bindings::AudioReceiverWrapper::new();
    receiver.add_sender(9, &key, 0).unwrap();

    let mut loudest_after_wrap = 0.0f64;
    for seq in (65_530u16..=65_535).chain(0..6) {
        let frame = if seq > 100 { quiet(seq) } else { loud(seq) };
        receiver
            .on_packet(&frame)
            .unwrap_or_else(|e| panic!("frame {seq} refused: {e:?}"));
        sleep(Duration::from_millis(21)).await;
        for decoded in receiver.pop_decoded() {
            if seq <= 100 {
                loudest_after_wrap = loudest_after_wrap.max(rms(&decoded.pcm));
            }
        }
    }
    assert!(
        loudest_after_wrap > 4_000.0,
        "frames after the wrap were not played (loudest output {loudest_after_wrap:.0})"
    );
}

/// A listener who joins while someone has been speaking for a while has to
/// hear them from the first frame they get, whatever its sequence number.
#[tokio::test]
async fn receiver_accepts_a_stream_already_in_progress() {
    let key = DaveCrypto::random_key();
    let frame_at = restampable_frame(&key, 12_000.0);
    let receiver = aura_core::uniffi_bindings::AudioReceiverWrapper::new();
    receiver.add_sender(9, &key, 0).unwrap();
    receiver
        .on_packet(&frame_at(40_000))
        .expect("first frame of an in-progress stream was refused as a replay");
}

// ---------------------------------------------------------------------------
// 3. Membership changes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn membership_changes_advance_epochs_and_cut_off_leavers() {
    let server = TestServer::start().await;
    let channel = server.create_channel("Room").await;
    let (mut a, mut b) = two_in_channel(&server, &channel).await;

    // C joins: the founder commits an Add, everyone advances to epoch 2.
    let mut c = TestClient::connect(&server, "carol").await;
    c.join_channel(&channel).await;
    c.wait_ready(&channel).await;
    for client in [&a, &b, &c] {
        for is_voice in [true, false] {
            client.wait_epoch(&channel, is_voice, 2).await;
        }
    }
    for client in [&a, &b, &c] {
        for other in [&a, &b, &c] {
            client.wait_sees(&channel, other.session_id).await;
        }
    }

    // All three can talk.
    a.send_text(&channel, "hello both").await;
    for rx in [&mut b, &mut c] {
        assert_eq!(rx.next_text().await.message.unwrap().content, "hello both");
    }
    c.send_text(&channel, "hi from carol").await;
    for rx in [&mut a, &mut b] {
        let got = rx.next_text().await;
        assert_eq!(got.packet.sender_session_id, c.session_id);
        assert_eq!(got.message.unwrap().content, "hi from carol");
    }
    a.send_audio_frame(&tone(0)).unwrap();
    for rx in [&b, &c] {
        let log = rx.wait_audio_packets(1).await;
        assert!(log[0].accepted.is_ok());
    }

    // C leaves (moves to the lobby) but keeps its local MLS state, so the
    // test proves the keys are useless, not that the client forgot them.
    c.join_channel_keeping_old_groups(&server.lobby_id).await;

    // The server asked the founder to drop C: one removal commit per group.
    let carol_uuid = c.user_uuid.clone();
    for is_voice in [true, false] {
        let sync = a
            .wait_event("alice's member sync", |e| {
                matches!(e, Event::SyncMembers(s)
                    if (s.group_type() == aura_protocol::MlsGroupType::Voice) == is_voice)
            })
            .await;
        let Event::SyncMembers(sync) = sync else {
            unreachable!()
        };
        assert_eq!(sync.channel_id, channel);
        assert!(!sync.member_uuids.contains(&carol_uuid));
        assert_eq!(sync.member_uuids.len(), 2);
    }
    for client in [&a, &b] {
        for is_voice in [true, false] {
            client.wait_epoch(&channel, is_voice, 3).await;
        }
    }
    // C never saw the removal commit: it is stuck at its last epoch.
    assert_eq!(c.epoch(&channel, false), Some(2));
    assert_eq!(c.epoch(&channel, true), Some(2));

    // The remaining two still talk, at the new epoch...
    let secret = "carol must not read this";
    let sent = a.send_text(&channel, secret).await;
    assert_eq!(sent.epoch, 3);
    let got = b.next_text().await;
    assert_eq!(got.message.as_ref().unwrap().content, secret);

    // ...and C can't decrypt it even when handed the exact packet.
    let err = c
        .try_decrypt(&got.packet)
        .expect_err("a removed member decrypted new traffic");
    assert!(err.contains("key derivation failed"), "{err}");
    // Nor does it ever receive it.
    c.sync().await;
    assert!(!c.has_event(|e| matches!(e, Event::Text(_))));

    // Voice too: alice's new-epoch frame decodes for bob.
    a.send_audio_frame(&tone(1)).unwrap();
    let log = b.wait_audio_packets(2).await;
    assert!(log[1].accepted.is_ok(), "{:?}", log[1].accepted);
    assert!(c.audio_log().len() <= 1, "carol got post-removal audio");
}

// ---------------------------------------------------------------------------
// 4. Leaving, rejoining, isolation between channels
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channel_isolation_and_rejoin() {
    let server = TestServer::start().await;
    let room1 = server.create_channel("Room 1").await;
    let room2 = server.create_channel("Room 2").await;
    let (mut a, mut b) = two_in_channel(&server, &room1).await;

    let mut d = TestClient::connect(&server, "dave").await;
    d.join_channel(&room2).await;
    d.wait_ready(&room2).await;

    // Text in room 1 reaches bob and not dave.
    a.send_text(&room1, "room one only").await;
    assert_eq!(
        b.next_text().await.message.unwrap().content,
        "room one only"
    );
    d.sync().await;
    sleep(Duration::from_millis(200)).await;
    assert!(!d.has_event(|e| matches!(e, Event::Text(_))));

    // Nor does dave hear room 1's audio.
    a.send_audio_frame(&tone(0)).unwrap();
    b.wait_audio_packets(1).await;
    sleep(Duration::from_millis(100)).await;
    assert!(d.audio_log().is_empty());

    // Bob moves to room 2: alice is told, and the new room works.
    b.join_channel(&room2).await;
    a.wait_event("bob leaving", |e| {
        matches!(e, Event::UserLeft(l) if l.session_id == b.session_id && l.channel_id == room1)
    })
    .await;
    b.wait_ready(&room2).await;
    d.wait_epoch(&room2, false, 1).await;
    b.wait_epoch(&room2, false, 1).await;
    b.send_text(&room2, "now in room two").await;
    assert_eq!(
        d.next_text().await.message.unwrap().content,
        "now in room two"
    );

    // Alice's room-1 text no longer reaches bob (or dave).
    a.send_text(&room1, "nobody here").await;
    b.sync().await;
    d.sync().await;
    sleep(Duration::from_millis(200)).await;
    assert!(!b.has_event(|e| matches!(e, Event::Text(t) if t.packet.channel_id == room1)));
    assert!(!d.has_event(|e| matches!(e, Event::Text(_))));

    // Alice is alone in room 1 now; the removal left it at epoch 2. Bob
    // comes back: a fresh join, a fresh Welcome, epoch 3.
    a.wait_epoch(&room1, false, 2).await;
    b.join_channel(&room1).await;
    b.wait_epoch(&room1, false, 3).await;
    b.wait_epoch(&room1, true, 3).await;
    a.wait_epoch(&room1, false, 3).await;
    a.wait_sees(&room1, b.session_id).await;
    b.wait_sees(&room1, a.session_id).await;

    a.send_text(&room1, "welcome back").await;
    assert_eq!(b.next_text().await.message.unwrap().content, "welcome back");
    b.send_text(&room1, "glad to be back").await;
    assert_eq!(
        a.next_text().await.message.unwrap().content,
        "glad to be back"
    );

    // Joining a channel that doesn't exist is answered with NotFound.
    b.join_channel("C_no_such_channel").await;
    let ev = b
        .wait_event("a not-found notice", |e| {
            matches!(e, Event::ChannelDeleted(_))
        })
        .await;
    let Event::ChannelDeleted(notice) = ev else {
        unreachable!()
    };
    assert_eq!(notice.channel_id, "C_no_such_channel");
    assert_eq!(notice.reason(), ChannelRemovalReason::NotFound);
    assert_eq!(notice.fallback_channel_id, server.lobby_id);
}

// ---------------------------------------------------------------------------
// 5. Moderation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn moderation_list_kick_and_ban() {
    let server = TestServer::start().await;
    let room = server.create_channel("Room").await;

    let mut admin = TestClient::connect_admin(&server, "boss").await;
    assert!(admin.is_admin);
    let mut u1 = TestClient::connect(&server, "ursula").await;
    let u2_identity = IdentitySecret::generate();
    let u2 = TestClient::try_connect(
        &server,
        "ulrich",
        IdentitySecret::from_bytes(&u2_identity.to_bytes()[..]).unwrap(),
    )
    .await
    .expect("login");

    // ListUsers: admins see every account; others are refused.
    let listing = admin.list_users(0, 0).await;
    assert!(listing.success, "{}", listing.error_message);
    assert_eq!(listing.total_count, 3);
    let names: Vec<_> = listing
        .users
        .iter()
        .map(|u| u.display_name.as_str())
        .collect();
    assert_eq!(names, ["boss", "ursula", "ulrich"]);
    assert!(listing.users[0].is_admin && !listing.users[1].is_admin);
    let refused = u1.list_users(0, 0).await;
    assert!(!refused.success && !refused.error_message.is_empty());

    // Kick: refused while the target sits in the lobby...
    let r = admin.kick_user(&u1.user_uuid, "spam").await;
    assert!(!r.success, "kicking someone not in a channel must fail");
    // ...accepted once they are in a regular channel.
    u1.join_channel(&room).await;
    u1.wait_ready(&room).await;
    let r = admin.kick_user(&u1.user_uuid, "spamming the room").await;
    assert!(r.success, "{}", r.error_message);
    let Event::ChannelDeleted(notice) = u1
        .wait_event("kick notice", |e| matches!(e, Event::ChannelDeleted(_)))
        .await
    else {
        unreachable!()
    };
    assert_eq!(notice.reason(), ChannelRemovalReason::Kicked);
    assert_eq!(notice.channel_id, room);
    assert_eq!(notice.detail, "spamming the room");
    assert_eq!(notice.fallback_channel_id, server.lobby_id);
    // A kick doesn't end the session: the user can come back.
    u1.join_channel(&room).await;
    u1.wait_ready(&room).await;

    // A non-admin can't kick or ban.
    assert!(!u1.kick_user(&u2.user_uuid, "x").await.success);
    assert!(!u1.ban_user(&u2.user_uuid, true, "x").await.success);

    // Ban: the target's connection is closed with code 1 "banned"...
    let r = admin.ban_user(&u2.user_uuid, true, "abuse").await;
    assert!(r.success, "{}", r.error_message);
    match u2.wait_closed().await {
        quinn::ConnectionError::ApplicationClosed(close) => {
            assert_eq!(u64::from(close.error_code), 1);
            assert_eq!(&close.reason[..], b"banned");
        }
        other => panic!("expected an application close, got {other:?}"),
    }
    // ...and logging in again is refused with the reason.
    let err = TestClient::try_connect(
        &server,
        "ulrich",
        IdentitySecret::from_bytes(&u2_identity.to_bytes()[..]).unwrap(),
    )
    .await
    .err()
    .expect("banned user logged in");
    let refused = err.downcast_ref::<LoginRefused>().expect("a refused login");
    assert!(refused.0.contains("User is banned"), "{}", refused.0);

    // Lifting the ban lets them back in.
    let r = admin.ban_user(&u2.user_uuid, false, "").await;
    assert!(r.success, "{}", r.error_message);
    TestClient::try_connect(
        &server,
        "ulrich",
        IdentitySecret::from_bytes(&u2_identity.to_bytes()[..]).unwrap(),
    )
    .await
    .expect("unbanned user logs in");
}
