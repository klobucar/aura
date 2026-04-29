//! Raw FFI bindings to libopus 1.6
//!
//! This crate provides unsafe C bindings for the Opus audio codec,
//! specifically targeting v1.6 features: DRED, 24-bit API, and 96kHz support.

#![allow(non_camel_case_types)]

use std::os::raw::{c_float, c_int, c_uchar};

// ============================================================================
// Opaque Types
// ============================================================================

/// Opus encoder state (opaque)
#[repr(C)]
pub struct OpusEncoder {
    _opaque: [u8; 0],
}

/// Opus decoder state (opaque)
#[repr(C)]
pub struct OpusDecoder {
    _opaque: [u8; 0],
}

/// DRED (Deep Redundancy) state container (opaque)
#[repr(C)]
pub struct OpusDRED {
    _opaque: [u8; 0],
}

/// DRED decoder/parser state (opaque)
#[repr(C)]
pub struct OpusDREDDecoder {
    _opaque: [u8; 0],
}

// ============================================================================
// Type Aliases (from opus_types.h)
// ============================================================================

pub type opus_int16 = i16;
pub type opus_int32 = i32;

// ============================================================================
// Application Modes
// ============================================================================

/// Best for most VoIP/videoconference applications where listening quality
/// and intelligibility matter most
pub const OPUS_APPLICATION_VOIP: c_int = 2048;

/// Best for broadcast/high-fidelity application where the decoded audio
/// should be as close as possible to the input
pub const OPUS_APPLICATION_AUDIO: c_int = 2049;

/// Only use when lowest-achievable latency is what matters most
pub const OPUS_APPLICATION_RESTRICTED_LOWDELAY: c_int = 2051;

// ============================================================================
// Error Codes
// ============================================================================

pub const OPUS_OK: c_int = 0;
pub const OPUS_BAD_ARG: c_int = -1;
pub const OPUS_BUFFER_TOO_SMALL: c_int = -2;
pub const OPUS_INTERNAL_ERROR: c_int = -3;
pub const OPUS_INVALID_PACKET: c_int = -4;
pub const OPUS_UNIMPLEMENTED: c_int = -5;
pub const OPUS_INVALID_STATE: c_int = -6;
pub const OPUS_ALLOC_FAIL: c_int = -7;

// ============================================================================
// CTL Request Codes
// ============================================================================

// Encoder CTLs
pub const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
pub const OPUS_GET_BITRATE_REQUEST: c_int = 4003;
pub const OPUS_SET_BANDWIDTH_REQUEST: c_int = 4008;
pub const OPUS_GET_BANDWIDTH_REQUEST: c_int = 4009;
pub const OPUS_SET_COMPLEXITY_REQUEST: c_int = 4010;
pub const OPUS_GET_COMPLEXITY_REQUEST: c_int = 4011;
pub const OPUS_SET_INBAND_FEC_REQUEST: c_int = 4012;
pub const OPUS_GET_INBAND_FEC_REQUEST: c_int = 4013;
pub const OPUS_SET_PACKET_LOSS_PERC_REQUEST: c_int = 4014;
pub const OPUS_GET_PACKET_LOSS_PERC_REQUEST: c_int = 4015;
pub const OPUS_SET_DTX_REQUEST: c_int = 4016;
pub const OPUS_GET_DTX_REQUEST: c_int = 4017;
pub const OPUS_SET_VBR_REQUEST: c_int = 4006;
pub const OPUS_GET_VBR_REQUEST: c_int = 4007;

// DRED CTLs (new in 1.5+)
pub const OPUS_SET_DRED_DURATION_REQUEST: c_int = 4050;
pub const OPUS_GET_DRED_DURATION_REQUEST: c_int = 4051;

// Decoder CTLs
pub const OPUS_SET_GAIN_REQUEST: c_int = 4034;
pub const OPUS_GET_GAIN_REQUEST: c_int = 4045;
pub const OPUS_GET_LAST_PACKET_DURATION_REQUEST: c_int = 4039;

// Generic CTLs
pub const OPUS_RESET_STATE: c_int = 4028;
pub const OPUS_GET_FINAL_RANGE_REQUEST: c_int = 4031;
pub const OPUS_GET_SAMPLE_RATE_REQUEST: c_int = 4029;

