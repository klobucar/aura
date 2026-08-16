//! End-to-end text messaging tests across MLS epoch changes.
//!
//! The unit tests in `mls` cover key derivation and retention in isolation.
//! These drive the full path a real client takes — derive a per-sender key,
//! encrypt with DAVE, hand the packet over, derive the key *for the epoch on
//! the packet*, decrypt — through the epoch transitions that happen in a live
//! channel: a member joining, a member leaving, and a PFS ratchet.
//!
//! The bug these exist to prevent: deriving at the group's current epoch
//! instead of the packet's silently fails to decrypt anything in flight across
//! a membership change, and looks fine in any single-client test.

use crate::mls::MlsClient;
use crate::text_crypto::{create_text_message, decrypt_text, encrypt_text};
use aura_protocol::EncryptedTextPacket;

const CHANNEL: &str = "lounge";

/// A participant's view of the text group.
struct Peer {
    mls: MlsClient,
    session_id: u32,
}

impl Peer {
    fn new(name: &str, session_id: u32) -> Self {
        Self {
            mls: MlsClient::new(name).expect("create mls client"),
            session_id,
        }
    }

    /// Encrypt as a real sender does: derive at the current epoch and stamp
    /// that epoch onto the packet.
    fn send(&mut self, group_id: &[u8], content: &str) -> EncryptedTextPacket {
        let (key, epoch) = self
            .mls
            .export_sender_key(group_id, self.session_id)
            .expect("sender key at current epoch");
        let msg = create_text_message("sender-uuid", content, None);
        encrypt_text(&key, epoch, CHANNEL.to_string(), self.session_id, &msg).expect("encrypt text")
    }

    /// Decrypt as a real receiver does: derive for the epoch on the packet,
    /// never the group's current epoch.
    fn receive(&mut self, group_id: &[u8], packet: &EncryptedTextPacket) -> Result<String, String> {
        let key = self
            .mls
            .export_sender_key_at_epoch(group_id, packet.sender_session_id, packet.epoch)
            .map_err(|e| format!("key derivation failed: {e}"))?;
        decrypt_text(&key, packet)
            .map(|m| m.content)
            .map_err(|e| format!("decrypt failed: {e}"))
    }

    fn epoch(&self, group_id: &[u8]) -> u64 {
        self.mls.epoch(group_id).expect("epoch")
    }
}

/// Build a two-member text group. Returns (founder, joiner, group_id).
fn two_member_group(group_id: &[u8]) -> (Peer, Peer) {
    let mut alice = Peer::new("alice", 100);
    let mut bob = Peer::new("bob", 200);

    alice.mls.create_group(group_id).expect("create group");

    let bob_kp = bob.mls.get_key_package_bytes().expect("bob key package");
    let (_commit, welcome) = alice.mls.add_member(group_id, &bob_kp).expect("add bob");
    bob.mls.process_welcome(&welcome).expect("bob joins");

    // Both sides warm keys for every visible member, as the clients do on
    // user-joined and on server snapshot.
    for peer in [&mut alice, &mut bob] {
        for sender in [100u32, 200] {
            peer.mls
                .register_sender(group_id, sender)
                .expect("warm sender key");
        }
    }

    (alice, bob)
}

#[test]
fn text_round_trip_at_same_epoch() {
    let group_id = b"e2e-text-same-epoch";
    let (mut alice, mut bob) = two_member_group(group_id);

    let packet = alice.send(group_id, "jitter buffer at 40 ms feels right");
    assert_eq!(packet.epoch, alice.epoch(group_id));

    let got = bob.receive(group_id, &packet).expect("bob decrypts");
    assert_eq!(got, "jitter buffer at 40 ms feels right");
}

/// The original bug: a message sent before a join is delivered after it. The
/// receiver's epoch has moved on, so it must derive at the packet's epoch.
#[test]
fn message_in_flight_survives_a_join() {
    let group_id = b"e2e-text-join";
    let (mut alice, mut bob) = two_member_group(group_id);

    // Alice sends at the current epoch; the packet is still on the wire.
    let in_flight = alice.send(group_id, "shipping the meter first");
    let sent_at = in_flight.epoch;

    // Charlie joins. Alice commits, Bob applies it — both move on.
    let mut charlie = Peer::new("charlie", 300);
    let charlie_kp = charlie
        .mls
        .get_key_package_bytes()
        .expect("charlie key package");
    let (commit, welcome) = alice
        .mls
        .add_member(group_id, &charlie_kp)
        .expect("add charlie");
    charlie
        .mls
        .process_welcome(&welcome)
        .expect("charlie joins");
    bob.mls
        .process_commit(group_id, &commit)
        .expect("bob applies commit");

    assert!(bob.epoch(group_id) > sent_at, "epoch must have advanced");

    // Bob still decrypts it, because he retained the pre-join key.
    let got = bob.receive(group_id, &in_flight).expect("bob decrypts");
    assert_eq!(got, "shipping the meter first");
}

/// A member who joined *after* a message was sent must not be able to read it.
/// That is MLS forward secrecy, and the retention cache must not undermine it.
#[test]
fn a_new_member_cannot_read_pre_join_messages() {
    let group_id = b"e2e-text-forward-secrecy";
    let (mut alice, _bob) = two_member_group(group_id);

    let before_join = alice.send(group_id, "secret said before charlie arrived");

    let mut charlie = Peer::new("charlie", 300);
    let charlie_kp = charlie
        .mls
        .get_key_package_bytes()
        .expect("charlie key package");
    let (_commit, welcome) = alice
        .mls
        .add_member(group_id, &charlie_kp)
        .expect("add charlie");
    charlie
        .mls
        .process_welcome(&welcome)
        .expect("charlie joins");

    assert!(
        charlie.receive(group_id, &before_join).is_err(),
        "a new member must not decrypt traffic from before they joined"
    );
}

