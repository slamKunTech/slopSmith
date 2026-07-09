//! `WS /ws/highway/{filename}` — streams song data for the highway renderer.
//! Port of `highway_ws` (server.py:1240-1658). The message sequence is the
//! load-bearing contract with the frontend (see CLAUDE.md "WebSocket Protocol
//! Reference"): `loading` → `song_info` → `beats` → `sections` → `anchors` →
//! `chord_templates` → [`lyrics`] → [`tone_changes`] → `notes`(chunked 500) →
//! `chords`(chunked 500) → [`phrases`(chunked 20)] → `ready`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::IntoResponse;
use serde_json::{json, Map, Value};

use crate::engine::audio::{convert_wem, find_wem_files};
use crate::engine::sng_vocals::parse_vocals_sng;
use crate::engine::song::{
    arrangement_string_count, note_to_wire, phrase_to_wire,
    Arrangement, Song,
};
use crate::engine::sloppak::{self, SloppakStem};
use crate::state::AppState;

#[derive(serde::Deserialize)]
pub struct HighwayQuery {
    #[serde(default = "default_arrangement")]
    arrangement: i64,
}
fn default_arrangement() -> i64 {
    -1
}

pub async fn highway_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    AxumPath(filename): AxumPath<String>,
    Query(q): Query<HighwayQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| run_highway(socket, state, filename, q.arrangement))
}

/// Send a JSON value as a Text frame.
async fn send_json(socket: &mut WebSocket, v: &Value) -> bool {
    let text = serde_json::to_string(v).unwrap_or_else(|_| "{}".into());
    socket.send(Message::Text(text.into())).await.is_ok()
}

async fn send_error(socket: &mut WebSocket, msg: &str) {
    let _ = send_json(socket, &json!({ "error": msg })).await;
    // Dropping the socket (function return) closes the connection.
}

