//! `GET /api/library`, `/api/library/artists`, `/api/library/stats`. Mirrors
//! server.py:697-722.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;

/// Look up a query param, falling back to `default`. Returns an owned String
/// so the caller isn't pinned to the query map's borrow lifetime.
fn get(q: &HashMap<String, String>, k: &str, default: &str) -> String {
    q.get(k).cloned().unwrap_or_else(|| default.to_string())
}

pub async fn list_library(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let page = get(&q, "page", "0").parse::<i64>().unwrap_or(0);
    let size = get(&q, "size", "24").parse::<i64>().unwrap_or(24).clamp(1, 100);
    let sort = get(&q, "sort", "artist");
    let dir = get(&q, "dir", "asc");
    let favorites = get(&q, "favorites", "0").parse::<i64>().unwrap_or(0) != 0;
    let fmt = q
        .get("format")
        .map(|s| s.as_str())
        .filter(|s| matches!(*s, "psarc" | "sloppak"))
        .unwrap_or("");
    let qstr = get(&q, "q", "");
    let (songs, total) = state
        .db
        .query_page(&qstr, page, size, &sort, &dir, favorites, fmt)
        .unwrap_or((Value::Array(vec![]), 0));
    Json(json!({ "songs": songs, "total": total, "page": page, "size": size }))
}

pub async fn list_artists(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let letter = get(&q, "letter", "");
    let qstr = get(&q, "q", "");
    let favorites = get(&q, "favorites", "0").parse::<i64>().unwrap_or(0) != 0;
    let page = get(&q, "page", "0").parse::<i64>().unwrap_or(0);
    let size = get(&q, "size", "50").parse::<i64>().unwrap_or(50).clamp(1, 100);
    let fmt = q
        .get("format")
        .map(|s| s.as_str())
        .filter(|s| matches!(*s, "psarc" | "sloppak"))
        .unwrap_or("");
    let (artists, total) = state
        .db
        .query_artists(&letter, &qstr, favorites, page, size, fmt)
        .unwrap_or((Value::Array(vec![]), 0));
    Json(json!({ "artists": artists, "total_artists": total, "page": page, "size": size }))
}

pub async fn library_stats(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let favorites = get(&q, "favorites", "0").parse::<i64>().unwrap_or(0) != 0;
    Json(state.db.query_stats(favorites))
}
