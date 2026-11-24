//! Every protobuf message the server or a client decodes from the wire, plus
//! the fixed-layout audio datagram header.
//!
//! The first byte picks the message type; the rest is the payload. A payload
//! that decodes must re-encode to something that decodes to the same bytes.
#![no_main]

use aura_protocol::*;
use libfuzzer_sys::fuzz_target;
use prost::Message;

macro_rules! roundtrip {
    ($ty:ty, $payload:expr) => {{
        if let Ok(msg) = <$ty>::decode($payload) {
            let once = msg.encode_to_vec();
            let again = <$ty>::decode(once.as_slice())
                .expect("a message we encoded must decode")
                .encode_to_vec();
            assert_eq!(once, again, "re-encoding is not stable");
        }
    }};
}

fuzz_target!(|data: &[u8]| {
    let Some((&kind, payload)) = data.split_first() else {
        return;
    };
    match kind % 40 {
        0 => roundtrip!(AuthRequest, payload),
        1 => roundtrip!(AuthResponse, payload),
        2 => roundtrip!(JoinChannelRequest, payload),
        3 => roundtrip!(EncryptedTextPacket, payload),
        4 => roundtrip!(CreateChannelRequest, payload),
        5 => roundtrip!(UpdateChannelRequest, payload),
        6 => roundtrip!(UpdateProfile, payload),
        7 => roundtrip!(DeleteChannelRequest, payload),
        8 => roundtrip!(DeleteUserRequest, payload),
        9 => roundtrip!(UserStatusUpdate, payload),
        10 => roundtrip!(ListUsersRequest, payload),
        11 => roundtrip!(VerifyUserRequest, payload),
        12 => roundtrip!(BanUserRequest, payload),
        13 => roundtrip!(GrantAdminRequest, payload),
        14 => roundtrip!(KickUserRequest, payload),
        15 => roundtrip!(MlsEnvelope, payload),
        16 => roundtrip!(MlsCommitWelcome, payload),
        17 => roundtrip!(MlsSyncMembers, payload),
        18 => roundtrip!(ServerState, payload),
        19 => roundtrip!(ChannelDeleted, payload),
        20 => roundtrip!(ListUsersResponse, payload),
        21 => roundtrip!(AdminResponse, payload),
        22 => roundtrip!(UserProfile, payload),
        23 => roundtrip!(UserJoined, payload),
        24 => roundtrip!(UserLeft, payload),
        25 => roundtrip!(TextMessage, payload),
        26 => roundtrip!(CreateChannelResponse, payload),
        27 => roundtrip!(MetadataUpdateResponse, payload),
        28 => roundtrip!(ChannelIcon, payload),
        29 => roundtrip!(GetProfile, payload),
        30 => roundtrip!(ChallengeResponse, payload),
        31 => roundtrip!(ChallengeRequest, payload),
        32 => roundtrip!(AudioPacket, payload),
        33 => roundtrip!(Header, payload),
        34 => roundtrip!(Position, payload),
        35 => roundtrip!(JoinChannelGroups, payload),
        36 => roundtrip!(ChannelGroupsWelcome, payload),
        37 => roundtrip!(MlsGroupInfo, payload),
        _ => {
            // Audio datagram header: parse, then write must reproduce the input.
            let bytes = bytes::Bytes::copy_from_slice(payload);
            if let Ok(packet) = FastAudioPacket::parse(bytes) {
                assert_eq!(packet.to_bytes().as_ref(), payload);
            }
        }
    }
});