#[allow(clippy::too_many_arguments)]
async fn run_highway(mut socket: WebSocket, state: Arc<AppState>, filename: String, arrangement: i64) {
    let dlc = match state.cfg.get_dlc_dir() {
        Some(d) => d,
        None => return send_error(&mut socket, "DLC folder not configured").await,
    };
    let psarc_path = dlc.join(&filename);
    if !psarc_path.exists() {
        return send_error(&mut socket, "File not found").await;
    }

    let is_slop = sloppak::is_sloppak(&psarc_path);

    // Send "Extracting..." and keep a 3s keepalive running during the load.
    let _ = send_json(&mut socket, &json!({ "type": "loading", "stage": "Extracting..." })).await;

    // Load (extract + parse) on a blocking thread, racing a 3s keepalive.
    let sloppak_cache = state.cfg.sloppak_cache_dir.clone();
    let dlc2 = dlc.clone();
    let st = state.clone();
    let fname = filename.clone();
    let ppath = psarc_path.clone();
    let mut load_handle = tokio::task::spawn_blocking(move || {
        if is_slop {
            let _ = std::fs::create_dir_all(&sloppak_cache);
            match sloppak::load_song(&fname, &dlc2, &sloppak_cache) {
                Ok(loaded) => LoadResult::Sloppak(loaded),
                Err(_) => LoadResult::Error("Failed to load sloppak".to_string()),
            }
        } else {
            match crate::caches::get_or_extract(&st.extract_cache, &fname, &ppath) {
                Ok((tmp, song, _is_new)) => LoadResult::Psarc { tmp, song },
                Err(_) => LoadResult::Error("Failed to extract PSARC".to_string()),
            }
        }
    });

    let mut interval = tokio::time::interval(Duration::from_secs(3));
    // tokio::time::interval's first tick completes immediately; Python's
    // keepalive task sleeps 3s before the first send, so discard the first
    // tick to match (no spurious keepalive when load finishes <3s).
    interval.tick().await;
    let load = loop {
        tokio::select! {
            _ = interval.tick() => {
                let _ = send_json(&mut socket, &json!({ "type": "loading", "stage": "Loading..." })).await;
            }
            res = &mut load_handle => {
                match res {
                    Ok(lr) => match lr {
                        LoadResult::Error(e) => return send_error(&mut socket, &e).await,
                        _ => break lr,
                    },
                    Err(_) => return send_error(&mut socket, "load failed").await,
                }
            }
        }
    };

    // Pull the pieces out of the load result.
    let (song, tmp, stems) = match load {
        LoadResult::Sloppak(loaded) => {
            let tmp = loaded.source_dir.to_string_lossy().to_string();
            (loaded.song, tmp, loaded.stems)
        }
        LoadResult::Psarc { tmp, song } => (song.as_ref().clone(), tmp.to_string_lossy().to_string(), Vec::new()),
        LoadResult::Error(e) => return send_error(&mut socket, &e).await,
    };

    if song.arrangements.is_empty() {
        return send_error(&mut socket, "No arrangements found").await;
    }

    // Pick arrangement: explicit > config default_arrangement > most notes.
    let mut best: i64 = -1;
    if 0 <= arrangement && (arrangement as usize) < song.arrangements.len() {
        best = arrangement;
    } else {
        let pref = state
            .settings
            .lock()
            .unwrap()
            .get("default_arrangement")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
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
        let mut best_count = 0;
        for (i, a) in song.arrangements.iter().enumerate() {
            let c = a.notes.len() + a.chords.iter().map(|ch| ch.notes.len()).sum::<usize>();
            if c > best_count {
                best_count = c;
                best = i as i64;
            }
        }
    }
    let arr = song.arrangements[best as usize].clone();

    // Audio.
    let audio_id = Path::new(&filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .replace(' ', "_");
    let mut audio_url: Option<String> = None;
    let mut audio_error: Option<String> = None;
    let mut stems_payload: Vec<Value> = Vec::new();

    if is_slop {
        let q_fn = url_encode(&filename, false);
        for s in &stems {
            let url = format!("/api/sloppak/{}/file/{}", q_fn, url_encode(&s.file, true));
            stems_payload.push(json!({ "id": s.id, "url": url, "default": s.default }));
        }
        if let Some(first) = stems_payload.first() {
            audio_url = first.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());
        } else {
            audio_error = Some("This sloppak has no playable stems.".into());
        }
    } else {
        let _ = std::fs::create_dir_all(&state.cfg.audio_cache_dir);
        for ext in [".mp3", ".ogg", ".wav"] {
            for cache_dir in [state.cfg.audio_cache_dir.clone(), state.cfg.static_dir.clone()] {
                let cached_audio = cache_dir.join(format!("audio_{audio_id}{ext}"));
                if let Ok(meta) = std::fs::metadata(&cached_audio) {
                    if meta.len() > 1000 {
                        audio_url = Some(format!("/audio/audio_{audio_id}{ext}"));
                        break;
                    }
                }
            }
            if audio_url.is_some() {
                break;
            }
        }
    }

    if audio_url.is_none() && !is_slop {
        let _ = send_json(&mut socket, &json!({ "type": "loading", "stage": "Converting audio..." })).await;
        let tmp_path = PathBuf::from(&tmp);
        let audio_cache = state.cfg.audio_cache_dir.clone();
        let mut audio_handle = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let wem_files = find_wem_files(&tmp_path);
            if wem_files.is_empty() {
                return Err("No WEM audio files were found inside this PSARC.".into());
            }
            let base = tmp_path.join("audio");
            let converted = convert_wem(&wem_files[0], &base).map_err(|e| format!("Audio conversion failed: {e}"))?;
            let ext = converted
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("wav")
                .to_string();
            let dest = audio_cache.join(format!("audio_{audio_id}.{ext}"));
            std::fs::copy(&converted, &dest).map_err(|e| format!("copy failed: {e}"))?;
            Ok(format!("/audio/audio_{audio_id}.{ext}"))
        });
        // Race conversion against keepalives.
        let mut ki = tokio::time::interval(Duration::from_secs(3));
        ki.tick().await; // discard immediate first tick (matches Python)
        let conv = loop {
            tokio::select! {
                _ = ki.tick() => {
                    let _ = send_json(&mut socket, &json!({ "type": "loading", "stage": "Loading..." })).await;
                }
                res = &mut audio_handle => {
                    break match res {
                        Ok(Ok(url)) => Ok(url),
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err("Audio conversion failed".to_string()),
                    };
                }
            }
        };
        match conv {
            Ok(url) => audio_url = Some(url),
            Err(e) => audio_error = Some(e),
        }
        // Clean up old audio cache files (keep max 100).
        prune_audio_cache(&state.cfg.audio_cache_dir);
    }

    // ── song_info ─────────────────────────────────────────────────────────
    let arr_list: Vec<Value> = song
        .arrangements
        .iter()
        .enumerate()
        .map(|(i, a)| {
            json!({
                "index": i,
                "name": a.name,
                "notes": a.notes.len() + a.chords.iter().map(|c| c.notes.len()).sum::<usize>(),
            })
        })
        .collect();
    let song_info = json!({
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
        "stringCount": arrangement_string_count(&arr),
        "capo": arr.capo,
        "format": if is_slop { "sloppak" } else { "psarc" },
        "stems": stems_payload,
    });
    let _ = send_json(&mut socket, &song_info).await;

    // beats
    let beats: Vec<Value> = song.beats.iter().map(|b| json!({ "time": b.time, "measure": b.measure })).collect();
    let _ = send_json(&mut socket, &json!({ "type": "beats", "data": beats })).await;

    // sections
    let sections: Vec<Value> = song.sections.iter().map(|s| json!({ "name": s.name, "time": s.start_time })).collect();
    let _ = send_json(&mut socket, &json!({ "type": "sections", "data": sections })).await;

    // anchors
    let anchors: Vec<Value> = arr.anchors.iter().map(|a| json!({ "time": a.time, "fret": a.fret, "width": a.width })).collect();
    let _ = send_json(&mut socket, &json!({ "type": "anchors", "data": anchors })).await;

    // chord_templates
    let templates: Vec<Value> = arr
        .chord_templates
        .iter()
        .map(|ct| json!({ "name": ct.name, "fingers": ct.fingers, "frets": ct.frets }))
        .collect();
    let _ = send_json(&mut socket, &json!({ "type": "chord_templates", "data": templates })).await;

    // lyrics
    let lyrics = collect_lyrics(&song, is_slop, &tmp);
    if !lyrics.is_empty() {
        let _ = send_json(&mut socket, &json!({ "type": "lyrics", "data": lyrics })).await;
    }

    // tone_changes (PSARC only)
    if !is_slop {
        let tone = collect_tone_changes(&arr, &tmp);
        if let Some(tone) = tone {
            let _ = send_json(&mut socket, &tone).await;
        }
    }

    // notes (chunked 500) — use note_to_wire verbatim (matches frontend field set).
    let notes: Vec<Value> = arr.notes.iter().map(note_to_wire).collect();
    let total_notes = notes.len();
    for chunk in notes.chunks(500) {
        let _ = send_json(&mut socket, &json!({ "type": "notes", "data": chunk, "total": total_notes })).await;
    }

    // chords (chunked 500). Built inline to match server.py:1590-1607's
    // field order — note `bn` comes BEFORE `sl`/`slu` here, which differs
    // from `chord_note_to_wire` (used by the sloppak wire format + phrases).
    let chords: Vec<Value> = arr
        .chords
        .iter()
        .map(|c| {
            let chord_notes: Vec<Value> = c
                .notes
                .iter()
                .map(|cn| {
                    let mut m = Map::new();
                    m.insert("s".into(), json!(cn.string));
                    m.insert("f".into(), json!(cn.fret));
                    m.insert("sus".into(), json!(crate::engine::song::round_dp(cn.sustain, 3)));
                    m.insert(
                        "bn".into(),
                        if cn.bend != 0.0 {
                            json!(crate::engine::song::round_dp(cn.bend, 1))
                        } else {
                            json!(0)
                        },
                    );
                    m.insert("sl".into(), json!(cn.slide_to));
                    m.insert("slu".into(), json!(cn.slide_unpitch_to));
                    m.insert("ho".into(), json!(cn.hammer_on));
                    m.insert("po".into(), json!(cn.pull_off));
                    m.insert("hm".into(), json!(cn.harmonic));
                    m.insert("hp".into(), json!(cn.harmonic_pinch));
                    m.insert("pm".into(), json!(cn.palm_mute));
                    m.insert("mt".into(), json!(cn.mute));
                    m.insert("tr".into(), json!(cn.tremolo));
                    m.insert("ac".into(), json!(cn.accent));
                    m.insert("tp".into(), json!(cn.tap));
                    Value::Object(m)
                })
                .collect();
            json!({
                "t": crate::engine::song::round_dp(c.time, 3),
                "id": c.chord_id,
                "hd": c.high_density,
                "notes": chord_notes,
            })
        })
        .collect();
    let total_chords = chords.len();
    for chunk in chords.chunks(500) {
        let _ = send_json(&mut socket, &json!({ "type": "chords", "data": chunk, "total": total_chords })).await;
    }

    // phrases (chunked 20) — only when present.
    if let Some(phrases) = &arr.phrases {
        let pv: Vec<Value> = phrases.iter().map(phrase_to_wire).collect();
        let total = pv.len();
        for chunk in pv.chunks(20) {
            let _ = send_json(&mut socket, &json!({ "type": "phrases", "data": chunk, "total": total })).await;
        }
    }

    let _ = send_json(&mut socket, &json!({ "type": "ready" })).await;

    // Keep connection alive for control messages (change_arrangement is a no-op,
    // matching Python server.py:1641-1644).
    while socket.recv().await.is_some() {}
}

