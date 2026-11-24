//! End-to-end tests for the encrypted file relay over real QUIC connections.
//!
//! The server only ever sees opaque bytes, so these use filler "ciphertext"
//! of the right lengths; the real encryption is covered in aura-core.
//! They live here (not in `tests/`) because the connection test harness is
//! private to this crate.

use super::*;
use crate::file_relay::{FileRelay, RelayLimits};
use aura_protocol::file_transfer::{
    error_code as ec, frame_chunk, FileStreamHeader, FILE_CHUNK_SIZE, FILE_HEADER_FIXED_LEN,
    MAX_FILE_CIPHERTEXT_SIZE,
};

struct Rig {
    server: TestServer,
    state: Arc<ServerState>,
    room: String,
    other: String,
}

fn rig(limits: Option<RelayLimits>) -> Rig {
    let mut state = test_state(None);
    if let Some(l) = limits {
        Arc::get_mut(&mut state).unwrap().file_relay = FileRelay::with_limits(l);
    }
    let room = add_channel(&state, "Room", false);
    let other = add_channel(&state, "Other", false);
    Rig {
        server: start_server(Arc::clone(&state)).unwrap(),
        state,
        room,
        other,
    }
}

async fn login(rig: &Rig, name: &str) -> TestClient {
    let id = aura_identity::IdentitySecret::generate();
    let c = TestClient::login(&rig.server, &id, name).await.unwrap();
    assert!(c.auth.success);
    c
}

async fn join(c: &mut TestClient, channel: &str, nonce: u64) {
    c.send_proto(
        MSG_JOIN_CHANNEL,
        aura_protocol::JoinChannelRequest {
            channel_id: channel.to_string(),
        },
    )
    .await
    .unwrap();
    c.sync(nonce).await.unwrap();
}

/// A header for a file of `size` plaintext bytes.
fn header(channel: &str, size: u64) -> FileStreamHeader {
    FileStreamHeader::for_upload(channel, [7; 16], size).unwrap()
}

fn fake_chunk(h: &FileStreamHeader, i: u32) -> Vec<u8> {
    vec![(i as u8).wrapping_add(0x30); h.chunk_ciphertext_len(i).unwrap()]
}

async fn open_upload(c: &TestClient, h: &FileStreamHeader) -> (SendStream, RecvStream) {
    let (mut s, r) = c.conn.open_bi().await.unwrap();
    s.write_all(&h.encode()).await.unwrap();
    (s, r)
}

async fn send_chunk(s: &mut SendStream, h: &FileStreamHeader, i: u32) {
    // The server may already have stopped us; the status says why.
    let _ = s.write_all(&frame_chunk(&fake_chunk(h, i))).await;
}

/// The server's `[code][recipients]` reply to an upload.
async fn status(r: &mut RecvStream) -> (u32, u32) {
    let mut b = [0u8; 8];
    timeout(Duration::from_secs(10), r.read_exact(&mut b))
        .await
        .expect("no status from the server")
        .expect("status stream failed");
    (
        u32::from_le_bytes(b[..4].try_into().unwrap()),
        u32::from_le_bytes(b[4..].try_into().unwrap()),
    )
}

/// Upload a whole file and return the server's status.
async fn upload_all(c: &TestClient, h: &FileStreamHeader) -> (u32, u32) {
    let (mut s, mut r) = open_upload(c, h).await;
    for i in 0..h.chunk_count {
        send_chunk(&mut s, h, i).await;
    }
    s.finish().unwrap();
    status(&mut r).await
}

#[derive(Debug)]
enum Got {
    Done(FileStreamHeader, Vec<Vec<u8>>),
    Reset(u32),
}

