//! Library metadata scan. Port of the scan logic in server.py:344-612.
//!
//! `_extract_meta_fast` reads PSARC entries in-memory (manifest JSONs + vocals
//! XML/SNG) without unpacking; `_extract_meta_sloppak` reads the sloppak
//! manifest; `_extract_meta_for_file` dispatches on extension. The full
//! PSARC fallback (`unpack_psarc` + `load_song`, for SNG-only official DLC)
//! arrives in Wave 3/4 with `engine::song`; until then, songs the fast path
//! can't title are logged and skipped.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::db::SongMeta;
use crate::engine::{psarc, sloppak, tunings};
use crate::state::{AppState, ScanStatus};

/// Initial scan status (server.py:472).
fn scan_init() -> ScanStatus {
    ScanStatus {
        running: false,
        stage: "idle".to_string(),
        total: 0,
        done: 0,
        current: String::new(),
        error: None,
    }
}

/// Extract metadata from a PSARC using in-memory reading (no disk I/O).
/// Mirrors `_extract_meta_fast` (server.py:344-419).
pub fn extract_meta_fast(psarc_path: &Path) -> std::io::Result<SongMeta> {
    let files = psarc::read_psarc_entries(psarc_path, Some(&["*.json", "*.xml", "*vocals*.sng"]))?;

    let mut title = String::new();
    let mut artist = String::new();
    let mut album = String::new();
    let mut year = String::new();
    let mut duration = 0.0f64;
    let mut tuning = "E Standard".to_string();
    let mut tuning_from_guitar = false;
    let mut arrangements: Vec<Value> = Vec::new();
    let mut has_lyrics = false;
    let mut arr_index = 0i64;

    // Parse manifest JSONs (sorted by path for stable arrangement ordering).
    let mut json_paths: Vec<&String> = files.keys().filter(|p| p.to_lowercase().ends_with(".json")).collect();
    json_paths.sort();
    for path in json_paths {
        let data = &files[path];
        let jdata: Value = match serde_json::from_slice(data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let entries = jdata.get("Entries").and_then(|v| v.as_object());
        let entries = match entries {
            Some(e) => e,
            None => continue,
        };
        for (_k, v) in entries.iter() {
            let attrs = match v.get("Attributes") {
                Some(a) => a,
                None => continue,
            };
            let arr_name = attrs.get("ArrangementName").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if matches!(arr_name.as_str(), "Vocals" | "ShowLights" | "JVocals") {
                continue;
            }
            if title.is_empty() {
                title = attrs.get("SongName").and_then(|v| v.as_str()).unwrap_or("").to_string();
                artist = attrs.get("ArtistName").and_then(|v| v.as_str()).unwrap_or("").to_string();
                album = attrs.get("AlbumName").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if let Some(yr) = attrs.get("SongYear") {
                    year = yr.as_i64().map(|n| n.to_string()).unwrap_or_default();
                }
                if let Some(sl) = attrs.get("SongLength") {
                    duration = sl.as_f64().unwrap_or(0.0);
                }
            }
            if !arr_name.is_empty() {
                // Tuning — prefer guitar arrangements over bass.
                if let Some(tun) = attrs.get("Tuning").and_then(|v| v.as_object()) {
                    let offsets: Vec<i64> = (0..6).map(|i| {
                        tun.get(&format!("string{i}")).and_then(|v| v.as_i64()).unwrap_or(0)
                    }).collect();
                    let tun_name = tunings::tuning_name(&offsets);
                    let is_guitar = matches!(arr_name.as_str(), "Lead" | "Rhythm" | "Combo");
                    if tuning == "E Standard" || (is_guitar && !tuning_from_guitar) {
                        tuning = tun_name;
                        if is_guitar {
                            tuning_from_guitar = true;
                        }
                    }
                }
                let notes = attrs.get("NotesHard")
                    .and_then(|v| v.as_i64())
                    .or_else(|| attrs.get("NotesMedium").and_then(|v| v.as_i64()))
                    .unwrap_or(0);
                arrangements.push(serde_json::json!({ "index": arr_index, "name": arr_name, "notes": notes }));
                arr_index += 1;
            }
        }
    }

    // Vocals: CDLC ships a vocals XML; official DLC ships a vocals .sng.
    for (path, _data) in &files {
        let lower = path.to_lowercase();
        if lower.ends_with(".xml") {
            if xml_root_is_vocals(&files[path]) {
                has_lyrics = true;
                break;
            }
        } else if lower.ends_with(".sng") && lower.contains("vocals") {
            has_lyrics = true;
            break;
        }
    }

    // Sort arrangements: Lead > Combo > Rhythm > Bass.
    let priority = |name: &str| match name {
        "Lead" => 0,
        "Combo" => 1,
        "Rhythm" => 2,
        "Bass" => 3,
        _ => 99,
    };
    arrangements.sort_by_key(|a| a.get("name").and_then(|v| v.as_str()).map(priority).unwrap_or(99));
    for (i, a) in arrangements.iter_mut().enumerate() {
        a["index"] = Value::from(i as i64);
    }

    Ok(SongMeta {
        title,
        artist,
        album,
        year,
        duration,
        tuning,
        arrangements: Value::Array(arrangements),
        has_lyrics,
        format: "psarc".to_string(),
        stem_count: 0,
    })
}

/// Extract metadata for a sloppak. Mirrors `_extract_meta_sloppak`
/// (server.py:422-428).
pub fn extract_meta_sloppak(path: &Path) -> std::io::Result<SongMeta> {
    let m = sloppak::extract_meta(path)?;
    Ok(SongMeta {
        title: m.title,
        artist: m.artist,
        album: m.album,
        year: m.year,
        duration: m.duration,
        tuning: tunings::tuning_name(&m.tuning_offsets),
        arrangements: Value::Array(m.arrangements),
        has_lyrics: m.has_lyrics,
        format: "sloppak".to_string(),
        stem_count: m.stem_count,
    })
}

/// Extract metadata — dispatches on extension. Mirrors
/// `_extract_meta_for_file` (server.py:431-469). Returns `None` if the song
/// can't be titled (the caller logs and skips it).
pub fn extract_meta_for_file(path: &Path) -> Option<SongMeta> {
    if sloppak::is_sloppak(path) {
        return extract_meta_sloppak(path).ok();
    }
    // PSARC fast path.
    if let Ok(meta) = extract_meta_fast(path) {
        if !meta.title.is_empty() {
            return Some(meta);
        }
    }
    // Fallback (SNG-only official DLC) needs engine::song::load_song — Wave 3/4.
    // Until then, such songs are skipped.
    None
}

/// True if the bytes are an XML document whose root element is `<vocals>`.
fn xml_root_is_vocals(data: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(data) else { return false };
    match roxmltree::Document::parse(text) {
        Ok(d) => d.root_element().tag_name().name() == "vocals",
        Err(_) => false,
    }
}

// ── Background scan ──────────────────────────────────────────────────────────

/// Scan all .psarc/.sloppak files in the DLC dir and cache metadata. Mirrors
/// `_background_scan` (server.py:476-566). Sequential (the Python uses an
/// 8-way thread pool; parallelism is a future optimization).
pub fn background_scan(state: Arc<AppState>) {
    {
        let mut s = state.scan_status.lock().unwrap();
        *s = ScanStatus { running: true, stage: "listing".to_string(), ..scan_init() };
    }

    let dlc = match state.cfg.get_dlc_dir() {
        Some(d) => d,
        None => {
            let mut s = state.scan_status.lock().unwrap();
            *s = ScanStatus { stage: "idle".to_string(), error: Some("DLC folder not configured".into()), ..scan_init() };
            tracing::info!("Scan: no DLC folder configured");
            return;
        }
    };

    // List .psarc (skip rs1compatibility) and .sloppak.
    let (psarcs, sloppaks) = match list_songs(&dlc) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("Unable to list {}: {e}", dlc.display());
            let mut s = state.scan_status.lock().unwrap();
            *s = ScanStatus { stage: "error".to_string(), error: Some(msg.clone()), ..scan_init() };
            tracing::error!("Scan failed: {msg}");
            return;
        }
    };
    let all_songs: Vec<PathBuf> = psarcs.iter().chain(sloppaks.iter()).cloned().collect();
    tracing::info!("Scan: listed {} PSARCs and {} sloppaks in {}", psarcs.len(), sloppaks.len(), dlc.display());

    let rel = |f: &Path| -> String {
        f.strip_prefix(&dlc).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| f.file_name().unwrap_or_default().to_string_lossy().to_string())
    };
    let current_files: HashSet<String> = all_songs.iter().map(|f| rel(f)).collect();

    let stale = state.db.delete_missing(&current_files);
    if stale > 0 {
        tracing::info!("Removed {stale} stale DB entries");
    }

    // Figure out which need scanning (mtime/size mismatch or missing).
    let mut to_scan: Vec<(PathBuf, f64, i64)> = Vec::new();
    for f in &all_songs {
        let Ok(meta) = std::fs::metadata(f) else { continue };
        let mtime = mtime(&meta);
        let size = meta.len() as i64;
        let r = rel(f);
        if state.db.get(&r, mtime, size).is_none() {
            to_scan.push((f.clone(), mtime, size));
        }
    }

    if to_scan.is_empty() {
        let mut s = state.scan_status.lock().unwrap();
        *s = ScanStatus { stage: "complete".to_string(), ..scan_init() };
        tracing::info!("Scan: nothing new to scan ({} songs, all cached)", all_songs.len());
        return;
    }

    {
        let mut s = state.scan_status.lock().unwrap();
        *s = ScanStatus { running: true, stage: "scanning".to_string(), total: to_scan.len(), ..scan_init() };
    }
    tracing::info!(
        "Library: {} PSARCs + {} sloppaks, {} cached, {} to scan",
        psarcs.len(), sloppaks.len(), all_songs.len() - to_scan.len(), to_scan.len()
    );

    for (f, mtime, size) in &to_scan {
        let fname = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        tracing::info!("  scanning {fname}");
        let r = rel(f);
        match extract_meta_for_file(f) {
            Some(meta) => {
                if let Err(e) = state.db.put(&r, *mtime, *size, &meta) {
                    tracing::error!("  Failed: {fname}: {e}");
                }
            }
            None => tracing::warn!("  Skipped (no metadata): {fname}"),
        }
        {
            let mut s = state.scan_status.lock().unwrap();
            s.done += 1;
            s.current = fname;
        }
    }

    tracing::info!("Scan complete: {} songs cached", to_scan.len());
    let mut s = state.scan_status.lock().unwrap();
    *s = ScanStatus { stage: "complete".to_string(), ..scan_init() };
}

