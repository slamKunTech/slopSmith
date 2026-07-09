//! `GET /audio/{filename}` — serve cached audio from the writable audio cache
//! (or legacy static dir). Mirrors server.py:1664-1671.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::AppState;

pub async fn serve_audio(
    State(state): State<Arc<AppState>>,
    Path(filename): Path<String>,
) -> Response {
    for d in [state.cfg.audio_cache_dir.clone(), state.cfg.static_dir.clone()] {
        let audio_file = d.join(&filename);
        if audio_file.exists() {
            let mt = match audio_file.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase().as_str() {
                "mp3" => "audio/mpeg",
                "ogg" | "opus" | "oga" => "audio/ogg",
                "wav" => "audio/wav",
                "flac" => "audio/flac",
                _ => "application/octet-stream",
            };
            if let Ok(bytes) = std::fs::read(&audio_file) {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, mt.to_string())],
                    Bytes::from(bytes),
                )
                    .into_response();
            }
        }
    }
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}
