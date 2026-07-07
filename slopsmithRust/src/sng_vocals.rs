//! Decrypt + parse Rocksmith 2014 vocals SNG files.
//!
//! Translated from `lib/sng_vocals.py`.
//!
//! RsCli's sng2xml only handles instrumental arrangements, so official DLC
//! (which ships SNG-only) has no lyrics path. This module decodes the vocals
//! SNG directly so lyrics can be served for both official DLC and CDLC.
//!
//! SNG vocals file format (little-endian unless noted)
//! ────────────────────────────────────────────────────
//! Top-level envelope (what `decrypt_sng` strips):
//!
//!     offset  size  field
//!     ──────  ────  ──────────────────────────────────────
//!      0      4     magic (u32)        # ignored by the decoder
//!      4      4     version (u32)      # ignored by the decoder
//!      8     16     iv                 # AES-CTR initial counter
//!     24      N     encrypted_payload  # AES-CTR ciphertext
//!    -56     56     signature footer   # ignored by the decoder
//!
//! `encrypted_payload`, after AES-CTR decryption with the platform key
//! (PC_KEY or MAC_KEY), starts with:
//!
//!     +0   4     uncompressed_size (big-endian u32)
//!     +4   ...   zlib stream
//!
//! Decompressed body for a vocals arrangement:
//!
//!     +0   16    four u32 zeros (section counts: beats / phrases /
//!                  chord_templates / chord_notes — all 0 for vocals)
//!     +16   4    vocal_count (u32)
//!     +20   N*60 vocal entries
//!
//! Each 60-byte vocal entry:
//!
//!     +0    4     time   (float32)
//!     +4    4     note   (int32; unused — vocals aren't pitched here)
//!     +8    4     length (float32)
//!     +12  48     lyric  (utf-8, null-terminated, zero-padded)

use std::error::Error;
use std::fs;
use std::io::Read;

use ctr::cipher::{KeyIvInit, StreamCipher};
use flate2::read::ZlibDecoder;

/// AES-256 in big-endian 128-bit counter mode (matches Python's
/// `Counter.new(128, initial_value=int.from_bytes(iv, "big"))`).
type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;

// Well-known Rocksmith 2014 SNG AES keys (public, used by sng2014HSL et al).
const PC_KEY: &str = "CB648DF3D12A16BF71701414E69619EC171CCA5D2A142E3E59DE7ADDA18A3A30";
const MAC_KEY: &str = "9821330E34B91F70D0A48CBD625993126970CEA09192C0E6CDA676CC9838289D";

/// A single lyric syllable, matching the wire shape the highway WS expects.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricEntry {
    /// Start time (seconds).
    pub t: f64,
    /// Duration (seconds).
    pub d: f64,
    /// The lyric word/syllable text.
    pub w: String,
}

/// Decode a hex string into raw bytes.
fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Round to 3 decimal places (mirrors Python's `round(x, 3)`).
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Strip the SNG envelope: AES-CTR decrypt, then zlib-inflate the payload.
fn decrypt_sng(data: &[u8], platform: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    // Header: u32 magic, u32 version, 16-byte IV, payload..., 56-byte signature.
    if data.len() < 24 + 56 {
        return Err("SNG too small".into());
    }
    let iv = &data[8..24];
    let encrypted = &data[24..data.len() - 56];

    let key = hex_decode(if platform == "mac" { MAC_KEY } else { PC_KEY });

    let mut cipher = Aes256Ctr::new_from_slices(&key, iv)?;
    let mut buf = encrypted.to_vec();
    cipher.apply_keystream(&mut buf);

    // First 4 bytes big-endian uncompressed size, then zlib stream.
    if buf.len() < 4 {
        return Err("decrypted payload too small".into());
    }
    let mut decoder = ZlibDecoder::new(&buf[4..]);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

/// Return lyrics in the same wire shape the highway WS expects.
///
/// `platform` should be "pc" (default) or "mac". Any decode/parse failure
/// yields an empty vector, exactly like the Python implementation.
pub fn parse_vocals_sng(path: &str, platform: &str) -> Vec<LyricEntry> {
    let raw = match fs::read(path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let body = match decrypt_sng(&raw, platform) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    // Vocals SNG layout: four empty u32 section counts (beats/phrases/
    // chord_templates/chord_notes, all zero for a vocals-only track), then the
    // vocals section itself: u32 count followed by N × 60-byte entries.
    const ENTRY_SIZE: usize = 60;
    const HEADER_SKIP: usize = 16; // four zero u32s preceding the vocal count

    if body.len() < HEADER_SKIP + 4 {
        return Vec::new();
    }

    let count = u32::from_le_bytes(
        body[HEADER_SKIP..HEADER_SKIP + 4]
            .try_into()
            .expect("4-byte slice"),
    ) as usize;

    if count == 0 || body.len() < HEADER_SKIP + 4 + count * ENTRY_SIZE {
        return Vec::new();
    }

    let mut out: Vec<LyricEntry> = Vec::with_capacity(count);
    let mut off = HEADER_SKIP + 4;

    for _ in 0..count {
        let time = f32::from_le_bytes(body[off..off + 4].try_into().unwrap()) as f64;
        // note (int32) at off+4 is unused for vocals.
        let length = f32::from_le_bytes(body[off + 8..off + 12].try_into().unwrap()) as f64;

        let lyric_raw = &body[off + 12..off + 60];
        let end = lyric_raw
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(lyric_raw.len());
        let slice = &lyric_raw[..end];
        let lyric = match std::str::from_utf8(slice) {
            Ok(v) => v.to_string(),
            // latin-1 fallback: each byte maps 1:1 to a code point.
            Err(_) => slice.iter().map(|&b| b as char).collect(),
        };

        out.push(LyricEntry {
            t: round3(time),
            d: round3(length),
            w: lyric,
        });
        off += ENTRY_SIZE;
    }

    out
}
