//! Tests for end-to-end encrypted file transfer chunking.

use crate::file_transfer::*;
use sha2::{Digest, Sha256};

const C: usize = FILE_CHUNK_SIZE;

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn sha(d: &[u8]) -> [u8; 32] {
    Sha256::digest(d).into()
}

fn encryptor(d: &[u8]) -> FileEncryptor {
    FileEncryptor::with_key_and_id(
        [0x11; 32],
        [0x22; 16],
        "a.bin",
        "application/octet-stream",
        d.len() as u64,
        sha(d),
    )
    .unwrap()
}

/// Encrypt a whole file into its chunk ciphertexts.
fn seal(d: &[u8]) -> (FileManifest, Vec<Vec<u8>>) {
    let mut enc = encryptor(d);
    let mut out = vec![];
    let mut rest = d;
    while let Some(n) = enc.next_plaintext_len() {
        let (head, tail) = rest.split_at(n);
        out.push(enc.encrypt_next(head).unwrap());
        rest = tail;
    }
    enc.finish().unwrap();
    (enc.manifest().clone(), out)
}

fn open(m: &FileManifest, chunks: &[Vec<u8>]) -> Result<Vec<u8>, FileError> {
    let mut dec = FileDecryptor::new(m.clone())?;
    let mut out = vec![];
    for c in chunks {
        out.extend(dec.decrypt_next(c)?);
    }
    dec.finish()?;
    Ok(out)
}

fn max_plaintext() -> u64 {
    max_file_plaintext_size()
}

#[test]
fn round_trip_various_sizes() {
    for n in [0, 1, 100, C - 1, C, C + 1, 3 * C, 3 * C + 17] {
        let d = data(n);
        let (m, chunks) = seal(&d);
        assert_eq!(m.chunk_count as usize, chunks.len(), "size {n}");
        assert_eq!(open(&m, &chunks).unwrap(), d, "size {n}");
    }
}

#[test]
fn empty_file_is_one_tag_only_chunk() {
    let (m, chunks) = seal(&[]);
    assert_eq!(m.chunk_count, 1);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].len(), 16);
}

#[test]
fn exact_multiple_has_no_empty_trailing_chunk() {
    let (m, chunks) = seal(&data(2 * C));
    assert_eq!(m.chunk_count, 2);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[1].len(), C + 16);
}

#[test]
fn flipped_byte_is_rejected() {
    let d = data(2 * C + 5);
    let (m, mut chunks) = seal(&d);
    chunks[1][100] ^= 1;
    assert_eq!(open(&m, &chunks), Err(FileError::Tampered));
    // Flipping a tag byte too.
    let (m, mut chunks) = seal(&d);
    let last = chunks[2].len() - 1;
    chunks[2][last] ^= 0x80;
    assert_eq!(open(&m, &chunks), Err(FileError::Tampered));
}

#[test]
fn swapped_chunks_are_rejected() {
    let (m, mut chunks) = seal(&data(3 * C));
    chunks.swap(0, 1);
    assert_eq!(open(&m, &chunks), Err(FileError::Tampered));
}

#[test]
fn dropped_last_chunk_is_rejected() {
    let (m, mut chunks) = seal(&data(2 * C + 9));
    chunks.pop();
    assert_eq!(open(&m, &chunks), Err(FileError::Truncated));
}

#[test]
fn dropped_middle_chunk_is_rejected() {
    let (m, mut chunks) = seal(&data(3 * C));
    chunks.remove(1);
    // The last chunk now arrives at index 1 and fails authentication.
    assert_eq!(open(&m, &chunks), Err(FileError::Tampered));
}

#[test]
fn extra_chunk_is_rejected() {
    let (m, mut chunks) = seal(&data(C + 1));
    let dup = chunks[0].clone();
    chunks.push(dup);
    assert_eq!(open(&m, &chunks), Err(FileError::TooManyChunks));
}

#[test]
fn chunk_from_another_file_is_rejected() {
    let d = data(2 * C);
    let (m, mut chunks) = seal(&d);
    // Same key, different file id.
    let mut other =
        FileEncryptor::with_key_and_id([0x11; 32], [0x33; 16], "b", "x/y", d.len() as u64, sha(&d))
            .unwrap();
    chunks[0] = other.encrypt_next(&d[..C]).unwrap();
    assert_eq!(open(&m, &chunks), Err(FileError::Tampered));
}

#[test]
fn wrong_key_is_rejected() {
    let (mut m, chunks) = seal(&data(100));
    m.file_key = vec![0x99; 32];
    assert_eq!(open(&m, &chunks), Err(FileError::Tampered));
}

#[test]
fn hash_mismatch_is_rejected() {
    let d = data(C + 3);
    let (mut m, chunks) = seal(&d);
    m.sha256 = vec![0; 32];
    assert_eq!(open(&m, &chunks), Err(FileError::HashMismatch));
}

#[test]
fn encryptor_rejects_data_that_does_not_match_declared_hash() {
    let d = data(100);
    let mut enc = FileEncryptor::new("a", "b", 100, sha(&d)).unwrap();
    let mut changed = d.clone();
    changed[0] ^= 1;
    enc.encrypt_next(&changed).unwrap();
    assert_eq!(enc.finish(), Err(FileError::HashMismatch));
}

#[test]
fn encryptor_enforces_chunk_lengths_and_count() {
    let d = data(C + 1);
    let mut enc = encryptor(&d);
    assert_eq!(
        enc.encrypt_next(&d[..C - 1]),
        Err(FileError::WrongPlaintextLength)
    );
    // An error ends the transfer.
    assert_eq!(enc.encrypt_next(&d[..C]), Err(FileError::Failed));

    let mut enc = encryptor(&d);
    enc.encrypt_next(&d[..C]).unwrap();
    assert_eq!(enc.finish(), Err(FileError::Truncated));
    enc.encrypt_next(&d[C..]).unwrap();
    enc.finish().unwrap();
    assert_eq!(enc.encrypt_next(&[]), Err(FileError::TooManyChunks));
}

