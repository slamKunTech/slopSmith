//! PSARC file extractor for Rocksmith 2014.
//!
//! Rust port of `slopsmith/lib/psarc.py`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use cfb_mode::cipher::{AsyncStreamCipher, KeyIvInit};
use flate2::read::ZlibDecoder;

type Aes256CfbEnc = cfb_mode::Encryptor<aes::Aes256>;
type Aes256CfbDec = cfb_mode::Decryptor<aes::Aes256>;

pub const MAGIC: &[u8; 4] = b"PSAR";
pub const BLOCK_SIZE: u32 = 65536;
pub const ENTRY_SIZE: usize = 30;

/// Rocksmith PSARC encryption key (TOC only).
pub const ARC_KEY: [u8; 32] = [
    0xC5, 0x3D, 0xB2, 0x38, 0x70, 0xA1, 0xA2, 0xF7, 0x1C, 0xAE, 0x64, 0x06, 0x1F, 0xDD, 0x0E, 0x11,
    0x57, 0x30, 0x9D, 0xC8, 0x52, 0x04, 0xD4, 0xC5, 0xBF, 0xDF, 0x25, 0x09, 0x0D, 0xF2, 0x57, 0x2C,
];
pub const ARC_IV: [u8; 16] = [
    0xE9, 0x15, 0xAA, 0x01, 0x8F, 0xEF, 0x71, 0xFC, 0x50, 0x81, 0x32, 0xE4, 0xBB, 0x4C, 0xEB, 0x42,
];

/// A single TOC entry describing a stored file.
#[derive(Debug, Clone)]
pub struct Entry {
    pub z_index: u32,
    pub length: u64,
    pub offset: u64,
}

/// Decrypt the TOC region using AES-256-CFB (segment_size=128, i.e. full-block CFB).
fn decrypt_toc(data: &[u8]) -> Vec<u8> {
    crypt_toc(data, false)
}

/// Encrypt the TOC region using AES-256-CFB (segment_size=128, i.e. full-block CFB).
pub fn encrypt_toc(data: &[u8]) -> Vec<u8> {
    crypt_toc(data, true)
}

/// Shared CFB helper. We pad the buffer to a 16-byte boundary so the underlying
/// block-mode cipher can process it, then truncate back. Because CFB-128 only
/// feeds forward, the extra trailing bytes never affect the real prefix, which
/// makes this byte-for-byte compatible with Python's `segment_size=128` CFB.
fn crypt_toc(data: &[u8], encrypt: bool) -> Vec<u8> {
    let orig_len = data.len();
    let mut buf = data.to_vec();
    let rem = buf.len() % 16;
    if rem != 0 {
        buf.resize(buf.len() + (16 - rem), 0);
    }
    if encrypt {
        Aes256CfbEnc::new(&ARC_KEY.into(), &ARC_IV.into()).encrypt(&mut buf);
    } else {
        Aes256CfbDec::new(&ARC_KEY.into(), &ARC_IV.into()).decrypt(&mut buf);
    }
    buf.truncate(orig_len);
    buf
}

fn read_u32_be<R: Read>(f: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    f.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

/// Decode a big-endian integer from an arbitrary-length byte slice.
fn be_int(bytes: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for &b in bytes {
        v = (v << 8) | b as u64;
    }
    v
}

/// Attempt zlib decompression, returning an error if the stream is invalid.
fn zlib_decompress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut dec = ZlibDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

/// Extract a single entry's raw (decompressed) bytes.
fn extract_entry<R: Read + Seek>(
    f: &mut R,
    entry: &Entry,
    block_sizes: &[u32],
    block_size: u32,
) -> io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(entry.offset))?;
    if entry.length == 0 {
        return Ok(Vec::new());
    }

    let bs = block_size as u64;
    let num_blocks = ((entry.length + bs - 1) / bs) as usize;
    let mut result: Vec<u8> = Vec::new();

    for i in 0..num_blocks {
        let bi = entry.z_index as usize + i;
        let compressed_size = if bi < block_sizes.len() {
            block_sizes[bi]
        } else {
            0
        };

        if compressed_size == 0 {
            let remaining = entry.length - result.len() as u64;
            let to_read = std::cmp::min(bs, remaining) as usize;
            let mut buf = vec![0u8; to_read];
            f.read_exact(&mut buf)?;
            result.extend_from_slice(&buf);
        } else {
            let mut buf = vec![0u8; compressed_size as usize];
            f.read_exact(&mut buf)?;
            match zlib_decompress(&buf) {
                Ok(d) => result.extend_from_slice(&d),
                Err(_) => result.extend_from_slice(&buf),
            }
        }
    }

    result.truncate(entry.length as usize);
    Ok(result)
}