/// What a recipient sees: the next delivery stream, read to its end.
/// `None` if no stream arrives within `wait`.
async fn receive(conn: Connection, wait: Duration) -> Option<Got> {
    let mut recv = timeout(wait, conn.accept_uni()).await.ok()?.unwrap();
    let all = match timeout(Duration::from_secs(20), recv.read_to_end(64 << 20))
        .await
        .expect("delivery stream stalled")
    {
        Ok(b) => b,
        Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(c))) => {
            return Some(Got::Reset(c.into_inner() as u32))
        }
        Err(e) => panic!("delivery failed: {e}"),
    };
    let ch_len = FileStreamHeader::channel_id_len_from_prefix(&all).unwrap();
    let hlen = FILE_HEADER_FIXED_LEN + ch_len;
    let h = FileStreamHeader::decode(&all[..hlen]).unwrap();
    let mut chunks = vec![];
    let mut rest = &all[hlen..];
    while !rest.is_empty() {
        let n = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        chunks.push(rest[4..4 + n].to_vec());
        rest = &rest[4 + n..];
    }
    Some(Got::Done(h, chunks))
}

const SHORT: Duration = Duration::from_millis(600);

#[tokio::test]
async fn relays_to_current_channel_members_only() {
    let _serial = LOGIN_TESTS.lock().await;
    let m = &crate::metrics::METRICS;
    let (started0, done0) = (
        m.file_transfers_started_total(),
        m.file_transfers_finished_with("completed"),
    );
    let rig = rig(None);
    let (mut alice, mut bob, mut carol, mut dave, erin) = (
        login(&rig, "Alice").await,
        login(&rig, "Bob").await,
        login(&rig, "Carol").await,
        login(&rig, "Dave").await,
        login(&rig, "Erin").await,
    );
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;
    join(&mut carol, &rig.room, 1).await;
    join(&mut dave, &rig.other, 1).await; // other channel
                                          // Erin is connected but in no channel.

    let h = header(&rig.room, 3 * FILE_CHUNK_SIZE as u64 + 5);
    assert_eq!(h.chunk_count, 4);
    let (bc, cc, dc, ec_) = (
        bob.conn.clone(),
        carol.conn.clone(),
        dave.conn.clone(),
        erin.conn.clone(),
    );
    let rb = tokio::spawn(receive(bc, TEST_TIMEOUT));
    let rc = tokio::spawn(receive(cc, TEST_TIMEOUT));
    let rd = tokio::spawn(receive(dc, SHORT));
    let re = tokio::spawn(receive(ec_, SHORT));

    // Frank joins after the transfer has started and must not get it.
    let (mut s, mut r) = open_upload(&alice, &h).await;
    send_chunk(&mut s, &h, 0).await;
    let mut frank = login(&rig, "Frank").await;
    let frank_conn = frank.conn.clone();
    join(&mut frank, &rig.room, 1).await;
    let rf = tokio::spawn(receive(frank_conn, SHORT));
    for i in 1..h.chunk_count {
        send_chunk(&mut s, &h, i).await;
    }
    s.finish().unwrap();
    assert_eq!(status(&mut r).await, (0, 2), "relayed to Bob and Carol");

    for got in [rb.await.unwrap(), rc.await.unwrap()] {
        let Some(Got::Done(rh, chunks)) = got else {
            panic!("member did not receive the file: {got:?}");
        };
        assert_eq!(rh.sender_session_id, alice.auth.session_id);
        assert_eq!(rh.file_id, h.file_id);
        assert_eq!(rh.channel_id, rig.room);
        assert_eq!(rh.total_ciphertext_len, h.total_ciphertext_len);
        assert_eq!(chunks.len(), 4);
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(*c, fake_chunk(&h, i as u32), "chunk {i} relayed unchanged");
        }
    }
    for (who, got) in [
        ("other channel", rd.await.unwrap()),
        ("no channel", re.await.unwrap()),
        ("late joiner", rf.await.unwrap()),
    ] {
        assert!(got.is_none(), "{who} must not receive the file: {got:?}");
    }

    assert_eq!(m.file_transfers_started_total(), started0 + 1);
    assert_eq!(m.file_transfers_finished_with("completed"), done0 + 1);
    assert_eq!(rig.state.file_relay.active_transfers(), 0);
}

