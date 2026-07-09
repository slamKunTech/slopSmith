//! PSARC archive reader for Rocksmith 2014. Port of `lib/psarc.py`.
//!
//! Read-only: `read_psarc_entries` (in-memory, pattern-filtered) and
//! `unpack_psarc` (extract to disk). The TOC is AES-128-CFB-128 encrypted
//! when `archive_flags == 4`; per-block storage is zlib. The writer
//! (`pack_psarc`) lives in [`crate::engine::patcher`] (Wave 3).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
use aes::Aes256;
use flate2::read::ZlibDecoder;

/// Well-known Rocksmith 2014 PSARC AES key + IV (public; used by sng2014HSL
/// et al.). Mirrors `ARC_KEY`/`ARC_IV` (psarc.py:18-22).
const ARC_KEY: [u8; 32] = [
    0xC5, 0x3D, 0xB2, 0x38, 0x70, 0xA1, 0xA2, 0xF7, 0x1C, 0xAE, 0x64, 0x06, 0x1F, 0xDD, 0x0E, 0x11,
    0x57, 0x30, 0x9D, 0xC8, 0x52, 0x04, 0xD4, 0xC5, 0xBF, 0xDF, 0x25, 0x09, 0x0D, 0xF2, 0x57, 0x2C,
];
const ARC_IV: [u8; 16] = [
    0xE9, 0x15, 0xAA, 0x01, 0x8F, 0xEF, 0x71, 0xFC, 0x50, 0x81, 0x32, 0xE4, 0xBB, 0x4C, 0xEB, 0x42,
];

pub const MAGIC: &[u8; 4] = b"PSAR";
pub const BLOCK_SIZE: usize = 65536;
pub const ENTRY_SIZE: usize = 30;

/// A parsed TOC entry: z_index (offset into the block-size table), logical
/// length, and absolute offset in the file.
#[derive(Debug, Clone, Copy)]
pub struct TocEntry {
    pub z_index: u32,
    pub length: usize,
    pub offset: u64,
}

/// The parsed PSARC header + TOC + block table + file list.
pub struct ParsedToc {
    pub entries: Vec<TocEntry>,
    pub filenames: Vec<String>,
    pub block_sizes: Vec<u16>,
    pub block_size: usize,
}

/// Decrypt the TOC region with AES-256-CFB-128 (segment_size=128). CFB
/// decryption uses AES *encryption* of the previous ciphertext block (IV for
/// the first) XORed into the current block — implemented manually so we only
/// depend on the `aes` block cipher, not a specific CFB crate's API.
pub fn decrypt_toc(data: &[u8]) -> Vec<u8> {
    let cipher = Aes256::new(GenericArray::from_slice(&ARC_KEY));
    let mut out = data.to_vec();
    let mut prev: [u8; 16] = ARC_IV;
    for chunk in out.chunks_mut(16) {
        // Save the ciphertext block (CFB feeds ciphertext forward, not plaintext).
        let saved: Vec<u8> = chunk.to_vec();
        let mut keystream = prev;
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut keystream));
        for (b, k) in chunk.iter_mut().zip(keystream.iter()) {
            *b ^= *k;
        }
        // prev = full 16-byte ciphertext block (zero-padded if the final chunk
        // is short — irrelevant since no further block follows).
        let mut next = [0u8; 16];
        for (i, b) in saved.iter().enumerate() {
            if i < 16 {
                next[i] = *b;
            }
        }
        prev = next;
    }
    out
}

/// Encrypt the TOC region with AES-256-CFB-128 (segment_size=128) — the
/// inverse of [`decrypt_toc`]. CFB encryption: ciphertext = plaintext XOR
/// E(prev_ciphertext), prev_ciphertext = the just-produced ciphertext block.
pub fn encrypt_toc(data: &[u8]) -> Vec<u8> {
    let cipher = Aes256::new(GenericArray::from_slice(&ARC_KEY));
    let mut out = data.to_vec();
    let mut prev: [u8; 16] = ARC_IV;
    for chunk in out.chunks_mut(16) {
        let mut keystream = prev;
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut keystream));
        // XOR plaintext → ciphertext (chunk now holds ciphertext).
        for (b, k) in chunk.iter_mut().zip(keystream.iter()) {
            *b ^= *k;
        }
        // prev = the ciphertext we just produced.
        let mut next = [0u8; 16];
        for (i, b) in chunk.iter().enumerate() {
            if i < 16 {
                next[i] = *b;
            }
        }
        prev = next;
    }
    out
}

