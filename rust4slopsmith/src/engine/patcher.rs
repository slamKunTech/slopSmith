//! PSARC pack/unpack + the writer. Port of the read+write parts of
//! `lib/patcher.py`. `pack_psarc` produces a PSARC whose TOC is AES-CFB-128
//! encrypted and whose blocks are zlib-compressed (stored raw when compression
//! doesn't help). `unpack_psarc` delegates to [`crate::engine::psarc`].
//!
//! The App-ID patching CLI (`patch_psarc`) is not used by the server or
//! retune and is not ported; only `pack_psarc`/`unpack_psarc` are (retune
//! depends on both).

use std::io::Write;
use std::path::Path;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use md5::{Digest, Md5};

use crate::engine::psarc::{self, ENTRY_SIZE};

/// Pack a directory into a PSARC archive. Mirrors `pack_psarc`
/// (patcher.py:142-229). The output's TOC is AES-CFB-128 encrypted; blocks use
/// zlib (level 6, matching Python's `zlib.compress` default), stored raw when
/// compression doesn't shrink them.
pub fn pack_psarc(input_dir: &Path, output_path: &Path) -> std::io::Result<()> {
    let block_size = crate::engine::psarc::BLOCK_SIZE;

    // Collect files (sorted, like Python's sorted(rglob('*'))).
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in walkdir::WalkDir::new(input_dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(input_dir)
            .map(|p| p.to_string_lossy().replace('\\', "/").to_string())
            .unwrap_or_default();
        if rel.is_empty() {
            continue;
        }
        let data = std::fs::read(entry.path())?;
        files.push((rel, data));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // Entry 0 is the newline-delimited file list.
    let mut file_list = String::new();
    for (name, _) in &files {
        file_list.push_str(name);
        file_list.push('\n');
    }
    let file_list_bytes = file_list.into_bytes();

    let mut all_data: Vec<Vec<u8>> = Vec::with_capacity(files.len() + 1);
    all_data.push(file_list_bytes);
    for (_, data) in files {
        all_data.push(data);
    }
    let toc_entries = all_data.len();

    let mut compressed_blocks: Vec<Vec<u8>> = Vec::new();
    let mut block_sizes: Vec<u16> = Vec::new();
    // (z_index, length, blocks) per entry.
    let mut entry_info: Vec<(usize, usize, Vec<Vec<u8>>)> = Vec::new();

    for entry_data in &all_data {
        let z_index = compressed_blocks.len();
        let mut entry_blocks: Vec<Vec<u8>> = Vec::new();
        let mut offset = 0;
        while offset < entry_data.len() {
            let end = (offset + block_size).min(entry_data.len());
            let chunk = &entry_data[offset..end];
            let compressed = zlib_compress(chunk);
            if compressed.len() < chunk.len() {
                entry_blocks.push(compressed.clone());
                block_sizes.push(compressed.len() as u16);
            } else {
                entry_blocks.push(chunk.to_vec());
                block_sizes.push(0);
            }
            offset += block_size;
        }
        if entry_blocks.is_empty() {
            entry_blocks.push(Vec::new());
            block_sizes.push(0);
        }
        compressed_blocks.extend(entry_blocks.iter().cloned());
        entry_info.push((z_index, entry_data.len(), entry_blocks));
    }

    // Block table: big-endian u16 per block size.
    let mut block_table: Vec<u8> = Vec::with_capacity(block_sizes.len() * 2);
    for bs in &block_sizes {
        block_table.extend_from_slice(&bs.to_be_bytes());
    }

    let header_size = 32;
    let toc_data_size = ENTRY_SIZE * toc_entries;
    let toc_length = header_size + toc_data_size + block_table.len();

    // Per-entry offsets: the data region follows the TOC.
    let mut offsets: Vec<u64> = Vec::with_capacity(toc_entries);
    let mut cur = toc_length as u64;
    for (_zi, _len, blocks) in &entry_info {
        offsets.push(cur);
        for b in blocks {
            cur += b.len() as u64;
        }
    }

    // TOC data: md5(raw_data) + z_index(>I) + length(5 BE) + offset(5 BE).
    let mut toc_data: Vec<u8> = Vec::with_capacity(toc_data_size);
    for (i, (z_index, length, _blocks)) in entry_info.iter().enumerate() {
        let raw = &all_data[i];
        let mut hasher = Md5::new();
        hasher.update(raw);
        let md5 = hasher.finalize();
        toc_data.extend_from_slice(&md5);
        toc_data.extend_from_slice(&(*z_index as u32).to_be_bytes());
        toc_data.extend_from_slice(&to_be_bytes_5(*length as u64));
        toc_data.extend_from_slice(&to_be_bytes_5(offsets[i]));
    }

    // Encrypt TOC entries + block table as one region.
    let mut toc_region = Vec::with_capacity(toc_data.len() + block_table.len());
    toc_region.extend_from_slice(&toc_data);
    toc_region.extend_from_slice(&block_table);
    let encrypted_region = psarc::encrypt_toc(&toc_region);

    let mut out = std::fs::File::create(output_path)?;
    out.write_all(psarc::MAGIC)?;
    out.write_all(&65540u32.to_be_bytes())?;
    out.write_all(b"zlib")?;
    out.write_all(&(toc_length as u32).to_be_bytes())?;
    out.write_all(&(ENTRY_SIZE as u32).to_be_bytes())?;
    out.write_all(&(toc_entries as u32).to_be_bytes())?;
    out.write_all(&(block_size as u32).to_be_bytes())?;
    out.write_all(&4u32.to_be_bytes())?;
    out.write_all(&encrypted_region)?;
    for (_zi, _len, blocks) in &entry_info {
        for block in blocks {
            out.write_all(block)?;
        }
    }
    Ok(())
}

/// `zlib.compress(data)` (default level 6) → a zlib-wrapped deflate stream.
fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).ok();
    encoder.finish().unwrap_or_default()
}

/// Encode a u64 as 5 big-endian bytes (PSARC's 40-bit length/offset).
fn to_be_bytes_5(v: u64) -> [u8; 5] {
    let b = v.to_be_bytes();
    [b[3], b[4], b[5], b[6], b[7]]
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Pack a temp dir (small + >1-block + nested files), unpack, and assert
    /// the round-trip preserves contents. Also drops the packed file at
    /// /tmp/rust_packed.psarc for a cross-language Python-unpack check.
    #[test]
    fn pack_unpack_roundtrip() {
        let dir = std::env::temp_dir().join("slopsmith_pack_test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello world").unwrap();
        let big = vec![42u8; 100_000]; // spans 2 blocks (64KiB each)
        std::fs::write(dir.join("b.bin"), &big).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/c.json"), b"{\"x\":1}").unwrap();

        let out = std::env::temp_dir().join("slopsmith_pack_test.psarc");
        pack_psarc(&dir, &out).unwrap();
        std::fs::copy(&out, "/tmp/rust_packed.psarc").ok();

        let unpack = std::env::temp_dir().join("slopsmith_unpack_test");
        std::fs::remove_dir_all(&unpack).ok();
        crate::engine::psarc::unpack_psarc(&out, &unpack).unwrap();
        assert_eq!(std::fs::read(unpack.join("a.txt")).unwrap(), b"hello world");
        assert_eq!(std::fs::read(unpack.join("b.bin")).unwrap(), big);
        assert_eq!(std::fs::read(unpack.join("sub/c.json")).unwrap(), b"{\"x\":1}");
    }
}