#[tokio::test]
async fn empty_file_and_exact_chunk_multiple_relay() {
    let _serial = LOGIN_TESTS.lock().await;
    let rig = rig(None);
    let (mut alice, mut bob) = (login(&rig, "Alice").await, login(&rig, "Bob").await);
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;
    for size in [0u64, 2 * FILE_CHUNK_SIZE as u64] {
        let h = header(&rig.room, size);
        let rb = tokio::spawn(receive(bob.conn.clone(), TEST_TIMEOUT));
        assert_eq!(upload_all(&alice, &h).await, (0, 1), "size {size}");
        let Some(Got::Done(_, chunks)) = rb.await.unwrap() else {
            panic!("size {size} not delivered");
        };
        assert_eq!(chunks.len() as u32, h.chunk_count);
    }
}

#[tokio::test]
async fn refuses_declared_size_over_the_cap() {
    let _serial = LOGIN_TESTS.lock().await;
    let m = &crate::metrics::METRICS;
    let big0 = m.file_transfers_rejected_with("too_large");
    let rig = rig(None);
    let (mut alice, mut bob) = (login(&rig, "Alice").await, login(&rig, "Bob").await);
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;
    let rb = tokio::spawn(receive(bob.conn.clone(), SHORT));

    // Built by hand: the codec's own constructor refuses to.
    let h = FileStreamHeader {
        sender_session_id: 0,
        channel_id: rig.room.clone(),
        file_id: [1; 16],
        total_ciphertext_len: MAX_FILE_CIPHERTEXT_SIZE + 1,
        chunk_count: 401,
    };
    // The stream stays open: the refusal must not depend on the sender finishing.
    let (mut s, mut r) = alice.conn.open_bi().await.unwrap();
    s.write_all(&h.encode()).await.unwrap();
    assert_eq!(status(&mut r).await.0, ec::TOO_LARGE);
    assert!(rb.await.unwrap().is_none());
    assert_eq!(m.file_transfers_rejected_with("too_large"), big0 + 1);
}

#[tokio::test]
async fn relays_a_file_exactly_at_the_cap() {
    let _serial = LOGIN_TESTS.lock().await;
    let rig = rig(None);
    let (mut alice, mut bob) = (login(&rig, "Alice").await, login(&rig, "Bob").await);
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;
    let h = header(
        &rig.room,
        aura_protocol::file_transfer::max_plaintext_size(),
    );
    assert_eq!(h.total_ciphertext_len, MAX_FILE_CIPHERTEXT_SIZE);
    let rb = tokio::spawn(receive(bob.conn.clone(), Duration::from_secs(20)));
    assert_eq!(upload_all(&alice, &h).await, (0, 1));
    let Some(Got::Done(_, chunks)) = rb.await.unwrap() else {
        panic!("max-size file not delivered");
    };
    assert_eq!(chunks.len() as u32, h.chunk_count);
}

#[tokio::test]
async fn refuses_a_sender_who_is_not_in_the_channel() {
    let _serial = LOGIN_TESTS.lock().await;
    let rig = rig(None);
    let (mut alice, mut bob, mut mallory) = (
        login(&rig, "Alice").await,
        login(&rig, "Bob").await,
        login(&rig, "Mallory").await,
    );
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;
    join(&mut mallory, &rig.other, 1).await; // in a different channel

    let rb = tokio::spawn(receive(bob.conn.clone(), SHORT));
    let h = header(&rig.room, 1000);
    assert_eq!(upload_all(&mallory, &h).await.0, ec::NOT_IN_CHANNEL);
    assert!(rb.await.unwrap().is_none(), "Bob must see nothing");

    // Nor an unknown channel, nor a connected user in no channel at all.
    let h = header("no-such-channel", 1000);
    assert_eq!(upload_all(&alice, &h).await.0, ec::NOT_IN_CHANNEL);
    let nowhere = login(&rig, "Nowhere").await;
    assert_eq!(
        upload_all(&nowhere, &header(&rig.room, 10)).await.0,
        ec::NOT_IN_CHANNEL
    );
}

