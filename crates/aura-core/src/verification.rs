//! Pairwise identity verification — safety words and identity fingerprints.
//!
//! Two users compare a short, human-readable value out of band (read it aloud,
//! or over a channel an attacker on this one cannot reach) to prove neither
//! side is being impersonated. Each MLS group member holds an Ed25519 signature
//! keypair whose public half every other member learns from the group itself,
//! so this needs nothing new on the wire.
//!
//! Two distinct values live here, and they answer different questions:
//!
//! - [`key_fingerprint`] — *"which key is this?"* A per-key value, the same for
//!   every viewer. Drives the identity card, roster badges and member list.
//! - [`pairwise_fingerprint`] — *"are you and I seeing the same two keys?"*
//!   Derived from **both** identities and identical on both devices. Only this
//!   one is safe to compare out of band, and only it feeds [`safety_words`].
//!
//! The pairwise construction follows `docs/protocol.md` § Verification
//! Fingerprint. Aura deviates from stock DAVE in three ways, each recorded in
//! `docs/05_dave_protocol_deviations.md`: Ed25519 public keys rather than
//! P-256, UUID strings rather than u64 snowflakes, and BIP-39 words rather
//! than the 45-digit displayable code.
//!
//! **Cost:** [`pairwise_fingerprint`] runs scrypt at N=16384, r=8, p=2 — about
//! 32 MiB and ~100 ms. That memory-hardness is the point: it prices up an
//! attacker grinding keys for a colliding word set. Call it when a user opens
//! the trust sheet, never per frame, per member, or on a render path.

use scrypt::{scrypt, Params};
use sha2::{Digest, Sha256};

/// BIP-39 English wordlist, verified against the canonical
/// `bitcoin/bips/bip-0039/english.txt`
/// (SHA-256 `2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda`).
///
/// Every entry is unique in its first four characters, so a word misheard over
/// the phone is still recoverable. Changing this list silently invalidates
/// every verification a user has already stored — treat it as a protocol
/// constant, not a tunable.
const WORDLIST_RAW: &str = include_str!("bip39_english.txt");

/// Number of words shown in the trust sheet.
pub const SAFETY_WORD_COUNT: usize = 6;

/// Bits consumed per word. 2^11 = 2048 = the wordlist length.
const BITS_PER_WORD: usize = 11;

/// Fingerprint version, mixed in so a future format change cannot be
/// confused with this one. `docs/protocol.md` fixes it at 16 bits of zero.
const FINGERPRINT_VERSION: [u8; 2] = [0x00, 0x00];

/// Fixed scrypt salt from `docs/protocol.md` § Verification Fingerprint.
///
/// A constant salt is correct here and not an oversight: both devices must
/// derive the same value from public inputs alone, so there is nothing
/// per-user to salt with. The salt only domain-separates this construction
/// from other scrypt users.
const SCRYPT_SALT: [u8; 16] = [
    0x24, 0xca, 0xb1, 0x7a, 0x7a, 0xf8, 0xec, 0x2b, 0x82, 0xb4, 0x12, 0xb9, 0x2d, 0xab, 0x19, 0x2e,
];

/// scrypt CPU/memory cost, expressed as log2(N). N = 16384.
const SCRYPT_LOG_N: u8 = 14;
/// scrypt block size parameter.
const SCRYPT_R: u32 = 8;
/// scrypt parallelisation parameter.
const SCRYPT_P: u32 = 2;

/// Length of a pairwise fingerprint, in bytes.
pub const PAIRWISE_FINGERPRINT_LEN: usize = 64;
/// Length of an identity (per-key) fingerprint, in bytes.
pub const KEY_FINGERPRINT_LEN: usize = 32;

/// Ed25519 public keys are 32 bytes. Anything else is not a key we can verify.
const ED25519_PUBLIC_KEY_LEN: usize = 32;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerificationError {
    #[error("public key must be {expected} bytes, got {got}")]
    BadPublicKeyLength { expected: usize, got: usize },

    #[error("user identifier must not be empty")]
    EmptyIdentifier,

    #[error("key derivation failed: {0}")]
    Kdf(String),
}

