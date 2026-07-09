//! `GET /api/song/{rest}`, `GET .../art`, `POST .../meta`, `POST .../art/upload`.
//! Mirrors server.py:1029-1169. axum's catch-all must be the last segment, so
//! a single `{*rest}` route dispatches on the suffix (`/art`, `/meta`,
//! `/art/upload`, or bare filename).

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use serde_json::{json, Value};

use crate::engine::psarc;
use crate::engine::sloppak;
use crate::scan::extract_meta_for_file;
use crate::state::AppState;

/// `GET /api/song/{*rest}` — dispatch: bare filename → song info; `…/art` → art.
pub async fn song_get(
    State(state): State<Arc<AppState>>,
    Path(rest): Path<String>,
) -> Response {
    if let Some(filename) = rest.strip_suffix("/art") {
        return get_song_art(state, filename.to_string()).await.into_response();
    }
    get_song_info(state, rest).await.into_response()
}

/// `POST /api/song/{*rest}` — dispatch: `…/meta` → update meta; `…/art/upload`
/// → upload art.
pub async fn song_post(
    State(state): State<Arc<AppState>>,
    Path(rest): Path<String>,
    body: axum::body::Body,
) -> Response {
    if let Some(filename) = rest.strip_suffix("/meta") {
        let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap_or_default();
        let data: Value = serde_json::from_slice(&bytes[..]).unwrap_or(Value::Null);
        return update_song_meta(state, filename.to_string(), data).await.into_response();
    }
    if let Some(filename) = rest.strip_suffix("/art/upload") {
        let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap_or_default();
        let data: Value = serde_json::from_slice(&bytes[..]).unwrap_or(Value::Null);
        return upload_song_art(state, filename.to_string(), data).await.into_response();
    }
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}

/// `GET /api/song/{filename}` — song metadata, cached or freshly extracted.
/// Mirrors server.py:1145-1169.
pub async fn get_song_info(state: Arc<AppState>, filename: String) -> Json<Value> {
    let dlc = match state.cfg.get_dlc_dir() {
        Some(d) => d,
        None => return Json(json!({ "error": "DLC folder not configured" })),
    };
    let psarc_path = dlc.join(&filename);
    if !psarc_path.exists() {
        return Json(json!({ "error": "File not found" }));
    }
    let stat = match std::fs::metadata(&psarc_path) {
        Ok(s) => s,
        Err(_) => return Json(json!({ "error": "File not found" })),
    };
    let mtime = file_mtime(&stat);
    let size = stat.len() as i64;

    if let Some(cached) = state.db.get(&filename, mtime, size) {
        return Json(cached.to_json());
    }

    let st = state.clone();
    let meta = tokio::task::spawn_blocking(move || {
        let meta = extract_meta_for_file(&psarc_path);
        if let Some(m) = &meta {
            let _ = st.db.put(&filename, mtime, size, m);
        }
        meta
    })
    .await
    .ok()
    .flatten();

    match meta {
        Some(m) => Json(m.to_json()),
        None => Json(json!({ "error": "Could not extract metadata" })),
    }
}

/// `GET /api/song/{filename}/art` — album art: sloppak cover, or PSARC DDS→PNG.
/// Mirrors server.py:1029-1090.
pub async fn get_song_art(state: Arc<AppState>, filename: String) -> Response {
    let dlc = match state.cfg.get_dlc_dir() {
        Some(d) => d,
        None => return (StatusCode::NOT_FOUND, Json(json!({ "error": "not configured" }))).into_response(),
    };
    let psarc_path = dlc.join(&filename);
    if !psarc_path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response();
    }

    // Sloppak: serve the manifest-declared cover (default cover.jpg).
    if sloppak::is_sloppak(&psarc_path) {
        if let Ok(src) = sloppak::resolve_source_dir(&filename, &dlc, &state.cfg.sloppak_cache_dir) {
            if let Ok(manifest) = sloppak::load_manifest(&psarc_path) {
                let cover_rel = manifest.get("cover").and_then(|v| v.as_str()).unwrap_or("cover.jpg").to_string();
                let cover_path = src.join(&cover_rel);
                // Path-traversal guard.
                if !cover_path.starts_with(&src) {
                    return (StatusCode::FORBIDDEN, Json(json!({ "error": "forbidden" }))).into_response();
                }
                if cover_path.is_file() {
                    let mt = match cover_path.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase().as_str() {
                        "png" => "image/png",
                        "webp" => "image/webp",
                        _ => "image/jpeg",
                    };
                    return serve_file(&cover_path, mt);
                }
            }
        }
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "no art" }))).into_response();
    }

    // PSARC: DDS → PNG, cached.
    let _ = std::fs::create_dir_all(&state.cfg.art_cache_dir);
    let safe_name = filename.replace('/', "_").replace(' ', "_");
    let cached = state.cfg.art_cache_dir.join(format!("{safe_name}.png"));
    if cached.exists() {
        return serve_file(&cached, "image/png");
    }

    let cached_clone = cached.clone();
    let ppath = psarc_path.clone();
    let extracted = tokio::task::spawn_blocking(move || -> Option<PathBuf> {
        let tmp = mkdtemp("rs_art_");
        let res = (|| -> Option<PathBuf> {
            psarc::unpack_psarc(&ppath, &tmp).ok()?;
            let mut dds_files: Vec<(u64, PathBuf)> = walkdir::WalkDir::new(&tmp)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("dds"))
                .filter_map(|e| e.metadata().ok().map(|m| (m.len(), e.path().to_path_buf())))
                .collect();
            dds_files.sort_by(|a, b| b.0.cmp(&a.0));
            let dds = dds_files.first()?.1.clone();
            dds_to_png(&dds, &cached_clone)
        })();
        std::fs::remove_dir_all(&tmp).ok();
        res
    })
    .await
    .ok()
    .flatten();

    match extracted {
        Some(p) => serve_file(&p, "image/png"),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "no art" }))).into_response(),
    }
}