#[tokio::test]
async fn refuses_malformed_headers_and_empty_channels() {
    let _serial = LOGIN_TESTS.lock().await;
    let rig = rig(None);
    let (mut alice, mut bob) = (login(&rig, "Alice").await, login(&rig, "Bob").await);
    join(&mut alice, &rig.room, 1).await;

    // Garbage first byte.
    let (mut s, mut r) = alice.conn.open_bi().await.unwrap();
    s.write_all(&[0xFF; 64]).await.unwrap();
    assert_eq!(status(&mut r).await.0, ec::MALFORMED_HEADER);

    // Sizes that disagree with the chunk count.
    let mut h = header(&rig.room, 100);
    h.chunk_count = 9;
    let (mut s, mut r) = alice.conn.open_bi().await.unwrap();
    s.write_all(&h.encode()).await.unwrap();
    assert_eq!(status(&mut r).await.0, ec::MALFORMED_HEADER);

    // Alone in the channel: nobody to relay to.
    assert_eq!(
        upload_all(&alice, &header(&rig.room, 100)).await.0,
        ec::NO_RECIPIENTS
    );
    join(&mut bob, &rig.room, 1).await;
    assert_eq!(upload_all(&alice, &header(&rig.room, 100)).await, (0, 1));
}

#[tokio::test]
async fn aborts_when_the_sender_sends_more_than_declared() {
    let _serial = LOGIN_TESTS.lock().await;
    let rig = rig(None);
    let (mut alice, mut bob) = (login(&rig, "Alice").await, login(&rig, "Bob").await);
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;

    // Extra data after the declared chunks.
    let rb = tokio::spawn(receive(bob.conn.clone(), TEST_TIMEOUT));
    let h = header(&rig.room, 100);
    let (mut s, mut r) = open_upload(&alice, &h).await;
    send_chunk(&mut s, &h, 0).await;
    s.write_all(&[1, 2, 3]).await.unwrap();
    assert_eq!(status(&mut r).await.0, ec::PROTOCOL_VIOLATION);
    assert_eq!(
        format!("{:?}", rb.await.unwrap().unwrap()),
        format!("{:?}", Got::Reset(ec::PROTOCOL_VIOLATION)),
        "recipients are told to discard it"
    );

    // A chunk whose length prefix disagrees with the header.
    let rb = tokio::spawn(receive(bob.conn.clone(), TEST_TIMEOUT));
    let h = header(&rig.room, 2 * FILE_CHUNK_SIZE as u64);
    let (mut s, mut r) = open_upload(&alice, &h).await;
    send_chunk(&mut s, &h, 0).await;
    s.write_all(&frame_chunk(&[0u8; 100])).await.unwrap();
    assert_eq!(status(&mut r).await.0, ec::PROTOCOL_VIOLATION);
    assert!(matches!(
        rb.await.unwrap().unwrap(),
        Got::Reset(c) if c == ec::PROTOCOL_VIOLATION
    ));

    // Fewer chunks than declared, then a clean finish.
    let rb = tokio::spawn(receive(bob.conn.clone(), TEST_TIMEOUT));
    let h = header(&rig.room, 2 * FILE_CHUNK_SIZE as u64);
    let (mut s, mut r) = open_upload(&alice, &h).await;
    send_chunk(&mut s, &h, 0).await;
    s.finish().unwrap();
    assert_ne!(status(&mut r).await.0, 0);
    assert!(matches!(rb.await.unwrap().unwrap(), Got::Reset(_)));
}

#[tokio::test]
async fn aborts_when_the_sender_leaves_the_channel() {
    let _serial = LOGIN_TESTS.lock().await;
    let m = &crate::metrics::METRICS;
    let left0 = m.file_transfers_finished_with("sender_left");
    let rig = rig(None);
    let (mut alice, mut bob) = (login(&rig, "Alice").await, login(&rig, "Bob").await);
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;

    let rb = tokio::spawn(receive(bob.conn.clone(), TEST_TIMEOUT));
    let h = header(&rig.room, 3 * FILE_CHUNK_SIZE as u64);
    let (mut s, mut r) = open_upload(&alice, &h).await;
    send_chunk(&mut s, &h, 0).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    join(&mut alice, &rig.other, 2).await; // moves away mid-transfer

    assert_eq!(status(&mut r).await.0, ec::SENDER_LEFT);
    assert!(matches!(
        rb.await.unwrap().unwrap(),
        Got::Reset(c) if c == ec::SENDER_LEFT
    ));
    drop(s);
    assert!(m.file_transfers_finished_with("sender_left") > left0);
    assert_eq!(rig.state.file_relay.active_transfers(), 0);
}