/// Read a big-endian u32.
fn rd_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Read a big-endian 5-byte integer (PSARC uses 40-bit lengths/offsets).
fn rd_u40(b: &[u8]) -> u64 {
    let mut bytes = [0u8; 8];
    bytes[3..].copy_from_slice(b);
    u64::from_be_bytes(bytes)
}

/// Parse the PSARC header, TOC, block table, and file list (entry 0). Mirrors
/// `_parse_toc` (psarc.py:54-100).
pub fn parse_toc<R: Read + Seek>(f: &mut R) -> std::io::Result<ParsedToc> {
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a PSARC file",
        ));
    }

    let mut hdr = [0u8; 28]; // version(4)+compression(4)+toc_length(4)+toc_entry_size(4)+
                            // toc_entries(4)+block_size(4)+archive_flags(4)
    f.read_exact(&mut hdr)?;
    let toc_length = rd_u32(&hdr[8..12]) as usize;
    let toc_entry_size = rd_u32(&hdr[12..16]) as usize;
    let toc_entries = rd_u32(&hdr[16..20]) as usize;
    let block_size = rd_u32(&hdr[20..24]) as usize;
    let archive_flags = rd_u32(&hdr[24..28]);

    let toc_region_size = toc_length.saturating_sub(32);
    let mut toc_region_raw = vec![0u8; toc_region_size];
    f.read_exact(&mut toc_region_raw)?;

    let toc_region = if archive_flags == 4 {
        decrypt_toc(&toc_region_raw)
    } else {
        toc_region_raw
    };

    let toc_data_size = toc_entry_size * toc_entries;
    let toc_data = &toc_region[..toc_data_size.min(toc_region.len())];
    let bt_data = &toc_region[toc_data_size.min(toc_region.len())..];

    let mut entries = Vec::with_capacity(toc_entries);
    for i in 0..toc_entries {
        let off = i * toc_entry_size;
        let ed = &toc_data[off..(off + toc_entry_size).min(toc_data.len())];
        if ed.len() < 30 {
            break;
        }
        let z_index = rd_u32(&ed[16..20]);
        let length = rd_u40(&ed[20..25]) as usize;
        let offset = rd_u40(&ed[25..30]);
        entries.push(TocEntry { z_index, length, offset });
    }

    let mut block_sizes = Vec::with_capacity(bt_data.len() / 2);
    for i in 0..(bt_data.len() / 2) {
        let off = i * 2;
        block_sizes.push(u16::from_be_bytes([bt_data[off], bt_data[off + 1]]));
    }

    // Entry 0 is the newline-delimited file list.
    let file_list_data = extract_entry(f, entries[0], &block_sizes, block_size)?;
    let text = String::from_utf8_lossy(&file_list_data);
    let filenames: Vec<String> = text
        .replace("\r\n", "\n")
        .trim()
        .split('\n')
        .map(|s| s.to_string())
        .collect();

    Ok(ParsedToc {
        entries,
        filenames,
        block_sizes,
        block_size,
    })
}

/// Extract a single entry's bytes from the file. Mirrors `_extract_entry`
/// (psarc.py:29-51). `entry` is the TOC entry; `block_sizes` is the block-size
/// table; `block_size` is the archive's block size (usually 65536).
pub fn extract_entry<R: Read + Seek>(
    f: &mut R,
    entry: TocEntry,
    block_sizes: &[u16],
    block_size: usize,
) -> std::io::Result<Vec<u8>> {
    if entry.length == 0 {
        return Ok(Vec::new());
    }
    f.seek(SeekFrom::Start(entry.offset))?;
    let num_blocks = (entry.length + block_size - 1) / block_size;
    let mut result: Vec<u8> = Vec::with_capacity(entry.length);

    for i in 0..num_blocks {
        let bi = entry.z_index as usize + i;
        let compressed_size = if bi < block_sizes.len() {
            block_sizes[bi] as usize
        } else {
            0
        };

        if compressed_size == 0 {
            // Stored uncompressed.
            let remaining = entry.length - result.len();
            let take = block_size.min(remaining);
            let mut buf = vec![0u8; take];
            f.read_exact(&mut buf)?;
            result.extend_from_slice(&buf);
        } else {
            let mut block_data = vec![0u8; compressed_size];
            f.read_exact(&mut block_data)?;
            // Try zlib; on failure fall back to the raw block (matches the
            // Python `except zlib.error: result += block_data`).
            let mut decoder = ZlibDecoder::new(&block_data[..]);
            let mut out = Vec::new();
            match decoder.read_to_end(&mut out) {
                Ok(_) => result.extend_from_slice(&out),
                Err(_) => result.extend_from_slice(&block_data),
            }
        }
    }

    result.truncate(entry.length);
    Ok(result)
}