/// `POST /api/song/{filename}/meta` — update cached title/artist/album/year.
/// Mirrors server.py:1093-1110.
pub async fn update_song_meta(state: Arc<AppState>, filename: String, data: Value) -> Json<Value> {
    let mut updates: Vec<String> = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    for field in ["title", "artist", "album", "year"] {
        if let Some(v) = data.get(field) {
            updates.push(format!("{field} = ?"));
            let p = match v {
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                Value::Number(n) => n.as_i64().map(rusqlite::types::Value::Integer).unwrap_or(rusqlite::types::Value::Text(v.to_string())),
                _ => rusqlite::types::Value::Text(v.to_string()),
            };
            params.push(p);
        }
    }
    if updates.is_empty() {
        return Json(json!({ "error": "No fields to update" }));
    }
    params.push(rusqlite::types::Value::Text(filename));
    let sql = format!("UPDATE songs SET {} WHERE filename = ?", updates.join(", "));
    if let Err(e) = state.db.execute_sql(&sql, &params) {
        return Json(json!({ "error": e.to_string() }));
    }
    Json(json!({ "ok": true }))
}

/// `POST /api/song/{filename}/art/upload` — base64 PNG/JPG → cached PNG.
/// Mirrors server.py:1113-1142.
pub async fn upload_song_art(state: Arc<AppState>, filename: String, data: Value) -> Json<Value> {
    let b64 = data.get("image").and_then(|v| v.as_str()).unwrap_or("");
    if b64.is_empty() {
        return Json(json!({ "error": "No image data" }));
    }
    // Strip data URL prefix.
    let b64 = if let Some(idx) = b64.find(',') { &b64[idx + 1..] } else { b64 };
    let img_data = match base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
        Ok(d) => d,
        Err(_) => return Json(json!({ "error": "Invalid base64" })),
    };

    let _ = std::fs::create_dir_all(&state.cfg.art_cache_dir);
    let safe_name = filename.replace('/', "_").replace(' ', "_");
    let cached = state.cfg.art_cache_dir.join(format!("{safe_name}.png"));

    // Decode + convert to PNG.
    match image::load_from_memory(&img_data) {
        Ok(img) => match img.to_rgb8().save(&cached) {
            Ok(_) => Json(json!({ "ok": true })),
            Err(e) => Json(json!({ "error": format!("Invalid image: {e}") })),
        },
        Err(e) => Json(json!({ "error": format!("Invalid image: {e}") })),
    }
}

/// Decode a DDS to PNG. Tries the `image` crate first; falls back to `sips`
/// (macOS) / `ffmpeg` for DXT-compressed DDS the `image` crate can't read.
fn dds_to_png(dds: &std::path::Path, png: &std::path::Path) -> Option<PathBuf> {
    if let Ok(img) = image::open(dds) {
        if img.to_rgb8().save(png).is_ok() {
            return Some(png.to_path_buf());
        }
    }
    // Fallback: sips (macOS) — `sips -s format png input.dds --out out.png`.
    if let Ok(out) = std::process::Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(dds)
        .args(["--out"]).arg(png)
        .output()
    {
        if out.status.success() && png.exists() {
            return Some(png.to_path_buf());
        }
    }
    // Fallback: ffmpeg.
    if let Ok(out) = std::process::Command::new("ffmpeg")
        .args(["-y", "-i"]).arg(dds).arg(png)
        .output()
    {
        if out.status.success() && png.exists() {
            return Some(png.to_path_buf());
        }
    }
    None
}

fn serve_file(path: &std::path::Path, mt: &str) -> Response {
    match std::fs::read(path) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mt.to_string())],
            Bytes::from(bytes),
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
    }
}

fn file_mtime(meta: &std::fs::Metadata) -> f64 {
    use std::time::SystemTime;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn mkdtemp(prefix: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("{prefix}{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).ok();
    dir
}