#[tokio::test]
async fn aborts_when_the_sender_disconnects() {
    let _serial = LOGIN_TESTS.lock().await;
    let rig = rig(None);
    let (mut alice, mut bob) = (login(&rig, "Alice").await, login(&rig, "Bob").await);
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;

    let rb = tokio::spawn(receive(bob.conn.clone(), TEST_TIMEOUT));
    let h = header(&rig.room, 3 * FILE_CHUNK_SIZE as u64);
    let (mut s, _r) = open_upload(&alice, &h).await;
    send_chunk(&mut s, &h, 0).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    alice.close();

    assert!(matches!(
        rb.await.unwrap().unwrap(),
        Got::Reset(c) if c == ec::SENDER_LEFT
    ));
    for _ in 0..50 {
        if rig.state.file_relay.active_transfers() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("transfer still active after the sender disconnected");
}

#[tokio::test]
async fn aborts_a_stalled_sender() {
    let _serial = LOGIN_TESTS.lock().await;
    let rig = rig(Some(RelayLimits {
        chunk_idle_timeout: Duration::from_millis(300),
        ..Default::default()
    }));
    let (mut alice, mut bob) = (login(&rig, "Alice").await, login(&rig, "Bob").await);
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;
    let rb = tokio::spawn(receive(bob.conn.clone(), TEST_TIMEOUT));
    let h = header(&rig.room, 2 * FILE_CHUNK_SIZE as u64);
    let (mut s, mut r) = open_upload(&alice, &h).await;
    send_chunk(&mut s, &h, 0).await;
    // ...and then nothing.
    assert_eq!(status(&mut r).await.0, ec::SENDER_STALLED);
    assert!(matches!(rb.await.unwrap().unwrap(), Got::Reset(_)));
    drop(s);
}

#[tokio::test]
async fn a_stalled_recipient_loses_its_copy_without_blocking_others() {
    let _serial = LOGIN_TESTS.lock().await;
    let m = &crate::metrics::METRICS;
    let dropped0 = m.file_transfers_finished_with("completed");
    let rig = rig(Some(RelayLimits {
        recipient_buffer_chunks: 32,
        ..Default::default()
    }));
    let (mut alice, mut bob, mut carol) = (
        login(&rig, "Alice").await,
        login(&rig, "Bob").await,
        login(&rig, "Carol").await,
    );
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;
    join(&mut carol, &rig.room, 1).await;

    // Bob accepts the stream and then never reads it.
    let bob_conn = bob.conn.clone();
    let slow = tokio::spawn(async move { bob_conn.accept_uni().await.unwrap() });
    let rc = tokio::spawn(receive(carol.conn.clone(), Duration::from_secs(10)));

    let size = 16 << 20; // far more than Bob's queue plus QUIC's flow control window
    let h = header(&rig.room, size);
    let started = std::time::Instant::now();
    let (code, delivered) = upload_all(&alice, &h).await;
    assert_eq!(code, 0, "the sender's transfer succeeds");
    assert_eq!(delivered, 1, "only Carol still has a live copy");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the sender was held up by the slow recipient"
    );

    let Some(Got::Done(_, chunks)) = rc.await.unwrap() else {
        panic!("Carol did not get the whole file");
    };
    assert_eq!(chunks.len() as u32, h.chunk_count);

    // Bob's stream is reset, not completed.
    let mut bob_stream = slow.await.unwrap();
    let r = timeout(Duration::from_secs(10), bob_stream.read_to_end(64 << 20))
        .await
        .unwrap();
    assert!(
        matches!(
            r,
            Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(c)))
                if c.into_inner() as u32 == ec::RECIPIENT_TOO_SLOW
        ),
        "{r:?}"
    );
    assert!(m.file_transfers_finished_with("completed") > dropped0);
}