/// glob-style pattern match against `fnmatch.fnmatch(name.lower(), p.lower())`.
/// Implements the subset PSARC patterns use: `*`, `?`, literal otherwise;
/// case-insensitive; `*` crosses `/`.
fn fnmatch(name: &str, pattern: &str) -> bool {
    fn m(name: &[u8], pat: &[u8]) -> bool {
        let mut ni = 0;
        let mut pi = 0;
        let mut star_pi = None;
        let mut star_ni = 0;
        while ni < name.len() {
            if pi < pat.len() && (pat[pi] == b'?' || pat[pi] == name[ni]) {
                ni += 1;
                pi += 1;
            } else if pi < pat.len() && pat[pi] == b'*' {
                star_pi = Some(pi);
                star_ni = ni;
                pi += 1;
            } else if let Some(sp) = star_pi {
                pi = sp + 1;
                star_ni += 1;
                ni = star_ni;
            } else {
                return false;
            }
        }
        while pi < pat.len() && pat[pi] == b'*' {
            pi += 1;
        }
        pi == pat.len()
    }
    let name_l = name.to_lowercase().into_bytes();
    let pat_l = pattern.to_lowercase().into_bytes();
    m(&name_l, &pat_l)
}

/// Read specific files from a PSARC into memory. Mirrors
/// `read_psarc_entries` (psarc.py:103-130). `patterns` are fnmatch globs
/// (case-insensitive); `None` reads all entries.
pub fn read_psarc_entries(
    filepath: &Path,
    patterns: Option<&[&str]>,
) -> std::io::Result<std::collections::HashMap<String, Vec<u8>>> {
    let mut f = std::fs::File::open(filepath)?;
    let toc = parse_toc(&mut f)?;
    let mut result: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    // entries[1..] aligns with filenames (entry 0 is the file list itself).
    for (entry, filename) in toc.entries.iter().skip(1).zip(toc.filenames.iter()) {
        let filename = filename.trim();
        if filename.is_empty() {
            continue;
        }
        if let Some(pats) = patterns {
            if !pats.iter().any(|p| fnmatch(filename, p)) {
                continue;
            }
        }
        // Match Python: failures are swallowed (entry simply omitted).
        if let Ok(data) = extract_entry(&mut f, *entry, &toc.block_sizes, toc.block_size) {
            result.insert(filename.to_string(), data);
        }
    }
    Ok(result)
}

/// Extract a PSARC archive to `output_dir`. Mirrors `unpack_psarc`
/// (psarc.py:133-154). Returns the list of extracted file paths. On a
/// per-entry failure, an empty file is written (matches Python).
pub fn unpack_psarc(filepath: &Path, output_dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut f = std::fs::File::open(filepath)?;
    let toc = parse_toc(&mut f)?;
    let mut extracted = Vec::new();

    for (entry, filename) in toc.entries.iter().skip(1).zip(toc.filenames.iter()) {
        let filename = filename.trim();
        if filename.is_empty() {
            continue;
        }
        // Path-traversal guard: reject entries that escape output_dir.
        let outpath = output_dir.join(filename);
        if !outpath.starts_with(output_dir) {
            continue;
        }
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = extract_entry(&mut f, *entry, &toc.block_sizes, toc.block_size)
            .unwrap_or_default();
        let mut file = std::fs::File::create(&outpath)?;
        file.write_all(&data)?;
        extracted.push(outpath);
    }
    Ok(extracted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnmatch_basics() {
        assert!(fnmatch("foo.json", "*.json"));
        assert!(fnmatch("gfx/abc.xml", "*.xml"));
        assert!(fnmatch("song_vocals.sng", "*vocals*.sng"));
        assert!(!fnmatch("foo.json", "*.xml"));
        assert!(fnmatch("ABC.JSON", "*.json")); // case-insensitive
    }
}
