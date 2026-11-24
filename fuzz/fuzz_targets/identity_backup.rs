//! Encrypted identity backup import with an arbitrary file and passphrase.
//!
//! scrypt's cost comes from the file's own header (log2 N 10..=20, r up to 16,
//! p up to 4: gigabytes and seconds), which would leave the fuzzer idle. When
//! the header asks for parameters the importer accepts, they are rewritten to
//! the cheapest accepted ones before the call. Headers outside the accepted
//! range are passed through untouched, so the "unreasonable parameters" path
//! is still exercised; nothing in the importer is changed for fuzzing.
//! The AEAD tag cannot be forged, so this covers parsing and validation up to
//! the decrypt, not the plaintext handling after it.
#![no_main]

use arbitrary::Arbitrary;
use aura_core::identity_backup::decrypt_identity_backup;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    backup: Vec<u8>,
    passphrase: String,
}

fuzz_target!(|input: Input| {
    let mut backup = input.backup;
    // magic(7) version(1) log_n(1) r(4) p(4)
    if backup.len() >= 17 {
        let log_n = backup[8];
        let r = u32::from_be_bytes(backup[9..13].try_into().unwrap());
        let p = u32::from_be_bytes(backup[13..17].try_into().unwrap());
        if (10..=20).contains(&log_n) && (1..=16).contains(&r) && (1..=4).contains(&p) {
            backup[8] = 10;
            backup[9..13].copy_from_slice(&1u32.to_be_bytes());
            backup[13..17].copy_from_slice(&1u32.to_be_bytes());
        }
    }
    let _ = decrypt_identity_backup(backup, input.passphrase);
});
