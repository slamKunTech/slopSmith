//! `GET /` and `/static/*`. Mirrors server.py:1674-1679.
//!
//! `/` serves `static/index.html`; `/static/*` is mounted via
//! [`tower_http::services::ServeDir`] in [`crate::routes::router`]. Audio
//! serving (`GET /audio/{filename}`) lands in Wave 5.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

pub async fn index(State(state): State<Arc<AppState>>) -> Response {
    let path = state.cfg.static_dir.join("index.html");
    match std::fs::read(&path) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "index.html not found").into_response(),
    }
}
