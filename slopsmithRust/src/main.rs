//! Rocksmith Web — Axum backend (Rust port of `server.py`).
//!
//! Serves the highway viewer + song library. This is the crate entry point:
//! it wires up the Axum router, owns the shared `AppState`, runs the background
//! metadata scan, and drives the highway / retune WebSockets.
//!
//! NOTE: the heavy binary decoders (PSARC unpacking, WEM→audio conversion,
//! DDS art extraction, sloppak loading) live in their own crate modules in the
//! full port. Here they are represented by clearly-marked placeholder functions
//! so the web layer — routing, state, DB access, WebSocket protocol — is
//! complete and self-consistent. Swapping in the real decoders only requires
//! filling `load_song` / `unpack_psarc` / `convert_audio`.

#![allow(dead_code)]

mod db;
mod plugins;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path as AxPath, Query, State,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower_http::services::ServeDir;
use walkdir::WalkDir;

use db::{MetadataDB, SongMeta};
use plugins::{load_plugins, register_plugin_api, PluginContext, PluginInfo};

// ── Song data model (mirrors lib/song.py dataclasses) ─────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Note {
    pub time: f64,
    pub string: i32,
    pub fret: i32,
    pub sustain: f64,
    pub slide_to: i32,
    pub slide_unpitch_to: i32,
    pub bend: f64,
    pub hammer_on: bool,
    pub pull_off: bool,
    pub harmonic: bool,
    pub harmonic_pinch: bool,
    pub palm_mute: bool,
    pub mute: bool,
    pub tremolo: bool,
    pub accent: bool,
    pub link_next: bool,
    pub tap: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChordTemplate {
    pub name: String,
    pub fingers: Vec<i32>,
    pub frets: Vec<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Chord {
    pub time: f64,
    pub chord_id: i32,
    pub notes: Vec<Note>,
    pub high_density: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Anchor {
    pub time: f64,
    pub fret: i32,
    pub width: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Beat {
    pub time: f64,
    pub measure: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub number: i32,
    pub start_time: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HandShape {
    pub chord_id: i32,
    pub start_time: f64,
    pub end_time: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhraseLevel {
    pub difficulty: i32,
    pub notes: Vec<Note>,
    pub chords: Vec<Chord>,
    pub anchors: Vec<Anchor>,
    pub hand_shapes: Vec<HandShape>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Phrase {
    pub start_time: f64,
    pub end_time: f64,
    pub max_difficulty: i32,
    pub levels: Vec<PhraseLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arrangement {
    pub name: String,
    pub tuning: Vec<i32>,
    pub capo: i32,
    pub notes: Vec<Note>,
    pub chords: Vec<Chord>,
    pub anchors: Vec<Anchor>,
    pub hand_shapes: Vec<HandShape>,
    pub chord_templates: Vec<ChordTemplate>,
    pub phrases: Option<Vec<Phrase>>,
}

impl Default for Arrangement {
    fn default() -> Self {
        Arrangement {
            name: String::new(),
            tuning: vec![0; 6],
            capo: 0,
            notes: Vec::new(),
            chords: Vec::new(),
            anchors: Vec::new(),
            hand_shapes: Vec::new(),
            chord_templates: Vec::new(),
            phrases: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: i32,
    pub song_length: f64,
    pub offset: f64,
    pub beats: Vec<Beat>,
    pub sections: Vec<Section>,
    pub arrangements: Vec<Arrangement>,
    pub audio_path: String,
    pub lyrics: Vec<Value>,
}

// ── Tuning naming (port of lib/tunings.py) ────────────────────────────────────

fn tuning_name(offsets: &[i32]) -> String {
    if offsets.len() == 6 && offsets.iter().all(|&o| o == offsets[0]) {
        let name = match offsets[0] {
            0 => Some("E Standard"),
            -1 => Some("Eb Standard"),
            -2 => Some("D Standard"),
            -3 => Some("C# Standard"),
            -4 => Some("C Standard"),
            -5 => Some("B Standard"),
            -6 => Some("Bb Standard"),
            -7 => Some("A Standard"),
            1 => Some("F Standard"),
            2 => Some("F# Standard"),
            _ => None,
        };
        if let Some(n) = name {
            return n.to_string();
        }
    }

    // Drop tunings: low string two semitones below the rest.
    if offsets.len() == 6
        && offsets[0] == offsets[1] - 2
        && offsets[1..].iter().all(|&o| o == offsets[1])
    {
        let note_names = [
            "E", "F", "F#", "G", "Ab", "A", "Bb", "B", "C", "C#", "D", "Eb",
        ];
        let idx = ((offsets[0] % 12) + 12) % 12;
        return format!("Drop {}", note_names[idx as usize]);
    }

    let named: &[(&[i32], &str)] = &[
        (&[-2, 0, 0, 0, 0, 0], "Drop D"),
        (&[-4, -2, -2, -2, -2, -2], "Drop C"),
        (&[-2, -2, 0, 0, 0, 0], "Double Drop D"),
        (&[0, 0, 0, -1, 0, 0], "Open G"),
        (&[-2, -2, 0, 0, -2, -2], "Open D"),
        (&[-2, 0, 0, 0, -2, 0], "DADGAD"),
        (&[0, 2, 2, 1, 0, 0], "Open E"),
        (&[-2, 0, 0, 2, 3, 2], "Open D (alt)"),
    ];
    if offsets.len() == 6 {
        for (pat, name) in named {
            if *pat == offsets {
                return name.to_string();
            }
        }
    }

    if offsets.is_empty() {
        "Unknown".to_string()
    } else {
        offsets
            .iter()
            .map(|o| o.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Derive the active arrangement's string count (port of
/// `arrangement_string_count`).
fn arrangement_string_count(arr: &Arrangement) -> i32 {
    let mut max_s: i32 = -1;
    for n in &arr.notes {
        if n.string > max_s {
            max_s = n.string;
        }
    }
    for ch in &arr.chords {
        for cn in &ch.notes {
            if cn.string > max_s {
                max_s = cn.string;
            }
        }
    }
    let notes_count = if max_s >= 0 { max_s + 1 } else { 0 };
    let name_based = if arr.name.to_lowercase().contains("bass") {
        4
    } else {
        6
    };
    let tuning_len = arr.tuning.len() as i32;
    let tuning_count = if tuning_len != 6 { tuning_len } else { 0 };
    notes_count.max(name_based).max(tuning_count)
}

// ── Wire serialization helpers (highway protocol) ─────────────────────────────

fn r3(f: f64) -> f64 {
    (f * 1000.0).round() / 1000.0
}
fn r1(f: f64) -> f64 {
    (f * 10.0).round() / 10.0
}

fn note_to_wire(n: &Note) -> Value {
    json!({
        "t": r3(n.time), "s": n.string, "f": n.fret,
        "sus": r3(n.sustain),
        "sl": n.slide_to, "slu": n.slide_unpitch_to,
        "bn": if n.bend != 0.0 { r1(n.bend) } else { 0.0 },
        "ho": n.hammer_on, "po": n.pull_off,
        "hm": n.harmonic, "hp": n.harmonic_pinch,
        "pm": n.palm_mute, "mt": n.mute,
        "tr": n.tremolo, "ac": n.accent, "tp": n.tap,
    })
}

fn chord_note_to_wire(cn: &Note) -> Value {
    // Chord notes omit their own time (the chord carries it).
    json!({
        "s": cn.string, "f": cn.fret,
        "sus": r3(cn.sustain),
        "bn": if cn.bend != 0.0 { r1(cn.bend) } else { 0.0 },
        "sl": cn.slide_to, "slu": cn.slide_unpitch_to,
        "ho": cn.hammer_on, "po": cn.pull_off,
        "hm": cn.harmonic, "hp": cn.harmonic_pinch,
        "pm": cn.palm_mute, "mt": cn.mute,
        "tr": cn.tremolo, "ac": cn.accent, "tp": cn.tap,
    })
}

fn chord_to_wire(c: &Chord) -> Value {
    json!({
        "t": r3(c.time),
        "id": c.chord_id,
        "hd": c.high_density,
        "notes": c.notes.iter().map(chord_note_to_wire).collect::<Vec<_>>(),
    })
}

fn phrase_level_to_wire(pl: &PhraseLevel) -> Value {
    json!({
        "difficulty": pl.difficulty,
        "notes": pl.notes.iter().map(note_to_wire).collect::<Vec<_>>(),
        "chords": pl.chords.iter().map(chord_to_wire).collect::<Vec<_>>(),
        "anchors": pl.anchors.iter().map(|a| json!({"time": a.time, "fret": a.fret, "width": a.width})).collect::<Vec<_>>(),
        "handshapes": pl.hand_shapes.iter().map(|h| json!({"chord_id": h.chord_id, "start_time": h.start_time, "end_time": h.end_time})).collect::<Vec<_>>(),
    })
}

fn phrase_to_wire(p: &Phrase) -> Value {
    json!({
        "start_time": r3(p.start_time),
        "end_time": r3(p.end_time),
        "max_difficulty": p.max_difficulty,
        "levels": p.levels.iter().map(phrase_level_to_wire).collect::<Vec<_>>(),
    })
}

// ── Application state ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ScanStatus {
    pub running: bool,
    pub stage: String,
    pub total: usize,
    pub done: usize,
    pub current: String,
    pub error: Option<String>,
}

impl Default for ScanStatus {
    fn default() -> Self {
        ScanStatus {
            running: false,
            stage: "idle".to_string(),
            total: 0,
            done: 0,
            current: String::new(),
            error: None,
        }
    }
}

impl ScanStatus {
    fn running(stage: &str) -> Self {
        ScanStatus {
            running: true,
            stage: stage.to_string(),
            ..Default::default()
        }
    }
    fn stage(stage: &str) -> Self {
        ScanStatus {
            stage: stage.to_string(),
            ..Default::default()
        }
    }
    fn error(stage: &str, msg: &str) -> Self {
        ScanStatus {
            stage: stage.to_string(),
            error: Some(msg.to_string()),
            ..Default::default()
        }
    }
}

pub struct AppState {
    pub meta_db: Arc<Mutex<MetadataDB>>,
    pub scan_status: Arc<Mutex<ScanStatus>>,
    pub config_dir: PathBuf,
    pub dlc_dir_env: String,
    pub dlc_dir: PathBuf,
    pub art_cache_dir: PathBuf,
    pub audio_cache_dir: PathBuf,
    pub sloppak_cache_dir: PathBuf,
    pub static_dir: PathBuf,
    pub plugins: Arc<Mutex<Vec<PluginInfo>>>,
    pub extract_cache: Arc<Mutex<HashMap<String, (String, Song, f64)>>>,
}

// ── Config helpers ────────────────────────────────────────────────────────────

/// Resolve the active DLC directory (env var opt-in first, then config.json).
fn get_dlc_dir(state: &AppState) -> Option<PathBuf> {
    if !state.dlc_dir_env.is_empty() && state.dlc_dir.is_dir() {
        return Some(state.dlc_dir.clone());
    }
    let config_file = state.config_dir.join("config.json");
    if let Ok(text) = std::fs::read_to_string(&config_file) {
        if let Ok(Value::Object(cfg)) = serde_json::from_str::<Value>(&text) {
            if let Some(raw) = cfg.get("dlc_dir").and_then(|v| v.as_str()) {
                let raw = raw.trim();
                if !raw.is_empty() {
                    let p = PathBuf::from(raw);
                    if p.is_dir() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

fn default_settings(state: &AppState) -> Value {
    let dlc = if !state.dlc_dir_env.is_empty() && state.dlc_dir.is_dir() {
        state.dlc_dir.to_string_lossy().to_string()
    } else {
        String::new()
    };
    json!({ "dlc_dir": dlc })
}

/// Read and parse config.json, returning the object only if it parses to a dict.
fn load_config(config_file: &Path) -> Option<serde_json::Map<String, Value>> {
    let text = std::fs::read_to_string(config_file).ok()?;
    match serde_json::from_str::<Value>(&text).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ── Placeholder decoders (delegated to native modules in the full port) ───────

fn is_sloppak(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("sloppak"))
        .unwrap_or(false)
}

/// Extract metadata for a single file. The full port dispatches to PSARC /
/// sloppak decoders; here we derive a minimal record from the filename so the
/// library populates and cache lookups remain stable.
fn extract_meta_for_file(path: &Path, rel: &str) -> SongMeta {
    let stem = Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(rel)
        .to_string();
    let format = if is_sloppak(path) { "sloppak" } else { "psarc" };
    SongMeta {
        title: stem,
        artist: "Unknown Artist".to_string(),
        album: String::new(),
        year: String::new(),
        duration: 0.0,
        tuning: "E Standard".to_string(),
        arrangements: Vec::new(),
        has_lyrics: false,
        format: format.to_string(),
        stem_count: 0,
    }
}

/// Load a full `Song` from a container. Placeholder: the real port unpacks the
/// PSARC / sloppak and parses arrangement XML (see `lib/song.py::load_song`).
fn load_song(_path: &Path) -> Song {
    Song::default()
}

/// Return a cached extraction or a freshly loaded `Song`.
/// Returns `(tmp_dir, song, is_new)`.
async fn get_or_extract(state: &AppState, filename: &str, psarc_path: &Path) -> (String, Song, bool) {
    {
        let cache = state.extract_cache.lock().await;
        if let Some((tmp, song, ts)) = cache.get(filename) {
            if Path::new(tmp).exists() && (now_secs() - ts) < 300.0 {
                return (tmp.clone(), song.clone(), false);
            }
        }
    }

    let path = psarc_path.to_path_buf();
    let song = tokio::task::spawn_blocking(move || load_song(&path))
        .await
        .unwrap_or_default();
    let tmp = std::env::temp_dir()
        .join(format!("rs_web_{}", uuid::Uuid::new_v4()))
        .to_string_lossy()
        .to_string();

    let mut cache = state.extract_cache.lock().await;
    if cache.len() > 10 {
        if let Some(oldest) = cache
            .iter()
            .min_by(|a, b| a.1 .2.partial_cmp(&b.1 .2).unwrap())
            .map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(filename.to_string(), (tmp.clone(), song.clone(), now_secs()));
    (tmp, song, true)
}

// ── Background scan ───────────────────────────────────────────────────────────

fn rel_path(dlc: &Path, f: &Path) -> String {
    f.strip_prefix(dlc)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| {
            f.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
}

fn list_songs(dlc: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for entry in WalkDir::new(dlc).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if p.is_file() && name.ends_with(".psarc") && !name.contains("rs1compatibility") {
            out.push(p.to_path_buf());
        } else if name.ends_with(".sloppak") {
            // Both file (zip) and directory forms count.
            out.push(p.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}

async fn run_scan(state: Arc<AppState>) {
    {
        let mut s = state.scan_status.lock().await;
        if s.running {
            return;
        }
        *s = ScanStatus::running("listing");
    }

    let dlc = match get_dlc_dir(&state) {
        Some(d) => d,
        None => {
            let mut s = state.scan_status.lock().await;
            *s = ScanStatus::error("idle", "DLC folder not configured");
            eprintln!("Scan: no DLC folder configured");
            return;
        }
    };

    let dlc_for_list = dlc.clone();
    let listed = tokio::task::spawn_blocking(move || list_songs(&dlc_for_list)).await;
    let all_songs = match listed {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            let mut s = state.scan_status.lock().await;
            *s = ScanStatus::error("error", &format!("Unable to list {}: {}", dlc.display(), e));
            return;
        }
        Err(e) => {
            let mut s = state.scan_status.lock().await;
            *s = ScanStatus::error("error", &format!("Scan task failed: {}", e));
            return;
        }
    };

    println!("Scan: listed {} songs in {}", all_songs.len(), dlc.display());

    // Current filenames (relative to DLC root).
    let current: std::collections::HashSet<String> =
        all_songs.iter().map(|f| rel_path(&dlc, f)).collect();

    {
        let db = state.meta_db.lock().await;
        let stale = db.delete_missing(&current);
        if stale > 0 {
            println!("Removed {} stale DB entries", stale);
        }
    }

    // Determine which files need scanning.
    let mut to_scan: Vec<(PathBuf, f64, i64, String)> = Vec::new();
    {
        let db = state.meta_db.lock().await;
        for f in &all_songs {
            if let Ok(meta) = std::fs::metadata(f) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let size = meta.len() as i64;
                let rel = rel_path(&dlc, f);
                if db.get(&rel, mtime, size).is_none() {
                    to_scan.push((f.clone(), mtime, size, rel));
                }
            }
        }
    }

    if to_scan.is_empty() {
        let mut s = state.scan_status.lock().await;
        *s = ScanStatus::stage("complete");
        println!("Scan: nothing new ({} songs cached)", all_songs.len());
        return;
    }

    {
        let mut s = state.scan_status.lock().await;
        *s = ScanStatus::running("scanning");
        s.total = to_scan.len();
    }
    println!("Library: {} to scan", to_scan.len());

    for (f, mtime, size, rel) in to_scan {
        println!(
            "  scanning {}",
            f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        );
        let f_clone = f.clone();
        let rel_clone = rel.clone();
        let meta =
            tokio::task::spawn_blocking(move || extract_meta_for_file(&f_clone, &rel_clone))
                .await
                .unwrap_or_default();
        {
            let db = state.meta_db.lock().await;
            db.put(&rel, mtime, size, &meta);
        }
        let mut s = state.scan_status.lock().await;
        s.done += 1;
        s.current = f
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
    }

    let mut s = state.scan_status.lock().await;
    *s = ScanStatus::stage("complete");
    println!("Scan complete");
}

fn spawn_scan(state: Arc<AppState>) {
    tokio::spawn(async move { run_scan(state).await });
}

// ── Version / scan endpoints ──────────────────────────────────────────────────

async fn get_version(State(state): State<Arc<AppState>>) -> Json<Value> {
    if let Ok(v) = std::env::var("APP_VERSION") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Json(json!({ "version": v }));
        }
    }
    let version = state
        .static_dir
        .parent()
        .map(|p| p.join("VERSION"))
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    Json(json!({ "version": version }))
}

async fn scan_status(State(state): State<Arc<AppState>>) -> Json<ScanStatus> {
    let s = state.scan_status.lock().await;
    Json(s.clone())
}

async fn trigger_rescan(State(state): State<Arc<AppState>>) -> Json<Value> {
    {
        let s = state.scan_status.lock().await;
        if s.running {
            return Json(json!({ "message": "Scan already in progress" }));
        }
    }
    spawn_scan(state);
    Json(json!({ "message": "Rescan started" }))
}

async fn trigger_full_rescan(State(state): State<Arc<AppState>>) -> Json<Value> {
    {
        let s = state.scan_status.lock().await;
        if s.running {
            return Json(json!({ "message": "Scan already in progress" }));
        }
    }
    {
        let db = state.meta_db.lock().await;
        db.clear_songs();
    }
    spawn_scan(state);
    Json(json!({ "message": "Full rescan started" }))
}

// ── Library endpoints ─────────────────────────────────────────────────────────

fn default_size() -> i64 {
    24
}
fn default_artist_size() -> i64 {
    50
}
fn default_sort() -> String {
    "artist".into()
}
fn default_dir() -> String {
    "asc".into()
}

#[derive(Deserialize)]
struct LibraryQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    page: i64,
    #[serde(default = "default_size")]
    size: i64,
    #[serde(default = "default_sort")]
    sort: String,
    #[serde(default = "default_dir")]
    dir: String,
    #[serde(default)]
    favorites: i64,
    #[serde(default)]
    format: String,
}

async fn list_library(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LibraryQuery>,
) -> Json<Value> {
    let size = q.size.min(100);
    let fmt = if q.format == "psarc" || q.format == "sloppak" {
        q.format.as_str()
    } else {
        ""
    };
    let db = state.meta_db.lock().await;
    let (songs, total) = db.query_page(
        &q.q,
        q.page,
        size,
        &q.sort,
        &q.dir,
        q.favorites != 0,
        fmt,
    );
    Json(json!({ "songs": songs, "total": total, "page": q.page, "size": size }))
}

#[derive(Deserialize)]
struct ArtistsQuery {
    #[serde(default)]
    letter: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    favorites: i64,
    #[serde(default)]
    page: i64,
    #[serde(default = "default_artist_size")]
    size: i64,
    #[serde(default)]
    format: String,
}

async fn list_artists(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ArtistsQuery>,
) -> Json<Value> {
    let fmt = if q.format == "psarc" || q.format == "sloppak" {
        q.format.as_str()
    } else {
        ""
    };
    let db = state.meta_db.lock().await;
    let (artists, total) = db.query_artists(
        &q.letter,
        &q.q,
        q.favorites != 0,
        q.page,
        q.size.min(100),
        fmt,
    );
    Json(json!({ "artists": artists, "total_artists": total, "page": q.page, "size": q.size }))
}

#[derive(Deserialize)]
struct FavoritesQuery {
    #[serde(default)]
    favorites: i64,
}

async fn library_stats(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FavoritesQuery>,
) -> Json<Value> {
    let db = state.meta_db.lock().await;
    Json(serde_json::to_value(db.query_stats(q.favorites != 0)).unwrap_or(Value::Null))
}

async fn toggle_favorite(
    State(state): State<Arc<AppState>>,
    Json(data): Json<Value>,
) -> Json<Value> {
    let filename = data.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    if filename.is_empty() {
        return Json(json!({ "error": "No filename" }));
    }
    let db = state.meta_db.lock().await;
    let new_state = db.toggle_favorite(filename);
    Json(json!({ "favorite": new_state }))
}

// ── Loops endpoints ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LoopsQuery {
    filename: String,
}

async fn list_loops(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LoopsQuery>,
) -> Json<Value> {
    let db = state.meta_db.lock().await;
    Json(serde_json::to_value(db.loops_for(&q.filename)).unwrap_or(Value::Array(vec![])))
}

async fn save_loop(State(state): State<Arc<AppState>>, Json(data): Json<Value>) -> Json<Value> {
    let filename = data.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let start = data.get("start").and_then(|v| v.as_f64());
    let end = data.get("end").and_then(|v| v.as_f64());
    if filename.is_empty() || start.is_none() || end.is_none() {
        return Json(json!({ "error": "Missing fields" }));
    }
    let db = state.meta_db.lock().await;
    let final_name = db.add_loop(filename, name, start.unwrap(), end.unwrap());
    Json(json!({ "ok": true, "name": final_name }))
}

async fn delete_loop(
    State(state): State<Arc<AppState>>,
    AxPath(loop_id): AxPath<i64>,
) -> Json<Value> {
    let db = state.meta_db.lock().await;
    db.delete_loop(loop_id);
    Json(json!({ "ok": true }))
}

// ── Settings endpoints ────────────────────────────────────────────────────────

async fn get_settings(State(state): State<Arc<AppState>>) -> Json<Value> {
    let config_file = state.config_dir.join("config.json");
    match load_config(&config_file) {
        Some(map) => Json(Value::Object(map)),
        None => Json(default_settings(&state)),
    }
}

async fn save_settings(State(state): State<Arc<AppState>>, Json(data): Json<Value>) -> Json<Value> {
    std::fs::create_dir_all(&state.config_dir).ok();
    let config_file = state.config_dir.join("config.json");
    let mut cfg = load_config(&config_file).unwrap_or_else(|| match default_settings(&state) {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    });

    let mut messages: Vec<String> = Vec::new();

    if let Some(v) = data.get("dlc_dir") {
        if v.is_null() {
            // no-op
        } else if let Some(s) = v.as_str() {
            if s.is_empty() {
                cfg.insert("dlc_dir".into(), Value::String(String::new()));
            } else if Path::new(s).is_dir() {
                cfg.insert("dlc_dir".into(), Value::String(s.to_string()));
                let count = std::fs::read_dir(s)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| {
                                e.path().extension().and_then(|x| x.to_str()) == Some("psarc")
                            })
                            .count()
                    })
                    .unwrap_or(0);
                messages.push(format!("DLC folder: {} .psarc files found", count));
            } else {
                return Json(json!({ "error": format!("DLC directory not found: {}", s) }));
            }
        } else {
            return Json(json!({ "error": "dlc_dir must be a string path or empty" }));
        }
    }

    for key in ["default_arrangement", "demucs_server_url"] {
        if let Some(v) = data.get(key) {
            if v.is_null() {
                // no-op
            } else if let Some(s) = v.as_str() {
                cfg.insert(key.into(), Value::String(s.to_string()));
            } else {
                return Json(json!({ "error": format!("{} must be a string or empty", key) }));
            }
        }
    }

    if let Some(v) = data.get("master_difficulty") {
        if v.is_boolean() {
            return Json(json!({ "error": "master_difficulty must be a number between 0 and 100" }));
        }
        match coerce_number(v) {
            Some(n) => {
                let clamped = n.max(0.0).min(100.0) as i64;
                cfg.insert("master_difficulty".into(), json!(clamped));
            }
            None => {
                return Json(
                    json!({ "error": "master_difficulty must be a number between 0 and 100" }),
                )
            }
        }
    }

    if let Some(v) = data.get("av_offset_ms") {
        if v.is_boolean() {
            return Json(json!({ "error": "av_offset_ms must be a number between -1000 and 1000" }));
        }
        match coerce_number(v) {
            Some(n) => {
                let clamped = n.max(-1000.0).min(1000.0);
                cfg.insert("av_offset_ms".into(), json!(clamped));
            }
            None => {
                return Json(
                    json!({ "error": "av_offset_ms must be a number between -1000 and 1000" }),
                )
            }
        }
    }

    let out = serde_json::to_string_pretty(&Value::Object(cfg)).unwrap_or_default();
    std::fs::write(&config_file, out).ok();
    let msg = if messages.is_empty() {
        "Settings saved".to_string()
    } else {
        messages.join(". ")
    };
    Json(json!({ "message": msg }))
}

/// Accept numbers or numeric strings; reject anything else. Mirrors Python's
/// defensive `float()` coercion.
fn coerce_number(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<f64>().ok();
    }
    None
}

// ── Song metadata / art endpoints ─────────────────────────────────────────────

async fn get_song_info(
    State(state): State<Arc<AppState>>,
    AxPath(filename): AxPath<String>,
) -> Response {
    let dlc = match get_dlc_dir(&state) {
        Some(d) => d,
        None => {
            return json_err(StatusCode::NOT_FOUND, "DLC folder not configured");
        }
    };
    let psarc_path = dlc.join(&filename);
    if !psarc_path.exists() {
        return json_err(StatusCode::NOT_FOUND, "File not found");
    }
    let meta = match std::fs::metadata(&psarc_path) {
        Ok(m) => m,
        Err(_) => return json_err(StatusCode::NOT_FOUND, "File not found"),
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let size = meta.len() as i64;

    {
        let db = state.meta_db.lock().await;
        if let Some(cached) = db.get(&filename, mtime, size) {
            return Json(cached).into_response();
        }
    }

    let path = psarc_path.clone();
    let rel = filename.clone();
    let extracted = tokio::task::spawn_blocking(move || extract_meta_for_file(&path, &rel))
        .await
        .unwrap_or_default();
    {
        let db = state.meta_db.lock().await;
        db.put(&filename, mtime, size, &extracted);
    }
    Json(extracted).into_response()
}

async fn update_song_meta(
    State(state): State<Arc<AppState>>,
    AxPath(filename): AxPath<String>,
    Json(data): Json<Value>,
) -> Json<Value> {
    let mut fields: Vec<(&str, String)> = Vec::new();
    for field in ["title", "artist", "album", "year"] {
        if let Some(v) = data.get(field) {
            let s = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            fields.push((field, s));
        }
    }
    if fields.is_empty() {
        return Json(json!({ "error": "No fields to update" }));
    }
    let db = state.meta_db.lock().await;
    db.update_song_fields(&filename, &fields);
    Json(json!({ "ok": true }))
}

async fn get_song_art(
    State(state): State<Arc<AppState>>,
    AxPath(filename): AxPath<String>,
) -> Response {
    let dlc = match get_dlc_dir(&state) {
        Some(d) => d,
        None => return json_err(StatusCode::NOT_FOUND, "not configured"),
    };
    let psarc_path = dlc.join(&filename);
    if !psarc_path.exists() {
        return json_err(StatusCode::NOT_FOUND, "not found");
    }

    // Sloppak (directory form): serve cover image from the source directory.
    if is_sloppak(&psarc_path) && psarc_path.is_dir() {
        for cover in ["cover.jpg", "cover.png", "cover.jpeg", "cover.webp"] {
            let p = psarc_path.join(cover);
            if p.is_file() {
                let ct = mime_for(&p).unwrap_or("image/jpeg");
                return file_response(&p, Some(ct)).await;
            }
        }
        return json_err(StatusCode::NOT_FOUND, "no art");
    }

    // PSARC: serve from art cache if present (DDS extraction handled by the
    // native decoder in the full port).
    std::fs::create_dir_all(&state.art_cache_dir).ok();
    let safe = filename.replace('/', "_").replace(' ', "_");
    let cached = state.art_cache_dir.join(format!("{}.png", safe));
    if cached.exists() {
        return file_response(&cached, Some("image/png")).await;
    }
    json_err(StatusCode::NOT_FOUND, "no art")
}

async fn upload_song_art_b64(
    State(state): State<Arc<AppState>>,
    AxPath(filename): AxPath<String>,
    Json(data): Json<Value>,
) -> Json<Value> {
    use base64::Engine;
    let mut b64 = data.get("image").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if b64.is_empty() {
        return Json(json!({ "error": "No image data" }));
    }
    // Strip data URL prefix.
    if let Some(idx) = b64.find(',') {
        b64 = b64[idx + 1..].to_string();
    }
    let bytes = match base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
        Ok(b) => b,
        Err(_) => return Json(json!({ "error": "Invalid base64" })),
    };

    std::fs::create_dir_all(&state.art_cache_dir).ok();
    let safe = filename.replace('/', "_").replace(' ', "_");
    let cached = state.art_cache_dir.join(format!("{}.png", safe));

    match image::load_from_memory(&bytes) {
        Ok(img) => {
            if img.to_rgb8().save_with_format(&cached, image::ImageFormat::Png).is_err() {
                return Json(json!({ "error": "Failed to write image" }));
            }
        }
        Err(e) => return Json(json!({ "error": format!("Invalid image: {}", e) })),
    }
    Json(json!({ "ok": true }))
}

// ── Sloppak file / audio serving ──────────────────────────────────────────────

async fn serve_sloppak_file(
    State(state): State<Arc<AppState>>,
    AxPath((filename, rel_path)): AxPath<(String, String)>,
) -> Response {
    let dlc = match get_dlc_dir(&state) {
        Some(d) => d,
        None => return json_err(StatusCode::NOT_FOUND, "not configured"),
    };
    // Source dir: directory-form sloppak resolves directly; zip-form would be
    // extracted to the sloppak cache by the native decoder in the full port.
    let container = dlc.join(&filename);
    let src = if container.is_dir() {
        container
    } else {
        state.sloppak_cache_dir.join(&filename)
    };
    if !src.exists() {
        return json_err(StatusCode::NOT_FOUND, "not found");
    }

    let target = src.join(&rel_path);
    // Prevent path traversal.
    let (canon_target, canon_src) = match (target.canonicalize(), src.canonicalize()) {
        (Ok(t), Ok(s)) => (t, s),
        _ => return json_err(StatusCode::NOT_FOUND, "not found"),
    };
    if !canon_target.starts_with(&canon_src) {
        return json_err(StatusCode::FORBIDDEN, "forbidden");
    }
    if !canon_target.is_file() {
        return json_err(StatusCode::NOT_FOUND, "not found");
    }
    let ct = mime_for(&canon_target);
    file_response(&canon_target, ct).await
}

async fn serve_audio(
    State(state): State<Arc<AppState>>,
    AxPath(filename): AxPath<String>,
) -> Response {
    for d in [&state.audio_cache_dir, &state.static_dir] {
        let f = d.join(&filename);
        if f.exists() {
            return file_response(&f, mime_for(&f)).await;
        }
    }
    json_err(StatusCode::NOT_FOUND, "not found")
}

// ── Highway WebSocket ─────────────────────────────────────────────────────────

fn neg_one() -> i64 {
    -1
}

#[derive(Deserialize)]
struct HighwayQuery {
    #[serde(default = "neg_one")]
    arrangement: i64,
}

async fn ws_highway(
    ws: WebSocketUpgrade,
    AxPath(filename): AxPath<String>,
    Query(q): Query<HighwayQuery>,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| highway_socket(socket, filename, q.arrangement, state))
}

async fn send_json(socket: &mut WebSocket, val: &Value) -> bool {
    socket
        .send(Message::Text(serde_json::to_string(val).unwrap_or_default()))
        .await
        .is_ok()
}

async fn highway_socket(mut socket: WebSocket, filename: String, arrangement: i64, state: Arc<AppState>) {
    let dlc = match get_dlc_dir(&state) {
        Some(d) => d,
        None => {
            send_json(&mut socket, &json!({ "error": "DLC folder not configured" })).await;
            let _ = socket.close().await;
            return;
        }
    };
    let psarc_path = dlc.join(&filename);
    if !psarc_path.exists() {
        send_json(&mut socket, &json!({ "error": "File not found" })).await;
        let _ = socket.close().await;
        return;
    }

    let is_slop = is_sloppak(&psarc_path);
    send_json(&mut socket, &json!({ "type": "loading", "stage": "Extracting..." })).await;

    let (tmp, song, _is_new) = get_or_extract(&state, &filename, &psarc_path).await;

    if song.arrangements.is_empty() {
        send_json(&mut socket, &json!({ "error": "No arrangements found" })).await;
        let _ = socket.close().await;
        return;
    }

    // Pick arrangement: explicit request > user preference > most notes.
    let mut best: i64 = -1;
    if arrangement >= 0 && (arrangement as usize) < song.arrangements.len() {
        best = arrangement;
    } else {
        let pref = load_config(&state.config_dir.join("config.json"))
            .and_then(|c| c.get("default_arrangement").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_default();
        if !pref.is_empty() {
            for (i, a) in song.arrangements.iter().enumerate() {
                if a.name == pref {
                    best = i as i64;
                    break;
                }
            }
        }
    }
    if best < 0 {
        best = 0;
        let mut best_count = 0usize;
        for (i, a) in song.arrangements.iter().enumerate() {
            let c = a.notes.len() + a.chords.iter().map(|ch| ch.notes.len()).sum::<usize>();
            if c > best_count {
                best_count = c;
                best = i as i64;
            }
        }
    }
    let arr = &song.arrangements[best as usize];

    // Audio resolution (native WEM conversion handled elsewhere in the full port).
    let mut audio_url: Option<String> = None;
    let mut audio_error: Option<String> = None;
    let stems_payload: Vec<Value> = Vec::new();
    let audio_id = Path::new(&filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("audio")
        .replace(' ', "_");

    if !is_slop {
        std::fs::create_dir_all(&state.audio_cache_dir).ok();
        'outer: for ext in [".mp3", ".ogg", ".wav"] {
            for dir in [&state.audio_cache_dir, &state.static_dir] {
                let f = dir.join(format!("audio_{}{}", audio_id, ext));
                if f.exists() && f.metadata().map(|m| m.len() > 1000).unwrap_or(false) {
                    audio_url = Some(format!("/audio/audio_{}{}", audio_id, ext));
                    break 'outer;
                }
            }
        }
        if audio_url.is_none() {
            audio_error = Some("Audio conversion is handled by the native decoder.".to_string());
        }
    } else {
        audio_error = Some("This sloppak has no playable stems.".to_string());
    }

    // song_info
    let arr_list: Vec<Value> = song
        .arrangements
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let notes = a.notes.len() + a.chords.iter().map(|c| c.notes.len()).sum::<usize>();
            json!({ "index": i, "name": a.name, "notes": notes })
        })
        .collect();
    if !send_json(
        &mut socket,
        &json!({
            "type": "song_info",
            "title": song.title,
            "artist": song.artist,
            "duration": song.song_length,
            "arrangement": arr.name,
            "arrangement_index": best,
            "arrangements": arr_list,
            "audio_url": audio_url,
            "audio_error": audio_error,
            "tuning": arr.tuning,
            "stringCount": arrangement_string_count(arr),
            "capo": arr.capo,
            "format": if is_slop { "sloppak" } else { "psarc" },
            "stems": stems_payload,
        }),
    )
    .await
    {
        return;
    }

    // beats
    let beats: Vec<Value> = song
        .beats
        .iter()
        .map(|b| json!({ "time": b.time, "measure": b.measure }))
        .collect();
    send_json(&mut socket, &json!({ "type": "beats", "data": beats })).await;

    // sections
    let sections: Vec<Value> = song
        .sections
        .iter()
        .map(|s| json!({ "name": s.name, "time": s.start_time }))
        .collect();
    send_json(&mut socket, &json!({ "type": "sections", "data": sections })).await;

    // anchors
    let anchors: Vec<Value> = arr
        .anchors
        .iter()
        .map(|a| json!({ "time": a.time, "fret": a.fret, "width": a.width }))
        .collect();
    send_json(&mut socket, &json!({ "type": "anchors", "data": anchors })).await;

    // chord templates
    let templates: Vec<Value> = arr
        .chord_templates
        .iter()
        .map(|ct| json!({ "name": ct.name, "fingers": ct.fingers, "frets": ct.frets }))
        .collect();
    send_json(&mut socket, &json!({ "type": "chord_templates", "data": templates })).await;

    // lyrics
    if !song.lyrics.is_empty() {
        send_json(&mut socket, &json!({ "type": "lyrics", "data": song.lyrics })).await;
    }

    // notes (chunks of 500)
    let notes: Vec<Value> = arr.notes.iter().map(note_to_wire).collect();
    let total_notes = notes.len();
    for chunk in notes.chunks(500) {
        send_json(
            &mut socket,
            &json!({ "type": "notes", "data": chunk, "total": total_notes }),
        )
        .await;
    }

    // chords (chunks of 500)
    let chords: Vec<Value> = arr.chords.iter().map(chord_to_wire).collect();
    let total_chords = chords.len();
    for chunk in chords.chunks(500) {
        send_json(
            &mut socket,
            &json!({ "type": "chords", "data": chunk, "total": total_chords }),
        )
        .await;
    }

    // phrases (chunks of 20) — only when multi-level data is present
    if let Some(phrases) = &arr.phrases {
        let total = phrases.len();
        let wire: Vec<Value> = phrases.iter().map(phrase_to_wire).collect();
        for chunk in wire.chunks(20) {
            send_json(
                &mut socket,
                &json!({ "type": "phrases", "data": chunk, "total": total }),
            )
            .await;
        }
    }

    send_json(&mut socket, &json!({ "type": "ready" })).await;

    // Keep connection alive for control messages.
    let _ = tmp;
    while let Some(Ok(msg)) = socket.recv().await {
        if let Message::Text(txt) = msg {
            if let Ok(data) = serde_json::from_str::<Value>(&txt) {
                if data.get("action").and_then(|v| v.as_str()) == Some("change_arrangement") {
                    // Handled client-side by reconnecting with ?arrangement=N.
                }
            }
        }
    }
}

// ── Retune WebSocket ──────────────────────────────────────────────────────────

fn default_target() -> String {
    "E Standard".into()
}

#[derive(Deserialize)]
struct RetuneQuery {
    filename: String,
    #[serde(default = "default_target")]
    target: String,
}

async fn ws_retune(
    ws: WebSocketUpgrade,
    Query(q): Query<RetuneQuery>,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| retune_socket(socket, q.filename, q.target, state))
}

async fn retune_socket(mut socket: WebSocket, filename: String, _target: String, state: Arc<AppState>) {
    let dlc = match get_dlc_dir(&state) {
        Some(d) => d,
        None => {
            send_json(&mut socket, &json!({ "error": "DLC folder not configured" })).await;
            let _ = socket.close().await;
            return;
        }
    };
    let psarc_path = dlc.join(&filename);
    if !psarc_path.exists() {
        send_json(&mut socket, &json!({ "error": "File not found" })).await;
        let _ = socket.close().await;
        return;
    }
    if filename.to_lowercase().ends_with(".sloppak") || is_sloppak(&psarc_path) {
        send_json(
            &mut socket,
            &json!({ "error": "Retune is not supported for .sloppak files" }),
        )
        .await;
        let _ = socket.close().await;
        return;
    }
    // Retune pipeline (SNG decode / repack) is delegated to the native module
    // in the full port.
    send_json(
        &mut socket,
        &json!({ "error": "Retune is handled by the native pipeline (not available in this build)" }),
    )
    .await;
    let _ = socket.close().await;
}

// ── Response helpers ──────────────────────────────────────────────────────────

fn json_err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn mime_for(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("ogg") | Some("opus") | Some("oga") => Some("audio/ogg"),
        Some("mp3") => Some("audio/mpeg"),
        Some("wav") => Some("audio/wav"),
        Some("flac") => Some("audio/flac"),
        Some("m4a") => Some("audio/mp4"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("webp") => Some("image/webp"),
        Some("json") => Some("application/json"),
        _ => None,
    }
}

async fn file_response(path: &Path, content_type: Option<&str>) -> Response {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let ct = content_type
                .map(|s| s.to_string())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            ([(header::CONTENT_TYPE, ct)], bytes).into_response()
        }
        Err(_) => json_err(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn index(State(state): State<Arc<AppState>>) -> Response {
    let index = state.static_dir.join("index.html");
    match tokio::fs::read_to_string(&index).await {
        Ok(html) => Html(html).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "index.html not found").into_response(),
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // ── Configuration (mirrors the module-level constants in server.py) ──
    let dlc_dir_env = std::env::var("DLC_DIR").unwrap_or_default().trim().to_string();
    let dlc_dir = if dlc_dir_env.is_empty() {
        PathBuf::new()
    } else {
        PathBuf::from(&dlc_dir_env)
    };

    let config_dir = std::env::var("CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("rocksmith-cdlc")
        });
    std::fs::create_dir_all(&config_dir).ok();

    let art_cache_dir = config_dir.join("art_cache");
    let audio_cache_dir = config_dir.join("audio_cache");
    let sloppak_cache_dir = config_dir.join("sloppak_cache");

    // Static assets live next to the executable's project root.
    let static_dir = std::env::var("STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"));
    std::fs::create_dir_all(&static_dir).ok();

    // ── Database ──
    let meta_db = Arc::new(Mutex::new(MetadataDB::new(&config_dir)));

    // ── Plugins ──
    let mut plugin_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(user_dir) = std::env::var("SLOPSMITH_PLUGINS_DIR") {
        let p = PathBuf::from(user_dir);
        if p.is_dir() {
            plugin_dirs.push(p);
        }
    }
    let builtin_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins");
    if builtin_plugins.is_dir() {
        plugin_dirs.push(builtin_plugins);
    }
    let plugin_ctx = PluginContext {
        config_dir: config_dir.clone(),
    };
    let plugins = Arc::new(Mutex::new(load_plugins(&plugin_dirs, &plugin_ctx)));

    let state = Arc::new(AppState {
        meta_db,
        scan_status: Arc::new(Mutex::new(ScanStatus::default())),
        config_dir,
        dlc_dir_env,
        dlc_dir,
        art_cache_dir,
        audio_cache_dir,
        sloppak_cache_dir,
        static_dir: static_dir.clone(),
        plugins,
        extract_cache: Arc::new(Mutex::new(HashMap::new())),
    });

    // ── Background metadata scan + periodic rescan ──
    spawn_scan(state.clone());
    {
        let periodic_state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            loop {
                let running = { periodic_state.scan_status.lock().await.running };
                if !running {
                    run_scan(periodic_state.clone()).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            }
        });
    }

    // ── Router ──
    let mut app = Router::new()
        .route("/api/version", get(get_version))
        .route("/api/scan-status", get(scan_status))
        .route("/api/rescan", post(trigger_rescan))
        .route("/api/rescan/full", post(trigger_full_rescan))
        .route("/api/library", get(list_library))
        .route("/api/library/artists", get(list_artists))
        .route("/api/library/stats", get(library_stats))
        .route("/api/favorites/toggle", post(toggle_favorite))
        .route("/api/loops", get(list_loops).post(save_loop))
        .route("/api/loops/:loop_id", delete(delete_loop))
        .route("/api/settings", get(get_settings).post(save_settings))
        .route("/api/song/:filename/meta", post(update_song_meta))
        .route("/api/song/:filename/art", get(get_song_art))
        .route("/api/song/:filename/art/upload", post(upload_song_art_b64))
        .route("/api/song/:filename", get(get_song_info))
        .route("/api/sloppak/:filename/file/*rel_path", get(serve_sloppak_file))
        .route("/audio/*filename", get(serve_audio))
        .route("/ws/highway/*filename", get(ws_highway))
        .route("/ws/retune", get(ws_retune))
        .route("/", get(index))
        .nest_service("/static", ServeDir::new(static_dir));

    // Plugin API endpoints.
    app = register_plugin_api(app);

    let app = app.with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8000));
    println!("Rocksmith Web listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind 0.0.0.0:8000");
    axum::serve(listener, app.into_make_service())
        .await
        .expect("server error");
}