enum LoadResult {
    Sloppak(sloppak::LoadedSloppak),
    Psarc { tmp: PathBuf, song: Arc<Song> },
    Error(String),
}

/// Collect lyrics: sloppak uses song.lyrics; PSARC reads a vocals XML, falling
/// back to the vocals SNG (official DLC).
fn collect_lyrics(song: &Song, is_slop: bool, tmp: &str) -> Vec<Value> {
    if is_slop {
        return song.lyrics.clone();
    }
    // Vocals XML first.
    let mut xml_files: Vec<PathBuf> = rglob(tmp, "xml");
    xml_files.sort();
    for xml_path in &xml_files {
        if let Ok(text) = std::fs::read_to_string(xml_path) {
            if let Ok(doc) = roxmltree::Document::parse(&text) {
                let root = doc.root_element();
                if root.tag_name().name() == "vocals" {
                    let mut out: Vec<Value> = Vec::new();
                    for v in root.children().filter(|c| c.is_element() && c.tag_name().name() == "vocal") {
                        out.push(json!({
                            "t": crate::engine::song::round_dp(v.attribute("time").and_then(|s| s.parse().ok()).unwrap_or(0.0), 3),
                            "d": crate::engine::song::round_dp(v.attribute("length").and_then(|s| s.parse().ok()).unwrap_or(0.0), 3),
                            "w": v.attribute("lyric").unwrap_or(""),
                        }));
                    }
                    return out;
                }
            }
        }
    }
    // SNG-only fallback.
    let mut sng_files: Vec<PathBuf> = rglob(tmp, "sng");
    sng_files.sort();
    for sng_path in &sng_files {
        let lower = sng_path.to_string_lossy().to_lowercase();
        if !lower.contains("vocals") {
            continue;
        }
        let plat = if lower.replace('\\', "/").contains("/macos/") { "mac" } else { "pc" };
        let lyrics = parse_vocals_sng(sng_path, plat);
        if !lyrics.is_empty() {
            return lyrics;
        }
    }
    Vec::new()
}

