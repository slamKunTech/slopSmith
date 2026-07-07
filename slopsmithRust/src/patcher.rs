//! Rocksmith 2014 CDLC App ID Patcher.
//!
//! Rust port of `slopsmith/lib/patcher.py`.
//!
//! Unpacks a `.psarc` file, replaces the App ID in manifests and appid files,
//! then repacks it, optionally copying the result to the Rocksmith dlc folder.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use flate2::write::ZlibEncoder;
use flate2::Compression;
use walkdir::WalkDir;

use crate::psarc::{encrypt_toc, ARC_IV, ARC_KEY, BLOCK_SIZE, ENTRY_SIZE, MAGIC};

// Silence unused-import warnings for the shared crypto constants which are
// referenced transitively via `encrypt_toc`.
const _: (&[u8; 32], &[u8; 16]) = (&ARC_KEY, &ARC_IV);

/// Default target App ID (Iron Maiden - Aces High).
pub const DEFAULT_APP_ID: &str = "258350";

/// Common CDLC App IDs that need replacing (Cherub Rock variants).
pub const CDLC_APP_IDS: [&str; 2] = ["248750", "248751"];

/// Path to the Rocksmith 2014 dlc folder under the user's home directory.
fn dlc_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".local/share/Steam/steamapps/common/Rocksmith2014/dlc")
}

/// Re-export the PSARC unpacker (Python `patcher.py` has its own copy that is
/// functionally identical to `psarc.unpack_psarc`).
pub fn unpack_psarc(filepath: &Path, output_dir: &Path) -> io::Result<Vec<String>> {
    crate::psarc::unpack_psarc(filepath, output_dir)
}

/// Compress a chunk with zlib (default level 6, matching Python's `zlib.compress`).
fn zlib_compress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data)?;
    enc.finish()
}

/// Encode a value as a 5-byte big-endian array.
fn to_be_5(v: u64) -> [u8; 5] {
    [
        ((v >> 32) & 0xff) as u8,
        ((v >> 24) & 0xff) as u8,
        ((v >> 16) & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        (v & 0xff) as u8,
    ]
}

/// Information collected for a single entry while packing.
struct EntryInfo {
    z_index: u32,
    length: u64,
    blocks: Vec<Vec<u8>>,
    offset: u64,
}

/// Pack a directory into a PSARC archive.
pub fn pack_psarc(input_dir: &Path, output_path: &Path) -> io::Result<()> {
    let block_size = BLOCK_SIZE as usize;

    // Collect all files (recursively), sorted by path for deterministic output.
    let mut file_paths: Vec<PathBuf> = WalkDir::new(input_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();
    file_paths.sort();

    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(file_paths.len());
    for p in &file_paths {
        let rel = p
            .strip_prefix(input_dir)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        let data = fs::read(p)?;
        files.push((rel, data));
    }

    // First entry is the newline-separated file listing.
    let mut file_list = String::new();
    for (name, _) in &files {
        file_list.push_str(name);
        file_list.push('\n');
    }
    let file_list_bytes = file_list.into_bytes();

    // all_data[0] = file listing, then each file's raw bytes.
    let mut all_data: Vec<Vec<u8>> = Vec::with_capacity(files.len() + 1);
    all_data.push(file_list_bytes);
    for (_, data) in &files {
        all_data.push(data.clone());
    }
    let toc_entries = all_data.len();

    let mut block_sizes: Vec<u16> = Vec::new();
    let mut entry_info: Vec<EntryInfo> = Vec::with_capacity(toc_entries);
    let mut total_blocks: u32 = 0;

    for entry_data in &all_data {
        let z_index = total_blocks;
        let mut entry_blocks: Vec<Vec<u8>> = Vec::new();
        let mut offset = 0usize;

        while offset < entry_data.len() {
            let end = std::cmp::min(offset + block_size, entry_data.len());
            let chunk = &entry_data[offset..end];
            let compressed = zlib_compress(chunk)?;
            if compressed.len() < chunk.len() {
                block_sizes.push(compressed.len() as u16);
                entry_blocks.push(compressed);
            } else {
                block_sizes.push(0);
                entry_blocks.push(chunk.to_vec());
            }
            offset += block_size;
        }

        if entry_blocks.is_empty() {
            entry_blocks.push(Vec::new());
            block_sizes.push(0);
        }

        total_blocks += entry_blocks.len() as u32;
        entry_info.push(EntryInfo {
            z_index,
            length: entry_data.len() as u64,
            blocks: entry_blocks,
            offset: 0,
        });
    }

    // Block size table (u16 BE each).
    let mut block_table: Vec<u8> = Vec::with_capacity(block_sizes.len() * 2);
    for &bs in &block_sizes {
        block_table.extend_from_slice(&bs.to_be_bytes());
    }

    let header_size = 32usize;
    let toc_data_size = ENTRY_SIZE * toc_entries;
    let toc_length = header_size + toc_data_size + block_table.len();

    // Compute the absolute offset of each entry's data blocks.
    let mut current_offset = toc_length as u64;
    for info in &mut entry_info {
        info.offset = current_offset;
        for block in &info.blocks {
            current_offset += block.len() as u64;
        }
    }

    // Build the TOC data (per-entry: md5(16) + z_index(4) + length(5) + offset(5)).
    let mut toc_data: Vec<u8> = Vec::with_capacity(toc_data_size);
    for (i, info) in entry_info.iter().enumerate() {
        let digest = md5::compute(&all_data[i]);
        toc_data.extend_from_slice(&digest.0);
        toc_data.extend_from_slice(&info.z_index.to_be_bytes());
        toc_data.extend_from_slice(&to_be_5(info.length));
        toc_data.extend_from_slice(&to_be_5(info.offset));
    }

    // Encrypt the TOC entries + block table together as one region.
    let mut toc_region = toc_data;
    toc_region.extend_from_slice(&block_table);
    let encrypted_region = encrypt_toc(&toc_region);

    // Write the archive.
    let mut out = fs::File::create(output_path)?;
    out.write_all(MAGIC)?;
    out.write_all(&65540u32.to_be_bytes())?; // version
    out.write_all(b"zlib")?; // compression
    out.write_all(&(toc_length as u32).to_be_bytes())?;
    out.write_all(&(ENTRY_SIZE as u32).to_be_bytes())?;
    out.write_all(&(toc_entries as u32).to_be_bytes())?;
    out.write_all(&BLOCK_SIZE.to_be_bytes())?;
    out.write_all(&4u32.to_be_bytes())?; // archive flags (encrypted TOC)
    out.write_all(&encrypted_region)?;
    for info in &entry_info {
        for block in &info.blocks {
            out.write_all(block)?;
        }
    }

    Ok(())
}

/// Read a file as UTF-8 text, returning `None` on I/O or decode failure.
fn read_text(path: &Path) -> Option<String> {
    fs::read(path).ok().and_then(|b| String::from_utf8(b).ok())
}

/// Collect all files under `root` whose extension (case-insensitive) matches `ext`.
fn files_with_ext(root: &Path, ext: &str) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(ext))
                .unwrap_or(false)
        })
        .collect()
}

