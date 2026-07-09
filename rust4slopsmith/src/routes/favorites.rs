//! `POST /api/favorites/toggle`. Mirrors server.py:725-732.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;

pub async fn toggle_favorite(
    State(state): State<Arc<AppState>>,
    Json(data): Json<Value>,
) -> Json<Value> {
    let filename = data.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    if filename.is_empty() {
        return Json(json!({ "error": "No filename" }));
    }
    let new_state = state.db.toggle_favorite(filename);
    Json(json!({ "favorite": new_state }))
}