/// Wordlist as a slice, split once on first use.
fn wordlist() -> &'static [&'static str] {
    use std::sync::OnceLock;
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| WORDLIST_RAW.lines().map(str::trim).collect())
}

/// Per-key identity fingerprint: `SHA-256(version || public_key)`.
///
/// Stable for a given key regardless of who is looking, which is what the
/// roster and member list want. This is *not* the value to compare out of
/// band — a MITM can show each side a fingerprint that matches the key that
/// side actually holds. Use [`pairwise_fingerprint`] for comparison.
pub fn key_fingerprint(public_key: &[u8]) -> Result<[u8; KEY_FINGERPRINT_LEN], VerificationError> {
    check_public_key(public_key)?;

    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_VERSION);
    hasher.update(public_key);

    Ok(hasher.finalize().into())
}

/// Pairwise fingerprint binding both identities, per `docs/protocol.md`.
///
/// ```text
/// bufLocal  = version || local_key  || local_id
/// bufRemote = version || remote_key || remote_id
/// Fn        = scrypt(sort([bufLocal, bufRemote]), salt, N=16384, r=8, p=2, 64)
/// ```
///
/// Sorting the two buffers by unsigned lexicographic order is what makes the
/// result symmetric: both devices concatenate them in the same order without
/// having to agree on who is "first", so both derive identical safety words.
///
/// Expensive by design — see the module docs before calling.
pub fn pairwise_fingerprint(
    local_public_key: &[u8],
    local_id: &str,
    remote_public_key: &[u8],
    remote_id: &str,
) -> Result<[u8; PAIRWISE_FINGERPRINT_LEN], VerificationError> {
    check_public_key(local_public_key)?;
    check_public_key(remote_public_key)?;
    if local_id.is_empty() || remote_id.is_empty() {
        return Err(VerificationError::EmptyIdentifier);
    }

    let local = identity_buffer(local_public_key, local_id);
    let remote = identity_buffer(remote_public_key, remote_id);

    // Unsigned lexicographic order. Rust's Ord for Vec<u8> compares bytes as
    // u8, which is already the unsigned ordering the spec asks for.
    let (first, second) = if local <= remote {
        (local, remote)
    } else {
        (remote, local)
    };

    let mut password = first;
    password.extend_from_slice(&second);

    // Output length is fixed by the `out` buffer below, not by Params.
    let params = Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P)
        .map_err(|e| VerificationError::Kdf(e.to_string()))?;

    let mut out = [0u8; PAIRWISE_FINGERPRINT_LEN];
    scrypt(&password, &SCRYPT_SALT, &params, &mut out)
        .map_err(|e| VerificationError::Kdf(e.to_string()))?;

    Ok(out)
}

/// `version || public_key || identifier`, the per-user half of the pairwise input.
fn identity_buffer(public_key: &[u8], id: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(FINGERPRINT_VERSION.len() + public_key.len() + id.len());
    buf.extend_from_slice(&FINGERPRINT_VERSION);
    buf.extend_from_slice(public_key);
    buf.extend_from_slice(id.as_bytes());
    buf
}

fn check_public_key(public_key: &[u8]) -> Result<(), VerificationError> {
    if public_key.len() != ED25519_PUBLIC_KEY_LEN {
        return Err(VerificationError::BadPublicKeyLength {
            expected: ED25519_PUBLIC_KEY_LEN,
            got: public_key.len(),
        });
    }
    Ok(())
}

/// The six words users read to each other, taken from a pairwise fingerprint.
///
/// Consumes the leading `6 × 11 = 66` bits, most-significant bit first, each
/// 11-bit group indexing the BIP-39 list. 66 bits of agreement is what an
/// attacker would have to forge, on top of scrypt's per-guess cost.
///
/// Only ever pass a [`pairwise_fingerprint`] here. Words derived from a
/// per-key fingerprint would compare equal for both sides of a MITM and prove
/// nothing.
pub fn safety_words(fingerprint: &[u8; PAIRWISE_FINGERPRINT_LEN]) -> [String; SAFETY_WORD_COUNT] {
    let words = wordlist();
    let mut out: Vec<String> = Vec::with_capacity(SAFETY_WORD_COUNT);

    for i in 0..SAFETY_WORD_COUNT {
        let index = read_bits_be(fingerprint, i * BITS_PER_WORD, BITS_PER_WORD) as usize;
        out.push(words[index].to_string());
    }

    out.try_into().expect("collected exactly SAFETY_WORD_COUNT")
}