#[test]
fn decryptor_checks_ciphertext_length_and_poisons_after_error() {
    let (m, chunks) = seal(&data(C + 1));
    let mut dec = FileDecryptor::new(m).unwrap();
    assert_eq!(
        dec.decrypt_next(&chunks[0][..10]),
        Err(FileError::BadChunkLength)
    );
    assert_eq!(dec.decrypt_next(&chunks[0]), Err(FileError::Failed));
    assert_eq!(dec.finish(), Err(FileError::Failed));
}

#[test]
fn manifest_validation() {
    let (m, _) = seal(&data(10));
    let mut bad = m.clone();
    bad.file_key.pop();
    assert_eq!(
        FileDecryptor::new(bad).err(),
        Some(FileError::InvalidManifest)
    );
    let mut bad = m.clone();
    bad.chunk_count = 5;
    assert_eq!(
        FileDecryptor::new(bad).err(),
        Some(FileError::InvalidManifest)
    );
    let mut bad = m.clone();
    bad.chunk_size = 1024;
    assert_eq!(
        FileDecryptor::new(bad).err(),
        Some(FileError::InvalidManifest)
    );
    let mut bad = m;
    bad.size = max_plaintext() + 1;
    assert_eq!(FileDecryptor::new(bad).err(), Some(FileError::FileTooLarge));
    assert!(FileEncryptor::new("a", "b", max_plaintext() + 1, [0; 32]).is_err());
}

#[test]
fn header_codec_and_manifest_check() {
    let d = data(C + 1);
    let enc = encryptor(&d);
    let hdr = enc.stream_header("chan-1").unwrap();
    assert_eq!(hdr.chunk_count, 2);
    assert_eq!(hdr.total_ciphertext_len, (C + 1 + 32) as u64);
    let mut delivered = hdr.clone();
    delivered.sender_session_id = 4;
    let dec = FileDecryptor::new(enc.manifest().clone()).unwrap();
    dec.check_header(&delivered).unwrap();

    let mut other = delivered.clone();
    other.file_id = [9; 16];
    assert_eq!(dec.check_header(&other), Err(FileError::HeaderMismatch));
    let mut other = delivered.clone();
    other.total_ciphertext_len += 1;
    assert!(dec.check_header(&other).is_err());

    // UniFFI codec round-trip.
    let rec = decode_file_stream_header(delivered.encode()).unwrap();
    assert_eq!(encode_file_stream_header(rec).unwrap(), delivered.encode());
    assert_eq!(
        file_stream_header_channel_id_len(delivered.encode()[..FILE_HEADER_FIXED_LEN].to_vec())
            .unwrap(),
        6
    );
}

#[test]
fn announcement_travels_in_encrypted_text() {
    let key = [0x42u8; 32];
    let d = data(500);
    let enc = encryptor(&d);
    let msg = crate::text_crypto::create_text_message("alice", "look at this", None);
    let pkt =
        encrypt_file_announcement(&key, 3, "7".into(), 1, msg, enc.manifest().clone()).unwrap();
    // The key must not appear in the packet in the clear.
    assert!(!pkt
        .ciphertext
        .windows(32)
        .any(|w| w == enc.manifest().file_key.as_slice()));
    let (m, manifest) = decrypt_file_announcement(&key, &pkt).unwrap();
    assert_eq!(m.content, "look at this");
    assert_eq!(m.r#type, aura_protocol::MediaType::File as i32);
    assert_eq!(manifest.unwrap(), *enc.manifest());
    assert!(decrypt_file_announcement(&[0x43u8; 32], &pkt).is_err());
}

#[test]
fn hasher_matches_sha256() {
    let d = data(1000);
    let mut h = FileHasher::new();
    h.update(&d[..300]);
    h.update(&d[300..]);
    assert_eq!(h.finalize(), sha(&d));
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Pins the nonce and AAD construction: any change to either breaks
/// interoperability between clients and must be a deliberate version bump.
/// Key 0x11*32, file id 0x22*16. Vectors were produced by an independent
/// Python implementation (HChaCha20 + ChaCha20-Poly1305), not by this code.
#[test]
fn known_answer_vectors() {
    // Single chunk.
    let d = b"hello aura file".to_vec();
    let (m, chunks) = seal(&d);
    assert_eq!(hex(&m.sha256), KAT_SHA256_HELLO);
    assert_eq!(chunks.len(), 1);
    assert_eq!(hex(&chunks[0]), KAT_CT_HELLO);

    // Empty file.
    let (_, chunks) = seal(&[]);
    assert_eq!(hex(&chunks[0]), KAT_CT_EMPTY);

    // Two chunks (65537 bytes of i % 251): digest of the concatenated ciphertexts.
    let (_, chunks) = seal(&data(C + 1));
    assert_eq!(hex(&sha(&chunks.concat())), KAT_DIGEST_TWO_CHUNKS);
}

const KAT_SHA256_HELLO: &str =
    "8849ec6a95e487957ed548539a4b8c1f1ccc5e2470bc3e979d490507aa90a3a7";
const KAT_CT_HELLO: &str = "7b72591113a8704e55fe198cf0e2792fff30be25bdadcea25f64c307c6aea6";
const KAT_CT_EMPTY: &str = "ad26f55d63f742dee4de1755e856fdaa";
const KAT_DIGEST_TWO_CHUNKS: &str =
    "0285fcb43660613408a93753aea02a50d89acbc8bf85b0efa12a59b13e3e4f47";