/// Build the `tone_changes` message from the arrangement-matching XML + the
/// tone ID→name map from the manifest JSON. PSARC only.
fn collect_tone_changes(arr: &Arrangement, tmp: &str) -> Option<Value> {
    let arr_name_lower = arr.name.to_lowercase();

    // tone ID→name map from manifest JSON (prefer arrangement-matching file).
    let mut tone_id_map: HashMap<i64, String> = HashMap::new();
    let mut json_files: Vec<PathBuf> = rglob(tmp, "json");
    json_files.sort();
    // Prefer arrangement-matching manifest.
    json_files.sort_by_key(|jf| {
        let stem = jf.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
        !(arr_name_lower.is_empty() || stem.contains(&arr_name_lower)) as i8
    });
    for jf in &json_files {
        let Ok(text) = std::fs::read_to_string(jf) else { continue };
        let Ok(data) = serde_json::from_str::<Value>(&text) else { continue };
        if let Some(entries) = data.get("Entries").and_then(|v| v.as_object()) {
            for (_k, entry) in entries.iter() {
                let Some(attrs) = entry.get("Attributes") else { continue };
                for (idx, key) in ["Tone_A", "Tone_B", "Tone_C", "Tone_D"].iter().enumerate() {
                    if let Some(val) = attrs.get(*key).and_then(|v| v.as_str()) {
                        if !val.is_empty() {
                            tone_id_map.insert(idx as i64, val.to_string());
                        }
                    }
                }
                if !tone_id_map.is_empty() {
                    break;
                }
            }
        }
        if !tone_id_map.is_empty() {
            break;
        }
    }

    // Parse XMLs (prefer arrangement-matching).
    let mut xml_files: Vec<PathBuf> = rglob(tmp, "xml");
    xml_files.sort();
    xml_files.sort_by_key(|xp| {
        let stem = xp.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
        !(arr_name_lower.is_empty() || stem.contains(&arr_name_lower)) as i8
    });

    for xml_path in &xml_files {
        let Ok(text) = std::fs::read_to_string(xml_path) else { continue };
        let Ok(doc) = roxmltree::Document::parse(&text) else { continue };
        let root = doc.root_element();
        if root.tag_name().name() != "song" {
            continue;
        }
        if let Some(tones_el) = root.children().find(|c| c.is_element() && c.tag_name().name() == "tones") {
            let mut tone_changes: Vec<Value> = Vec::new();
            for t in tones_el.children().filter(|c| c.is_element() && c.tag_name().name() == "tone") {
                let tc_time = t.attribute("time");
                let mut tc_name = t.attribute("name").unwrap_or("").to_string();
                let tc_id = t.attribute("id").unwrap_or("");
                if (tc_name.is_empty() || tc_name == "N/A") && !tc_id.is_empty() {
                    let id: i64 = tc_id.parse().unwrap_or(-1);
                    tc_name = tone_id_map.get(&id).cloned().unwrap_or_else(|| format!("Tone {tc_id}"));
                }
                if let Some(time) = tc_time {
                    if !tc_name.is_empty() {
                        tone_changes.push(json!({
                            "t": crate::engine::song::round_dp(time.parse::<f64>().unwrap_or(0.0), 3),
                            "name": tc_name,
                        }));
                    }
                }
            }
            if !tone_changes.is_empty() {
                let mut base_name = root
                    .children()
                    .find(|c| c.is_element() && c.tag_name().name() == "tonebase")
                    .and_then(|tb| tb.text())
                    .unwrap_or("")
                    .to_string();
                if base_name.is_empty() {
                    base_name = tone_id_map.get(&0).cloned().unwrap_or_default();
                }
                tone_changes.sort_by(|a, b| {
                    a["t"].as_f64().unwrap_or(0.0).partial_cmp(&b["t"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal)
                });
                return Some(json!({ "type": "tone_changes", "base": base_name, "data": tone_changes }));
            }
        }
    }
    None
}

/// Keep at most 100 audio cache files (LRU by atime).
fn prune_audio_cache(audio_cache: &Path) {
    let Ok(entries) = std::fs::read_dir(audio_cache) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with("audio_") && matches!(ext, "mp3" | "ogg" | "wav") {
            if let Ok(meta) = entry.metadata() {
                let atime = meta.accessed().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                files.push((atime, p));
            }
        }
    }
    if files.len() > 100 {
        files.sort_by_key(|(t, _)| *t);
        for (_, p) in files.iter().take(files.len() - 100) {
            std::fs::remove_file(p).ok();
        }
    }
}

fn rglob(dir: &str, ext: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if e.path().extension().and_then(|x| x.to_str()) == Some(ext) {
            v.push(e.path().to_path_buf());
        }
    }
    v
}

/// URL-encode a path component, mirroring `urllib.parse.quote`. `safe_slash`
/// = true keeps `/` unencoded (Python's default `safe='/'`, used for stem
/// file paths); false encodes everything (Python's `safe=''`, used for the
/// sloppak filename).
fn url_encode(s: &str, safe_slash: bool) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' || (safe_slash && c == '/') {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}