/// Unpack a PSARC, patch App IDs, repack, and optionally copy to the dlc folder.
///
/// Returns `true` on success.
pub fn patch_psarc(
    input_path: &Path,
    new_app_id: &str,
    output_dir: Option<&Path>,
    copy_to_dlc: bool,
) -> bool {
    if !input_path.exists() {
        println!("  File not found: {}", input_path.display());
        return false;
    }

    let file_name = match input_path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => {
            println!("  Invalid input path: {}", input_path.display());
            return false;
        }
    };
    println!("Processing: {}", file_name);

    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            println!("  Failed to create temp dir: {}", e);
            return false;
        }
    };
    let tmpdir = tmp.path();
    let stem = input_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "psarc".to_string());
    let extract_dir = tmpdir.join(&stem);

    if let Err(e) = unpack_psarc(input_path, &extract_dir) {
        println!("  Failed to unpack: {}", e);
        return false;
    }

    let mut patched_count = 0usize;

    // Patch *.appid files whose content is a known CDLC App ID.
    for appid_file in files_with_ext(&extract_dir, "appid") {
        if let Some(content) = read_text(&appid_file) {
            let trimmed = content.trim();
            if CDLC_APP_IDS.contains(&trimmed) {
                if fs::write(&appid_file, new_app_id).is_ok() {
                    patched_count += 1;
                    println!("  Patched appid: {} -> {}", trimmed, new_app_id);
                }
            }
        }
    }

    // Patch *.json manifests.
    for json_file in files_with_ext(&extract_dir, "json") {
        if let Some(content) = read_text(&json_file) {
            let mut new_content = content.clone();
            for old_id in CDLC_APP_IDS.iter() {
                new_content = new_content.replace(old_id, new_app_id);
            }
            if new_content != content && fs::write(&json_file, &new_content).is_ok() {
                patched_count += 1;
                let name = json_file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                println!("  Patched manifest: {}", name);
            }
        }
    }

    // Patch *.hsan files.
    for hsan_file in files_with_ext(&extract_dir, "hsan") {
        if let Some(content) = read_text(&hsan_file) {
            let mut new_content = content.clone();
            for old_id in CDLC_APP_IDS.iter() {
                new_content = new_content.replace(old_id, new_app_id);
            }
            if new_content != content && fs::write(&hsan_file, &new_content).is_ok() {
                patched_count += 1;
                let name = hsan_file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                println!("  Patched hsan: {}", name);
            }
        }
    }

    if patched_count == 0 {
        println!("  No App ID references found (may already be patched)");
        if copy_to_dlc {
            let dest = dlc_dir().join(&file_name);
            match fs::copy(input_path, &dest) {
                Ok(_) => println!("  Copied as-is to: {}", dest.display()),
                Err(e) => {
                    println!("  Failed to copy: {}", e);
                    return false;
                }
            }
        }
        return true;
    }

    let output_path = tmpdir.join(&file_name);
    if let Err(e) = pack_psarc(&extract_dir, &output_path) {
        println!("  Failed to repack: {}", e);
        return false;
    }

    if copy_to_dlc {
        let dest = dlc_dir().join(&file_name);
        match fs::copy(&output_path, &dest) {
            Ok(_) => println!("  Patched and copied to: {}", dest.display()),
            Err(e) => {
                println!("  Failed to copy: {}", e);
                return false;
            }
        }
    } else if let Some(out_dir) = output_dir {
        let dest = out_dir.join(&file_name);
        match fs::copy(&output_path, &dest) {
            Ok(_) => println!("  Patched and saved to: {}", dest.display()),
            Err(e) => {
                println!("  Failed to copy: {}", e);
                return false;
            }
        }
    }

    println!("  Done! ({} files patched)", patched_count);
    true
}