// Bandwidth constants
pub const OPUS_BANDWIDTH_NARROWBAND: c_int = 1101;
pub const OPUS_BANDWIDTH_MEDIUMBAND: c_int = 1102;
pub const OPUS_BANDWIDTH_WIDEBAND: c_int = 1103;
pub const OPUS_BANDWIDTH_SUPERWIDEBAND: c_int = 1104;
pub const OPUS_BANDWIDTH_FULLBAND: c_int = 1105;

// Special bitrate values
pub const OPUS_AUTO: c_int = -1000;
pub const OPUS_BITRATE_MAX: c_int = -1;

// ============================================================================
// FFI Functions
// ============================================================================

extern "C" {
    // ========================================================================
    // Encoder Functions
    // ========================================================================

    /// Allocates and initializes an encoder state.
    ///
    /// # Arguments
    /// * `Fs` - Sampling rate (8000, 12000, 16000, 24000, 48000, or 96000 Hz)
    /// * `channels` - Number of channels (1 or 2)
    /// * `application` - Coding mode (OPUS_APPLICATION_*)
    /// * `error` - Error code output
    ///
    /// # Returns
    /// Pointer to encoder state, or NULL on failure
    pub fn opus_encoder_create(
        Fs: opus_int32,
        channels: c_int,
        application: c_int,
        error: *mut c_int,
    ) -> *mut OpusEncoder;

    /// Frees an OpusEncoder allocated by opus_encoder_create()
    pub fn opus_encoder_destroy(st: *mut OpusEncoder);

    /// Perform a CTL function on an Opus encoder
    pub fn opus_encoder_ctl(st: *mut OpusEncoder, request: c_int, ...) -> c_int;

    /// Encodes an Opus frame from 16-bit PCM input
    ///
    /// # Arguments
    /// * `st` - Encoder state
    /// * `pcm` - Input signal (interleaved if 2 channels)
    /// * `frame_size` - Number of samples per channel (2.5, 5, 10, 20, 40, 60, 80, 100, 120 ms)
    /// * `data` - Output buffer (at least max_data_bytes)
    /// * `max_data_bytes` - Size of output buffer (4000 recommended)
    ///
    /// # Returns
    /// Length of encoded packet (bytes), or negative error code
    pub fn opus_encode(
        st: *mut OpusEncoder,
        pcm: *const opus_int16,
        frame_size: c_int,
        data: *mut c_uchar,
        max_data_bytes: opus_int32,
    ) -> opus_int32;

    /// Encodes an Opus frame from 24-bit PCM input (NEW in v1.6)
    ///
    /// PCM samples are stored in the lower 24 bits of opus_int32,
    /// with nominal range [-2^23, 2^23-1]. Values slightly outside
    /// this range are supported without hard clipping.
    pub fn opus_encode24(
        st: *mut OpusEncoder,
        pcm: *const opus_int32,
        frame_size: c_int,
        data: *mut c_uchar,
        max_data_bytes: opus_int32,
    ) -> opus_int32;

    /// Encodes an Opus frame from float PCM input
    pub fn opus_encode_float(
        st: *mut OpusEncoder,
        pcm: *const c_float,
        frame_size: c_int,
        data: *mut c_uchar,
        max_data_bytes: opus_int32,
    ) -> opus_int32;

    // ========================================================================
    // Decoder Functions
    // ========================================================================

    /// Allocates and initializes a decoder state
    pub fn opus_decoder_create(
        Fs: opus_int32,
        channels: c_int,
        error: *mut c_int,
    ) -> *mut OpusDecoder;

    /// Frees an OpusDecoder allocated by opus_decoder_create()
    pub fn opus_decoder_destroy(st: *mut OpusDecoder);

    /// Perform a CTL function on an Opus decoder
    pub fn opus_decoder_ctl(st: *mut OpusDecoder, request: c_int, ...) -> c_int;

    /// Decodes an Opus packet to 16-bit PCM
    ///
    /// # Arguments
    /// * `st` - Decoder state
    /// * `data` - Input payload (NULL for packet loss concealment)
    /// * `len` - Length of input payload
    /// * `pcm` - Output buffer (frame_size * channels samples)
    /// * `frame_size` - Number of samples per channel of available space
    /// * `decode_fec` - Flag to request FEC decoding (0 or 1)
    ///
    /// # Returns
    /// Number of decoded samples, or negative error code
    pub fn opus_decode(
        st: *mut OpusDecoder,
        data: *const c_uchar,
        len: opus_int32,
        pcm: *mut opus_int16,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;

    /// Decodes an Opus packet to 24-bit PCM (NEW in v1.6)
    pub fn opus_decode24(
        st: *mut OpusDecoder,
        data: *const c_uchar,
        len: opus_int32,
        pcm: *mut opus_int32,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;

    /// Decodes an Opus packet to float PCM
    pub fn opus_decode_float(
        st: *mut OpusDecoder,
        data: *const c_uchar,
        len: opus_int32,
        pcm: *mut c_float,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;

    // ========================================================================
    // DRED (Deep Redundancy) Functions - v1.5+, improved in v1.6
    // ========================================================================

    /// Creates a new DRED state
    pub fn opus_dred_create(error: *mut c_int) -> *mut OpusDRED;

    /// Frees a DRED state
    pub fn opus_dred_free(dred: *mut OpusDRED);

    /// Gets the size of an OpusDRED structure
    pub fn opus_dred_get_size() -> c_int;

    /// Creates a new DRED decoder
    pub fn opus_dred_decoder_create(error: *mut c_int) -> *mut OpusDREDDecoder;

    /// Frees a DRED decoder
    pub fn opus_dred_decoder_destroy(dec: *mut OpusDREDDecoder);

    /// Gets the size of an OpusDREDDecoder structure
    pub fn opus_dred_decoder_get_size() -> c_int;

    /// Parses DRED data from an Opus packet extension
    ///
    /// # Arguments
    /// * `dred_dec` - DRED decoder state
    /// * `dred` - DRED state to populate
    /// * `data` - Input Opus packet with DRED extension
    /// * `len` - Length of input data
    /// * `max_dred_samples` - Maximum number of DRED samples to decode
    /// * `sampling_rate` - Sampling rate in Hz
    /// * `dred_end` - Output: position where DRED data ends
    /// * `defer_processing` - If 1, defer CPU-intensive processing to opus_dred_process()
    ///
    /// # Returns
    /// Number of DRED samples available, or negative error code
    pub fn opus_dred_parse(
        dred_dec: *mut OpusDREDDecoder,
        dred: *mut OpusDRED,
        data: *const c_uchar,
        len: opus_int32,
        max_dred_samples: opus_int32,
        sampling_rate: opus_int32,
        dred_end: *mut c_int,
        defer_processing: c_int,
    ) -> c_int;

    /// Processes deferred DRED decoding
    pub fn opus_dred_process(
        dred_dec: *mut OpusDREDDecoder,
        dred: *const OpusDRED,
        dred_state: *mut OpusDRED,
    ) -> c_int;

    /// Decodes audio from DRED data to 16-bit PCM
    ///
    /// # Arguments
    /// * `st` - Decoder state
    /// * `dred` - DRED state from opus_dred_parse()
    /// * `dred_offset` - Offset in samples from the start of DRED data
    /// * `pcm` - Output buffer
    /// * `frame_size` - Number of samples per channel to decode
    ///
    /// # Returns
    /// Number of decoded samples, or negative error code
    pub fn opus_decoder_dred_decode(
        st: *mut OpusDecoder,
        dred: *const OpusDRED,
        dred_offset: opus_int32,
        pcm: *mut opus_int16,
        frame_size: c_int,
    ) -> c_int;

    /// Decodes audio from DRED data to 24-bit PCM (NEW in v1.6)
    pub fn opus_decoder_dred_decode24(
        st: *mut OpusDecoder,
        dred: *const OpusDRED,
        dred_offset: opus_int32,
        pcm: *mut opus_int32,
        frame_size: c_int,
    ) -> c_int;

    /// Decodes audio from DRED data to float PCM
    pub fn opus_decoder_dred_decode_float(
        st: *mut OpusDecoder,
        dred: *const OpusDRED,
        dred_offset: opus_int32,
        pcm: *mut c_float,
        frame_size: c_int,
    ) -> c_int;
}

// ============================================================================
// Helper: Convert error code to Result
// ============================================================================

/// Converts an Opus error code to a descriptive string
pub fn opus_strerror(error: c_int) -> &'static str {
    match error {
        OPUS_OK => "success",
        OPUS_BAD_ARG => "invalid argument",
        OPUS_BUFFER_TOO_SMALL => "buffer too small",
        OPUS_INTERNAL_ERROR => "internal error",
        OPUS_INVALID_PACKET => "invalid packet",
        OPUS_UNIMPLEMENTED => "unimplemented",
        OPUS_INVALID_STATE => "invalid state",
        OPUS_ALLOC_FAIL => "allocation failure",
        _ => "unknown error",
    }
}
