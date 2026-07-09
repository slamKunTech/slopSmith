//! `GET /api/sloppak/{filename}/file/{rel_path}` — serve a file from inside a
//! sloppak (stems, cover, arrangement JSON). Mirrors server.py:1208-1237.
//! axum's catch-all must be the last segment, so a single `{*rest}` route
//! parses `{filename}/file/{rel_path}` by splitting on the first `/file/`.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::engine::sloppak;
use crate::state::AppState;

pub async fn serve_sloppak_file(
    State(state): State<Arc<AppState>>,
    Path(rest): Path<String>,
) -> Response {
    // Split into {filename} + {rel_path} on the first "/file/" separator.
    let (filename, rel_path) = match rest.split_once("/file/") {
        Some((f, r)) => (f.to_string(), r.to_string()),
        None => return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
    };

    // Resolve the sloppak's source dir (cached, or fresh).
    let dlc = match state.cfg.get_dlc_dir() {
        Some(d) => d,
        None => return (StatusCode::NOT_FOUND, Json(json!({ "error": "not configured" }))).into_response(),
    };
    let src = match sloppak::get_cached_source_dir(&filename) {
        Some(s) => s,
        None => match sloppak::resolve_source_dir(&filename, &dlc, &state.cfg.sloppak_cache_dir) {
            Ok(s) => s,
            Err(_) => return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        },
    };

    // Path-traversal guard.
    let target = src.join(&rel_path);
    if !target.starts_with(&src) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "forbidden" }))).into_response();
    }
    if !target.is_file() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response();
    }

    let mt = match target.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase().as_str() {
        "ogg" | "opus" | "oga" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "json" => "application/json",
        _ => "application/octet-stream",
    };
    match std::fs::read(&target) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mt.to_string())],
            Bytes::from(bytes),
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
    }
}
