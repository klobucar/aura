//! Server login: an arbitrary `AuthRequest` payload, and a correctly signed
//! login under an arbitrary display name.
//!
//! Signature verification runs before display-name validation, so a fuzzer
//! cannot reach the name rules with forged signatures. The structured half of
//! the input therefore signs the transcript for the fuzzed name with a real
//! identity, optionally corrupting the signature or the challenge afterwards.
#![no_main]

use arbitrary::Arbitrary;
use aura_identity::{auth_transcript, IdentitySecret, Purpose, AUTH_CHALLENGE_LEN};
use aura_server::auth::AuthService;
use aura_server::config::Config;
use aura_server::db::Database;
use libfuzzer_sys::fuzz_target;
use prost::Message;
use std::sync::{Arc, OnceLock};

const CHALLENGE: [u8; AUTH_CHALLENGE_LEN] = [9; AUTH_CHALLENGE_LEN];

fn identity() -> &'static IdentitySecret {
    static ID: OnceLock<IdentitySecret> = OnceLock::new();
    ID.get_or_init(|| IdentitySecret::from_parts(&[1; 32], &[2; 32]))
}

#[derive(Arbitrary, Debug)]
struct Login {
    display_name: String,
    password: Option<String>,
    flip_signature_byte: Option<u16>,
    short_challenge: bool,
}

fn service() -> AuthService {
    AuthService::new(
        Arc::new(Database::open_in_memory().unwrap()),
        Config::default(),
    )
}

fuzz_target!(|data: &[u8]| {
    // Raw bytes as the frame body connection.rs decodes.
    if let Ok(req) = aura_protocol::AuthRequest::decode(data) {
        let _ = service().authenticate(
            &req.identity_public_key,
            &req.display_name,
            &req.signature,
            &req.challenge,
            req.server_password.as_deref(),
        );
    }

    // A genuinely signed login under a fuzzed name.
    if let Ok(login) = Login::arbitrary(&mut arbitrary::Unstructured::new(data)) {
        let id = identity();
        let pk = id.public_key();
        let mut sig = id
            .sign(
                Purpose::Auth,
                &auth_transcript(&CHALLENGE, &login.display_name, &pk),
            )
            .as_bytes()
            .to_vec();
        if let Some(i) = login.flip_signature_byte {
            let i = i as usize % sig.len();
            sig[i] ^= 1;
        }
        let challenge: &[u8] = if login.short_challenge {
            &CHALLENGE[1..]
        } else {
            &CHALLENGE
        };
        let auth = service();
        let first = auth.authenticate(
            pk.as_bytes(),
            &login.display_name,
            &sig,
            challenge,
            login.password.as_deref(),
        );
        // Logging in again as the same identity must not change its fate
        // unless the first attempt created the account.
        if let Ok(r) = first {
            let again = auth
                .authenticate(
                    pk.as_bytes(),
                    &login.display_name,
                    &sig,
                    challenge,
                    login.password.as_deref(),
                )
                .expect("second login of an accepted identity");
            assert_eq!(again.user_uuid, r.user_uuid);
        }
    }
});
