//! The UniFFI `decode_*` functions both clients call on server bytes. The
//! first byte picks the function; the rest is the payload.
#![no_main]

use aura_core::uniffi_bindings::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&kind, payload)) = data.split_first() else {
        return;
    };
    let payload = payload.to_vec();
    match kind % 13 {
        0 => drop(decode_server_state(payload)),
        1 => drop(decode_user_profile(payload)),
        2 => drop(decode_user_status_update(payload)),
        3 => drop(decode_user_joined(payload)),
        4 => drop(decode_user_left(payload)),
        5 => drop(decode_mls_sync_members(payload)),
        6 => drop(decode_channel_deleted(payload)),
        7 => drop(decode_create_channel_response(payload)),
        8 => drop(decode_metadata_update_response(payload)),
        9 => drop(decode_admin_response(payload)),
        10 => drop(decode_encrypted_text_packet(payload)),
        11 => drop(decode_mls_envelope(payload)),
        _ => drop(decode_list_users_response(payload)),
    }
});