/// Parse PSARC header, TOC, and file listing.
///
/// Returns `(entries, filenames, block_sizes, block_size)`.
fn parse_toc<R: Read + Seek>(f: &mut R) -> io::Result<(Vec<Entry>, Vec<String>, Vec<u32>, u32)> {
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Not a PSARC file"));
    }

    let _version = read_u32_be(f)?;
    let mut _compression = [0u8; 4];
    f.read_exact(&mut _compression)?;
    let toc_length = read_u32_be(f)?;
    let toc_entry_size = read_u32_be(f)?;
    let toc_entries = read_u32_be(f)?;
    let block_size = read_u32_be(f)?;
    let archive_flags = read_u32_be(f)?;

    let toc_region_size = toc_length.saturating_sub(32) as usize;
    let mut toc_region_raw = vec![0u8; toc_region_size];
    f.read_exact(&mut toc_region_raw)?;

    let toc_region = if archive_flags == 4 {
        decrypt_toc(&toc_region_raw)
    } else {
        toc_region_raw
    };

    let toc_data_size = (toc_entry_size as usize) * (toc_entries as usize);
    let toc_data = &toc_region[..toc_data_size.min(toc_region.len())];
    let bt_data = if toc_data_size < toc_region.len() {
        &toc_region[toc_data_size..]
    } else {
        &[][..]
    };

    let mut entries = Vec::with_capacity(toc_entries as usize);
    for i in 0..toc_entries as usize {
        let off = i * toc_entry_size as usize;
        let ed = &toc_data[off..off + toc_entry_size as usize];
        let z_index = u32::from_be_bytes([ed[16], ed[17], ed[18], ed[19]]);
        let length = be_int(&ed[20..25]);
        let offset = be_int(&ed[25..30]);
        entries.push(Entry {
            z_index,
            length,
            offset,
        });
    }

    let mut block_sizes = Vec::with_capacity(bt_data.len() / 2);
    for i in 0..(bt_data.len() / 2) {
        let hi = bt_data[i * 2] as u32;
        let lo = bt_data[i * 2 + 1] as u32;
        block_sizes.push((hi << 8) | lo);
    }

    let file_list_data = extract_entry(f, &entries[0], &block_sizes, block_size)?;
    let text = String::from_utf8_lossy(&file_list_data);
    let normalized = text.replace("\r\n", "\n");
    let filenames = normalized
        .trim()
        .split('\n')
        .map(|s| s.to_string())
        .collect::<Vec<String>>();

    Ok((entries, filenames, block_sizes, block_size))
}

/// Case-insensitive shell-style glob match (like Python's `fnmatch.fnmatch`).
fn fnmatch(name: &str, pattern: &str) -> bool {
    let name_l = name.to_lowercase();
    let pat_l = pattern.to_lowercase();
    match glob::Pattern::new(&pat_l) {
        Ok(p) => p.matches(&name_l),
        Err(_) => false,
    }
}

/// Read specific files from a PSARC archive directly into memory.
///
/// * `patterns` - Optional list of glob patterns to match (e.g. `["*.json", "*.xml"]`).
///   If `None`, reads all entries.
///
/// Returns a map of internal path -> raw bytes.
pub fn read_psarc_entries(
    filepath: &Path,
    patterns: Option<&[String]>,
) -> io::Result<HashMap<String, Vec<u8>>> {
    let mut result: HashMap<String, Vec<u8>> = HashMap::new();
    let file = File::open(filepath)?;
    let mut f = BufReader::new(file);

    let (entries, filenames, block_sizes, block_size) = parse_toc(&mut f)?;

    // entries[0] is the file listing; the rest map 1:1 with `filenames`.
    for (entry, filename) in entries.iter().skip(1).zip(filenames.iter()) {
        let filename = filename.trim();
        if filename.is_empty() {
            continue;
        }
        if let Some(pats) = patterns {
            if !pats.iter().any(|p| fnmatch(filename, p)) {
                continue;
            }
        }
        match extract_entry(&mut f, entry, &block_sizes, block_size) {
            Ok(data) => {
                result.insert(filename.to_string(), data);
            }
            Err(_) => { /* mirror Python: silently skip failed entries */ }
        }
    }

    Ok(result)
}

/// Extract a PSARC archive to `output_dir`.
///
/// Returns the list of extracted file paths.
pub fn unpack_psarc(filepath: &Path, output_dir: &Path) -> io::Result<Vec<String>> {
    let mut extracted: Vec<String> = Vec::new();

    let file = File::open(filepath)?;
    let mut f = BufReader::new(file);

    let (entries, filenames, block_sizes, block_size) = parse_toc(&mut f)?;

    for (entry, filename) in entries.iter().skip(1).zip(filenames.iter()) {
        let filename = filename.trim();
        if filename.is_empty() {
            continue;
        }
        let outpath = output_dir.join(filename);
        if let Some(parent) = outpath.parent() {
            fs::create_dir_all(parent)?;
        }
        match extract_entry(&mut f, entry, &block_sizes, block_size) {
            Ok(data) => {
                fs::write(&outpath, &data)?;
                extracted.push(outpath.to_string_lossy().into_owned());
            }
            Err(_) => {
                // Mirror Python behaviour: write an empty file on failure.
                fs::write(&outpath, b"")?;
            }
        }
    }

    Ok(extracted)
}
