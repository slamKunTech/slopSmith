//! Decrypt + parse Rocksmith 2014 vocals SNG files. Port of
//! `lib/sng_vocals.py`. RsCli's sng2xml only handles instruments, so official
//! DLC (SNG-only) gets its lyrics decoded here. Format documented in the
//! Python module docstring (sng_vocals.py:7-42).

use std::io::Read;
use std::path::Path;

use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
use aes::Aes256;
use flate2::read::ZlibDecoder;
use serde_json::{json, Value};

/// Well-known Rocksmith 2014 SNG AES keys (public).
const PC_KEY: [u8; 32] = [
    0xCB, 0x64, 0x8D, 0xF3, 0xD1, 0x2A, 0x16, 0xBF, 0x71, 0x70, 0x14, 0x14, 0xE6, 0x96, 0x19, 0xEC,
    0x17, 0x1C, 0xCA, 0x5D, 0x2A, 0x14, 0x2E, 0x3E, 0x59, 0xDE, 0x7A, 0xDD, 0xA1, 0x8A, 0x3A, 0x30,
];
const MAC_KEY: [u8; 32] = [
    0x98, 0x21, 0x33, 0x0E, 0x34, 0xB9, 0x1F, 0x70, 0xD0, 0xA4, 0x8C, 0xBD, 0x62, 0x59, 0x93, 0x12,
    0x69, 0x70, 0xCE, 0xA0, 0x91, 0x92, 0xC0, 0xE6, 0xCD, 0xA6, 0x76, 0xCC, 0x98, 0x38, 0x28, 0x9D,
];

/// Increment a 16-byte big-endian counter in place (pycryptodome CTR-128).
fn inc_be(counter: &mut [u8; 16]) {
    for b in counter.iter_mut().rev() {
        let (v, carry) = b.overflowing_add(1);
        *b = v;
        if !carry {
            return;
        }
    }
}

/// Decrypt the SNG envelope. Mirrors `_decrypt_sng` (sng_vocals.py:63-76).
/// AES-256-CTR with the 16-byte IV as the initial 128-bit big-endian counter;
/// the decrypted payload starts with a big-endian u32 uncompressed size, then
/// a zlib stream.
fn decrypt_sng(data: &[u8], platform: &str) -> anyhow::Result<Vec<u8>> {
    if data.len() < 24 + 56 {
        anyhow::bail!("SNG too small");
    }
    let iv: [u8; 16] = data[8..24].try_into().unwrap();
    let encrypted = &data[24..data.len() - 56];
    let key = if platform == "mac" { &MAC_KEY } else { &PC_KEY };

    // AES-CTR: keystream = E(counter); counter starts at IV (big-endian),
    // increments per 16-byte block. Decryption == encryption (XOR keystream).
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut buf = encrypted.to_vec();
    let mut counter = iv;
    for chunk in buf.chunks_mut(16) {
        let mut keystream = counter;
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut keystream));
        for (b, k) in chunk.iter_mut().zip(keystream.iter()) {
            *b ^= *k;
        }
        inc_be(&mut counter);
    }

    // Skip the 4-byte big-endian uncompressed size; the rest is a zlib stream.
    let mut decoder = ZlibDecoder::new(&buf[4..]);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

/// Lyrics in the wire shape the highway WS expects: `[{t, d, w}, ...]`.
/// Mirrors `parse_vocals_sng` (sng_vocals.py:79-114). Returns `[]` on any
/// decode error (matches Python's try/except → []).
pub fn parse_vocals_sng(path: &Path, platform: &str) -> Vec<Value> {
    let Ok(raw) = std::fs::read(path) else { return Vec::new() };
    let body = match decrypt_sng(&raw, platform) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    const ENTRY_SIZE: usize = 60;
    const HEADER_SKIP: usize = 16; // four zero u32s preceding the vocal count
    if body.len() < HEADER_SKIP + 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes(body[HEADER_SKIP..HEADER_SKIP + 4].try_into().unwrap()) as usize;
    if count == 0 || body.len() < HEADER_SKIP + 4 + count * ENTRY_SIZE {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(count);
    let mut off = HEADER_SKIP + 4;
    for _ in 0..count {
        let time = f32::from_le_bytes(body[off..off + 4].try_into().unwrap()) as f64;
        // note (i32) at off+4 is unused for vocals.
        let length = f32::from_le_bytes(body[off + 8..off + 12].try_into().unwrap()) as f64;
        let lyric_raw = &body[off + 12..off + 60];
        let end = lyric_raw.iter().position(|&b| b == 0).unwrap_or(lyric_raw.len());
        let lyric_bytes = &lyric_raw[..end];
        let lyric = std::str::from_utf8(lyric_bytes)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| String::from_utf8_lossy(lyric_bytes).into_owned());
        out.push(json!({
            "t": crate::engine::song::round_dp(time, 3),
            "d": crate::engine::song::round_dp(length, 3),
            "w": lyric,
        }));
        off += ENTRY_SIZE;
    }
    out
}
