//! Inbound MLS messages against a client that has just created a group: a
//! commit, a welcome, or a key package that a peer or the server relays.
//! Each op feeds arbitrary bytes to one of the entry points the clients drive
//! from the 0x50..=0x58 envelopes; the client must return an error, not panic.
#![no_main]

use arbitrary::Arbitrary;
use aura_core::mls::MlsClient;
use aura_identity::IdentitySecret;
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

const GROUP: &[u8] = b"text-v1-fuzz";

#[derive(Arbitrary, Debug)]
enum Op {
    Commit(Vec<u8>),
    Welcome(Vec<u8>),
    AddMember(Vec<u8>),
    /// A commit addressed to a group the client is not in.
    CommitOtherGroup(Vec<u8>),
}

fn identity() -> &'static IdentitySecret {
    static ID: OnceLock<IdentitySecret> = OnceLock::new();
    ID.get_or_init(|| IdentitySecret::from_parts(&[5; 32], &[6; 32]))
}

fuzz_target!(|ops: Vec<Op>| {
    let mut client = MlsClient::new("U_fuzz", identity()).expect("client");
    client.create_group(GROUP).expect("group");
    for op in ops.into_iter().take(8) {
        match op {
            Op::Commit(b) => {
                let _ = client.process_commit(GROUP, &b);
            }
            Op::Welcome(b) => {
                let _ = client.process_welcome(&b);
            }
            Op::AddMember(b) => {
                let _ = client.add_member(GROUP, &b);
            }
            Op::CommitOtherGroup(b) => {
                let _ = client.process_commit(b"voice-v1-other", &b);
            }
        }
        // The group must stay usable whatever was thrown at it.
        let _ = client.epoch(GROUP);
        let _ = client.group_members(GROUP);
    }
});
