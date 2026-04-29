//! Aura Core - Cross-Platform VoIP Client Engine
//!
//! This crate provides the shared Rust logic for the Aura voice/text client,
//! exposed to Swift (macOS) and C# (Windows) via UniFFI bindings.

#![allow(unpredictable_function_pointer_comparisons)]

#[cfg(feature = "native-audio")]
pub mod audio_io;
pub mod audio_pipeline;
pub mod crypto;
pub mod jitter_buffer;
pub mod mls;
pub mod noise_suppression;
pub mod opus;
pub mod opus16;
pub mod text_crypto;
pub mod tts;
pub mod uniffi_bindings;
pub mod vad;
pub mod voice_session;
#[cfg(feature = "webrtc-audio")]
pub mod webrtc_processor;
#[cfg(feature = "native-audio")]
use crate::uniffi_bindings::AudioHardware;

uniffi::setup_scaffolding!("aura_core");

// =============================================================================
// SECURITY: XChaCha20-Poly1305 Encryption (DAVE Protocol)
// =============================================================================
//
// This is where the E2EE encryption loop will be implemented.
//
// CRITICAL REQUIREMENTS:
//
// 1. CIPHER: XChaCha20-Poly1305 with 192-bit (24-byte) random nonces
//    - Extended nonce avoids birthday bound issues with high packet rates
//    - Use `chacha20poly1305` crate with `XChaCha20Poly1305` type
//
// 2. ZERO-PADDING COMMITMENT (TOB-DISCE2EC-5 Mitigation):
//    Before encryption, prepend 16 bytes of 0x00 to plaintext:
//
//    ```rust
//    fn encrypt_audio_frame(key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
//        // 1. Zero-Padding Commitment (prevents Partitioning Oracle attacks)
//        let mut padded = vec![0u8; 16];  // 16 bytes of 0x00
//        padded.extend_from_slice(plaintext);
//
//        // 2. Generate random 192-bit nonce
//        let nonce: [u8; 24] = rand::thread_rng().gen();
//
//        // 3. Encrypt with XChaCha20-Poly1305
//        let cipher = XChaCha20Poly1305::new(key.into());
//        let ciphertext = cipher.encrypt(&nonce.into(), padded.as_ref())
//            .expect("encryption failure");
//
//        // 4. Return: nonce || ciphertext
//        [nonce.as_ref(), &ciphertext].concat()
//    }
//    ```
//
// 3. DECRYPTION VALIDATION:
//    After decryption, verify the first 16 bytes are all 0x00.
//    If not, REJECT the packet (potential attack or key mismatch).
//
// 4. KEY DERIVATION:
//    Keys are derived from MLS epoch secrets via HKDF-SHA256.
//    Voice and Text use SEPARATE MLS groups with independent epochs.
//
// =============================================================================

// =============================================================================
// Simple C-FFI for C# Audio (Fallback when UniFFI generator is unavailable)
// =============================================================================

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_new() -> *mut AudioHardware {
    if let Ok(hw) = AudioHardware::new() {
        Box::into_raw(Box::new(hw))
    } else {
        std::ptr::null_mut()
    }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_free(hw: *mut AudioHardware) {
    if !hw.is_null() {
        unsafe { drop(Box::from_raw(hw)) };
    }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_start_capture(hw: *mut AudioHardware) -> i32 {
    let hw = unsafe { &*hw };
    if hw.start_capture().is_ok() {
        0
    } else {
        -1
    }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_stop_capture(hw: *mut AudioHardware) -> i32 {
    let hw = unsafe { &*hw };
    if hw.stop_capture().is_ok() {
        0
    } else {
        -1
    }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_read_capture(
    hw: *mut AudioHardware,
    buf: *mut i16,
    len: usize,
) -> i32 {
    let hw = unsafe { &*hw };
    if let Some(samples) = hw.read_capture() {
        let to_copy = samples.len().min(len);
        unsafe {
            std::ptr::copy_nonoverlapping(samples.as_ptr(), buf, to_copy);
        }
        to_copy as i32
    } else {
        0
    }
}

#[cfg(feature = "native-audio")]
#[no_mangle]
pub extern "C" fn aura_audio_write_playback(
    hw: *mut AudioHardware,
    buf: *const i16,
    len: usize,
) -> i32 {
    let hw = unsafe { &*hw };
    let mut vec = vec![0i16; len];
    unsafe {
        std::ptr::copy_nonoverlapping(buf, vec.as_mut_ptr(), len);
    }
    if hw.write_playback(vec).is_ok() {
        0
    } else {
        -1
    }
}

#[cfg(test)]
mod tests_audio;
#[cfg(test)]
mod tests_crypto;