/// Read `count` bits starting at `bit_offset`, most-significant bit first.
///
/// `count` is at most 11 here, so the accumulator cannot overflow u16.
fn read_bits_be(bytes: &[u8], bit_offset: usize, count: usize) -> u16 {
    debug_assert!(count <= 16);
    debug_assert!((bit_offset + count).div_ceil(8) <= bytes.len());

    let mut acc: u16 = 0;
    for i in 0..count {
        let bit_index = bit_offset + i;
        let byte = bytes[bit_index / 8];
        let bit = (byte >> (7 - (bit_index % 8))) & 1;
        acc = (acc << 1) | u16::from(bit);
    }
    acc
}

/// Group hex into 4-character blocks: `3f8a 91c2 be04 77dd …`.
///
/// The trust sheet renders one of these per identity, and the profile card
/// shows the same string. Grouping is what makes a 64-character hex run
/// checkable by eye.
pub fn format_fingerprint(fingerprint: &[u8]) -> String {
    let hex = hex::encode(fingerprint);
    hex.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("hex is ascii"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Head-and-tail form for tight spots: `3f8a…5503`.
///
/// A truncation, not a security boundary — never compare these to decide
/// trust. It exists so a roster row can hint at identity in a few pixels.
pub fn format_fingerprint_short(fingerprint: &[u8]) -> String {
    let hex = hex::encode(fingerprint);
    if hex.len() <= 8 {
        return hex;
    }
    format!("{}…{}", &hex[..4], &hex[hex.len() - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: [u8; 32] = [0x11; 32];
    const KEY_B: [u8; 32] = [0x22; 32];
    const ID_A: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
    const ID_B: &str = "6ba7b811-9dad-11d1-80b4-00c04fd430c8";

    #[test]
    fn wordlist_is_the_canonical_bip39_list() {
        let words = wordlist();
        assert_eq!(words.len(), 2048, "BIP-39 English list is 2048 words");

        // Anchors from the canonical file — catches a truncated or reordered
        // vendored copy.
        assert_eq!(words[0], "abandon");
        assert_eq!(words[2047], "zoo");

        // Every word unique in its first four characters is the property that
        // makes a misheard word recoverable.
        let mut prefixes: Vec<&str> = words.iter().map(|w| &w[..4.min(w.len())]).collect();
        prefixes.sort_unstable();
        let before = prefixes.len();
        prefixes.dedup();
        assert_eq!(before, prefixes.len(), "4-char prefixes must be unique");

        assert!(
            words
                .iter()
                .all(|w| w.chars().all(|c| c.is_ascii_lowercase())),
            "wordlist must be lowercase ascii"
        );
    }

    #[test]
    fn pairwise_fingerprint_is_symmetric() {
        // The whole design rests on this: each side passes itself first, and
        // both must still land on the same bytes.
        let from_a = pairwise_fingerprint(&KEY_A, ID_A, &KEY_B, ID_B).unwrap();
        let from_b = pairwise_fingerprint(&KEY_B, ID_B, &KEY_A, ID_A).unwrap();

        assert_eq!(
            from_a, from_b,
            "both sides must derive the same fingerprint"
        );
        assert_eq!(safety_words(&from_a), safety_words(&from_b));
    }

    #[test]
    fn different_keys_give_different_words() {
        let baseline = pairwise_fingerprint(&KEY_A, ID_A, &KEY_B, ID_B).unwrap();

        // An impostor with a fresh key but the same display identity.
        let impostor_key = [0x33u8; 32];
        let impostor = pairwise_fingerprint(&KEY_A, ID_A, &impostor_key, ID_B).unwrap();

        assert_ne!(baseline, impostor);
        assert_ne!(safety_words(&baseline), safety_words(&impostor));
    }

    #[test]
    fn identifier_is_bound_into_the_fingerprint() {
        // Same keys, different user ids — must not collide, or a rename would
        // silently inherit someone else's verification.
        let a = pairwise_fingerprint(&KEY_A, ID_A, &KEY_B, ID_B).unwrap();
        let b = pairwise_fingerprint(&KEY_A, ID_A, &KEY_B, "different-uuid").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn safety_words_are_stable_and_in_range() {
        let fp = pairwise_fingerprint(&KEY_A, ID_A, &KEY_B, ID_B).unwrap();
        let words = safety_words(&fp);

        assert_eq!(words.len(), SAFETY_WORD_COUNT);
        let list = wordlist();
        for w in &words {
            assert!(list.contains(&w.as_str()), "{w} is not a BIP-39 word");
        }

        // Deterministic across calls — users re-opening the sheet must not see
        // the words change.
        assert_eq!(words, safety_words(&fp));
    }

    #[test]
    fn read_bits_be_extracts_msb_first() {
        // 0b1010_1010, 0b1100_0000 -> first 11 bits = 0b101_0101_0110 = 1366
        let bytes = [0b1010_1010u8, 0b1100_0000];
        assert_eq!(read_bits_be(&bytes, 0, 11), 0b101_0101_0110);

        // Single bits, walking across the byte boundary.
        assert_eq!(read_bits_be(&bytes, 0, 1), 1);
        assert_eq!(read_bits_be(&bytes, 1, 1), 0);
        assert_eq!(read_bits_be(&bytes, 8, 1), 1);
        assert_eq!(read_bits_be(&bytes, 9, 1), 1);
        assert_eq!(read_bits_be(&bytes, 10, 1), 0);
    }

    #[test]
    fn key_fingerprint_is_per_key_and_stable() {
        let a = key_fingerprint(&KEY_A).unwrap();
        let b = key_fingerprint(&KEY_B).unwrap();

        assert_ne!(a, b);
        assert_eq!(a, key_fingerprint(&KEY_A).unwrap());
        assert_eq!(a.len(), KEY_FINGERPRINT_LEN);
    }

    #[test]
    fn rejects_keys_that_are_not_ed25519_sized() {
        assert_eq!(
            key_fingerprint(&[0u8; 31]),
            Err(VerificationError::BadPublicKeyLength {
                expected: 32,
                got: 31
            })
        );
        assert!(pairwise_fingerprint(&[0u8; 33], ID_A, &KEY_B, ID_B).is_err());
        assert!(pairwise_fingerprint(&KEY_A, ID_A, &[], ID_B).is_err());
    }

    #[test]
    fn rejects_empty_identifiers() {
        assert_eq!(
            pairwise_fingerprint(&KEY_A, "", &KEY_B, ID_B),
            Err(VerificationError::EmptyIdentifier)
        );
        assert_eq!(
            pairwise_fingerprint(&KEY_A, ID_A, &KEY_B, ""),
            Err(VerificationError::EmptyIdentifier)
        );
    }

    #[test]
    fn short_form_head_and_tail_match_the_full_form() {
        // A user reads `6c29…dc7a` on a roster row and checks it against the
        // full fingerprint on the trust sheet. If the elided ends did not come
        // from the same value that check would quietly mean nothing.
        let fp = key_fingerprint(&KEY_B).unwrap();
        let full = format_fingerprint(&fp);
        let short = format_fingerprint_short(&fp);

        let (head, tail) = short.split_once('…').expect("short form elides");
        assert!(full.starts_with(head), "{full} should start with {head}");
        assert!(full.ends_with(tail), "{full} should end with {tail}");

        // 32 bytes renders as 16 groups, which the sheet lays out as two rows
        // of eight.
        assert_eq!(full.split(' ').count(), 16);
    }

    #[test]
    fn formats_fingerprints_for_display() {
        let fp = [0x3f, 0x8a, 0x91, 0xc2, 0xbe, 0x04, 0x77, 0xdd];

        assert_eq!(format_fingerprint(&fp), "3f8a 91c2 be04 77dd");
        assert_eq!(format_fingerprint_short(&fp), "3f8a…77dd");

        // Short form degrades to plain hex rather than producing something
        // misleading when there is nothing to elide.
        assert_eq!(format_fingerprint_short(&[0xab, 0xcd]), "abcd");
    }
}
