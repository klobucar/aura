//! Identity key, signature, member credential and profile signature parsers:
//! bytes that peers and servers send us (credentials ride inside MLS key
//! packages; profile keys and signatures ride inside `UserProfile`).
#![no_main]

use arbitrary::Arbitrary;
use aura_core::profile::{verify_profile, ProfileFields};
use aura_identity::{
    IdentityPublicKey, IdentitySecret, IdentitySignature, MemberCredential, Purpose,
};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    key: Vec<u8>,
    signature: Vec<u8>,
    credential: Vec<u8>,
    secret: Vec<u8>,
    message: Vec<u8>,
    scheme: u16,
    user_id: String,
    bio: String,
    version: u64,
}

fuzz_target!(|input: Input| {
    let key = IdentityPublicKey::from_bytes(&input.key);
    let sig = IdentitySignature::from_bytes(&input.signature);
    if let (Ok(key), Ok(sig)) = (&key, &sig) {
        let _ = key.verify(Purpose::Auth, &input.message, sig);
    }
    if let Ok(cred) = MemberCredential::from_bytes(&input.credential) {
        let _ = cred.verify(input.scheme, &input.message);
        let _ = cred.user_id();
        let bytes = cred.to_bytes();
        let again = MemberCredential::from_bytes(&bytes).expect("own encoding must parse");
        assert_eq!(again.to_bytes(), bytes);
    }
    let _ = IdentitySecret::from_bytes(&input.secret);

    let fields = ProfileFields {
        user_id: input.user_id.clone(),
        display_name: String::new(),
        bio: input.bio,
        avatar_data: input.message,
        banner_data: Vec::new(),
        version: input.version,
    };
    let _ = verify_profile(&fields, &input.signature, &input.key, &input.user_id);
});
