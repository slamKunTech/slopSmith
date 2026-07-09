//! `GET/POST /api/loops`, `DELETE /api/loops/{loop_id}`. Mirrors
//! server.py:737-773.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};
use serde::Deserialize;
use std::collections::HashMap;

use crate::state::AppState;

pub async fn list_loops(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let filename = q.get("filename").map(|s| s.as_str()).unwrap_or("");
    let loops = state.db.list_loops(filename);
    Json(Value::Array(loops))
}

pub async fn save_loop(
    State(state): State<Arc<AppState>>,
    Json(data): Json<Value>,
) -> Json<Value> {
    let filename = data.get("filename").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let start = data.get("start").and_then(|v| v.as_f64());
    let end = data.get("end").and_then(|v| v.as_f64());
    let (start, end) = match (start, end) {
        (Some(s), Some(e)) => (s, e),
        _ => return Json(json!({ "error": "Missing fields" })),
    };
    match state.db.save_loop(&filename, &name, start, end) {
        Ok((ok, name)) => Json(json!({ "ok": ok, "name": name })),
        Err(e) => Json(json!({ "error": e })),
    }
}

#[derive(Deserialize)]
pub struct LoopId {
    pub loop_id: i64,
}

pub async fn delete_loop(
    State(state): State<Arc<AppState>>,
    Path(LoopId { loop_id }): Path<LoopId>,
) -> Json<Value> {
    let ok = state.db.delete_loop(loop_id);
    Json(json!({ "ok": ok }))
}