#[tokio::test]
async fn a_recipient_that_stops_reading_after_the_upload_cannot_hold_the_slot() {
    let _serial = LOGIN_TESTS.lock().await;
    // Bob's queue holds the whole file, so he is never cut off for falling
    // behind during the upload, and his writes may block for a long time:
    // only the drain deadline can release the sender.
    let rig = rig(Some(RelayLimits {
        recipient_buffer_chunks: 512,
        recipient_write_timeout: Duration::from_secs(60),
        drain_timeout: Duration::from_millis(500),
        ..Default::default()
    }));
    let (mut alice, mut bob, mut carol) = (
        login(&rig, "Alice").await,
        login(&rig, "Bob").await,
        login(&rig, "Carol").await,
    );
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;
    join(&mut carol, &rig.room, 1).await;

    // Bob accepts the stream and then never reads it.
    let bob_conn = bob.conn.clone();
    let slow = tokio::spawn(async move { bob_conn.accept_uni().await.unwrap() });
    let rc = tokio::spawn(receive(carol.conn.clone(), Duration::from_secs(10)));

    let size = 16 << 20; // more than QUIC's flow-control window, less than Bob's queue
    let h = header(&rig.room, size);
    let started = std::time::Instant::now();
    let (code, delivered) = upload_all(&alice, &h).await;
    assert_eq!(code, 0, "the sender's transfer succeeds");
    assert_eq!(delivered, 1, "only Carol's copy completed");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the sender waited on the stalled recipient"
    );
    assert_eq!(rig.state.file_relay.active_transfers(), 0, "slot released");

    let Some(Got::Done(_, chunks)) = rc.await.unwrap() else {
        panic!("Carol did not get the whole file");
    };
    assert_eq!(chunks.len() as u32, h.chunk_count);

    let mut bob_stream = slow.await.unwrap();
    let r = timeout(Duration::from_secs(10), bob_stream.read_to_end(64 << 20))
        .await
        .unwrap();
    assert!(
        matches!(
            r,
            Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(c)))
                if c.into_inner() as u32 == ec::RECIPIENT_TOO_SLOW
        ),
        "{r:?}"
    );
}

#[tokio::test]
async fn limits_concurrent_transfers_and_bytes_per_user() {
    let _serial = LOGIN_TESTS.lock().await;
    let rig = rig(Some(RelayLimits {
        max_concurrent_per_user: 2,
        kib_per_minute_per_user: 300,
        ..Default::default()
    }));
    let (mut alice, mut bob, mut carol) = (
        login(&rig, "Alice").await,
        login(&rig, "Bob").await,
        login(&rig, "Carol").await,
    );
    join(&mut alice, &rig.room, 1).await;
    join(&mut bob, &rig.room, 1).await;
    join(&mut carol, &rig.room, 1).await;
    let _drain = tokio::spawn({
        let c = bob.conn.clone();
        async move {
            while let Ok(mut r) = c.accept_uni().await {
                tokio::spawn(async move { r.read_to_end(64 << 20).await });
            }
        }
    });

    // Two transfers held open (one chunk of two sent) fill the slots: 2 x 128 KiB.
    let h = header(&rig.room, 2 * FILE_CHUNK_SIZE as u64);
    let (mut s1, _r1) = open_upload(&alice, &h).await;
    let (mut s2, _r2) = open_upload(&alice, &h).await;
    send_chunk(&mut s1, &h, 0).await;
    send_chunk(&mut s2, &h, 0).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.state.file_relay.active_transfers(), 2);

    // A third is refused...
    let small = header(&rig.room, 10);
    assert_eq!(upload_all(&alice, &small).await.0, ec::RATE_LIMITED);
    // ...but the limit is per user: Carol is unaffected.
    assert_eq!(upload_all(&carol, &small).await.0, 0);

    // Finish both; the slots free up.
    for (s, i) in [(&mut s1, 1), (&mut s2, 1)] {
        send_chunk(s, &h, i).await;
        s.finish().unwrap();
    }
    for _ in 0..50 {
        if rig.state.file_relay.active_transfers() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Byte budget: 300 KiB per minute, and the two 128 KiB transfers plus
    // Carol's tiny one (a user's own bucket) leave Alice 44 KiB: a 128 KiB
    // transfer no longer fits, while one chunk-minimum (64 KiB) doesn't either.
    assert_eq!(upload_all(&alice, &h).await.0, ec::RATE_LIMITED);
    assert_eq!(upload_all(&alice, &small).await.0, ec::RATE_LIMITED);
}