/// A PFS ratchet (empty commit) rotates the group secret. Messages sent before
/// it stay readable; messages after it use fresh keys.
#[test]
fn text_survives_a_pfs_ratchet() {
    let group_id = b"e2e-text-ratchet";
    let (mut alice, mut bob) = two_member_group(group_id);

    let before = alice.send(group_id, "before the ratchet");
    let before_epoch = before.epoch;

    // The founder ratchets, as it does when the server asks it to.
    let commit = alice.mls.self_update(group_id).expect("self update");
    bob.mls
        .process_commit(group_id, &commit)
        .expect("bob applies ratchet");

    assert_eq!(alice.epoch(group_id), before_epoch + 1);
    assert_eq!(bob.epoch(group_id), alice.epoch(group_id));

    // The pre-ratchet message is still readable.
    assert_eq!(
        bob.receive(group_id, &before).expect("decrypt pre-ratchet"),
        "before the ratchet"
    );

    // And a fresh message at the new epoch round-trips too.
    let after = alice.send(group_id, "after the ratchet");
    assert_eq!(after.epoch, before_epoch + 1);
    assert_eq!(
        bob.receive(group_id, &after).expect("decrypt post-ratchet"),
        "after the ratchet"
    );
}

/// Sustained conversation across repeated ratchets — the 5-minute/50-message
/// threshold means a long-lived channel ratchets many times, and each one is a
/// chance for the two sides' key state to drift apart.
#[test]
fn text_survives_repeated_ratchets() {
    let group_id = b"e2e-text-many-ratchets";
    let (mut alice, mut bob) = two_member_group(group_id);

    for round in 0..8 {
        let sent = alice.send(group_id, &format!("message in round {round}"));
        assert_eq!(
            bob.receive(group_id, &sent)
                .unwrap_or_else(|e| panic!("round {round} failed: {e}")),
            format!("message in round {round}")
        );

        // Bob replies, so both directions are exercised at every epoch.
        let reply = bob.send(group_id, &format!("ack {round}"));
        assert_eq!(
            alice
                .receive(group_id, &reply)
                .unwrap_or_else(|e| panic!("round {round} reply failed: {e}")),
            format!("ack {round}")
        );

        let commit = alice.mls.self_update(group_id).expect("self update");
        bob.mls
            .process_commit(group_id, &commit)
            .expect("bob applies ratchet");
    }

    assert_eq!(alice.epoch(group_id), bob.epoch(group_id));
}

/// Both sides must derive the *same* key for a given (sender, epoch) even after
/// several epochs have passed, or replies silently stop decrypting one way.
#[test]
fn both_directions_work_after_a_member_leaves() {
    let group_id = b"e2e-text-leave";

    let mut alice = Peer::new("alice", 100);
    let mut bob = Peer::new("bob", 200);
    let mut charlie = Peer::new("charlie", 300);

    alice.mls.create_group(group_id).expect("create group");

    let bob_kp = bob.mls.get_key_package_bytes().expect("bob kp");
    let (_c1, w1) = alice.mls.add_member(group_id, &bob_kp).expect("add bob");
    bob.mls.process_welcome(&w1).expect("bob joins");

    let charlie_kp = charlie.mls.get_key_package_bytes().expect("charlie kp");
    let (c2, w2) = alice
        .mls
        .add_member(group_id, &charlie_kp)
        .expect("add charlie");
    charlie.mls.process_welcome(&w2).expect("charlie joins");
    bob.mls.process_commit(group_id, &c2).expect("bob applies");

    for peer in [&mut alice, &mut bob, &mut charlie] {
        for sender in [100u32, 200, 300] {
            peer.mls
                .register_sender(group_id, sender)
                .expect("warm key");
        }
    }

    // Charlie speaks, then is removed by the founder.
    let charlies_last = charlie.send(group_id, "one last thing");

    let remove_commit = alice
        .mls
        .remove_member(group_id, 2)
        .expect("remove charlie");
    bob.mls
        .process_commit(group_id, &remove_commit)
        .expect("bob applies removal");

    // Charlie's parting message still decrypts for the remaining members.
    assert_eq!(
        bob.receive(group_id, &charlies_last)
            .expect("bob decrypts charlie's last message"),
        "one last thing"
    );

    // And the survivors can still talk to each other in both directions.
    let a = alice.send(group_id, "did charlie go?");
    assert_eq!(
        bob.receive(group_id, &a).expect("bob decrypts alice"),
        "did charlie go?"
    );

    let b = bob.send(group_id, "yes, epoch bumped");
    assert_eq!(
        alice.receive(group_id, &b).expect("alice decrypts bob"),
        "yes, epoch bumped"
    );
}

/// Once an epoch falls out of the retention window the message is unrecoverable
/// and must report that distinctly, so clients drop it instead of retrying.
#[test]
fn stale_epoch_is_reported_not_silently_wrong() {
    let group_id = b"e2e-text-stale";
    let (mut alice, mut bob) = two_member_group(group_id);

    let ancient = alice.send(group_id, "very old news");

    // Ratchet well past the retention window.
    for _ in 0..crate::mls::EPOCH_KEY_RETENTION + 3 {
        let commit = alice.mls.self_update(group_id).expect("self update");
        bob.mls.process_commit(group_id, &commit).expect("apply");
    }

    let err = bob
        .receive(group_id, &ancient)
        .expect_err("stale epoch must not decrypt");
    assert!(
        err.contains("No retained key for epoch"),
        "expected an epoch-unavailable error, got: {err}"
    );
}