/// Recursively list `.psarc` (excluding `rs1compatibility`) and `.sloppak`
/// files under `dlc`, sorted. Sloppaks match both file (zip) and directory
/// form by suffix.
fn list_songs(dlc: &Path) -> std::io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut psarcs = Vec::new();
    let mut sloppaks = Vec::new();
    for entry in walkdir::WalkDir::new(dlc).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() && !is_dir_sloppak(p) {
            continue;
        }
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if name.ends_with(".psarc") && !name.contains("rs1compatibility") {
            psarcs.push(p.to_path_buf());
        } else if sloppak::is_sloppak(p) {
            sloppaks.push(p.to_path_buf());
        }
    }
    psarcs.sort();
    sloppaks.sort();
    Ok((psarcs, sloppaks))
}

fn is_dir_sloppak(p: &Path) -> bool {
    p.is_dir() && sloppak::is_sloppak(p)
}

fn mtime(meta: &std::fs::Metadata) -> f64 {
    use std::time::SystemTime;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Start the background scan + periodic rescan. Mirrors `startup_scan`
/// (server.py:591-602). Pre-sets scan_status synchronously so the desktop's
/// first `/api/startup-status` poll sees `running=true`.
pub fn startup_scan(state: Arc<AppState>) {
    {
        let mut s = state.scan_status.lock().unwrap();
        *s = ScanStatus { running: true, stage: "listing".to_string(), ..scan_init() };
    }
    let st = state.clone();
    std::thread::Builder::new()
        .name("slopsmith-scan".into())
        .spawn(move || background_scan(st))
        .ok();
    let st = state.clone();
    std::thread::Builder::new()
        .name("slopsmith-rescan".into())
        .spawn(move || periodic_rescan(st))
        .ok();
}

/// Check for new files every 5 minutes. Mirrors `_periodic_rescan`
/// (server.py:605-612).
fn periodic_rescan(state: Arc<AppState>) {
    std::thread::sleep(std::time::Duration::from_secs(300));
    loop {
        let running = state.scan_status.lock().unwrap().running;
        if !running {
            background_scan(state.clone());
        }
        std::thread::sleep(std::time::Duration::from_secs(300));
    }
}

/// Trigger a rescan if one isn't already running. Returns false if a scan is
/// already in progress. Mirrors `trigger_rescan` (server.py:672-679).
pub fn trigger_rescan(state: Arc<AppState>) -> bool {
    {
        let s = state.scan_status.lock().unwrap();
        if s.running {
            return false;
        }
    }
    let st = state.clone();
    std::thread::Builder::new()
        .name("slopsmith-scan".into())
        .spawn(move || background_scan(st))
        .ok();
    true
}
