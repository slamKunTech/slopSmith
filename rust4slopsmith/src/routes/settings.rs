//! `GET/POST /api/settings`. Mirrors server.py:778-899.
//!
//! Config is a freeform JSON object (preserving unknown keys for forward
//! compatibility), with the known keys validated/coerced exactly as the Python
//! backend does: `dlc_dir`, `default_arrangement`, `demucs_server_url`,
//! `master_difficulty`, `av_offset_ms`.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};

use crate::state::AppState;

pub async fn get_settings(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = state
        .cfg
        .load_config()
        .map(Value::Object)
        .unwrap_or_else(|| state.cfg.default_settings());
    Json(cfg)
}

pub async fn save_settings(
    State(state): State<Arc<AppState>>,
    Json(data): Json<Value>,
) -> Json<Value> {
    let data = match data.as_object() {
        Some(m) => m.clone(),
        None => return Json(json!({ "error": "Invalid body" })),
    };

    // Ensure CONFIG_DIR exists.
    if let Err(e) = std::fs::create_dir_all(&state.cfg.config_dir) {
        return Json(json!({ "error": format!("Cannot create config dir: {e}") }));
    }

    let mut cfg: Map<String, Value> = state.cfg.load_config().unwrap_or_else(|| {
        state
            .cfg
            .default_settings()
            .as_object()
            .cloned()
            .unwrap_or_default()
    });

    let mut messages: Vec<String> = Vec::new();

    // dlc_dir
    if let Some(v) = data.get("dlc_dir") {
        match v {
            Value::Null => { /* no-op: preserve on-disk value */ }
            Value::String(s) => {
                if s.is_empty() {
                    cfg.insert("dlc_dir".into(), Value::String(String::new()));
                } else {
                    let p = std::path::Path::new(s);
                    if p.is_dir() {
                        cfg.insert("dlc_dir".into(), Value::String(s.clone()));
                        // Count .psarc files in the dir (matches Python's hint message).
                        let count = std::fs::read_dir(p)
                            .map(|entries| {
                                entries
                                    .filter_map(|e| e.ok())
                                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("psarc"))
                                    .count()
                            })
                            .unwrap_or(0);
                        messages.push(format!("DLC folder: {count} .psarc files found"));
                    } else {
                        return Json(json!({ "error": format!("DLC directory not found: {s}") }));
                    }
                }
            }
            _ => return Json(json!({ "error": "dlc_dir must be a string path or empty" })),
        }
    }

    // default_arrangement + demucs_server_url
    for key in ["default_arrangement", "demucs_server_url"] {
        if let Some(v) = data.get(key) {
            match v {
                Value::Null => { /* no-op */ }
                Value::String(s) => {
                    cfg.insert(key.into(), Value::String(s.clone()));
                }
                _ => return Json(json!({ "error": format!("{key} must be a string or empty") })),
            }
        }
    }

    // master_difficulty
    if let Some(v) = data.get("master_difficulty") {
        // Reject bool explicitly: serde_json keeps bool distinct from number.
        if v.is_boolean() {
            return Json(json!({ "error": "master_difficulty must be a number between 0 and 100" }));
        }
        match coerce_i64(v) {
            Some(n) => {
                let clamped = n.clamp(0, 100);
                cfg.insert("master_difficulty".into(), Value::from(clamped));
            }
            None => {
                return Json(json!({ "error": "master_difficulty must be a number between 0 and 100" }));
            }
        }
    }

    // av_offset_ms
    if let Some(v) = data.get("av_offset_ms") {
        if v.is_boolean() {
            return Json(json!({ "error": "av_offset_ms must be a number between -1000 and 1000" }));
        }
        match coerce_f64(v) {
            Some(n) => {
                let clamped = n.clamp(-1000.0, 1000.0);
                cfg.insert("av_offset_ms".into(), json!(clamped));
            }
            None => {
                return Json(json!({ "error": "av_offset_ms must be a number between -1000 and 1000" }));
            }
        }
    }

    let pretty = serde_json::to_string_pretty(&Value::Object(cfg.clone())).unwrap_or_default();
    if let Err(e) = std::fs::write(state.cfg.config_dir.join("config.json"), pretty) {
        return Json(json!({ "error": format!("Failed to write config: {e}") }));
    }
    // Refresh the in-memory cache.
    *state.settings.lock().unwrap() = Value::Object(cfg);

    let msg = if messages.is_empty() {
        "Settings saved".to_string()
    } else {
        messages.join(". ")
    };
    Json(json!({ "message": msg }))
}

/// Coerce a JSON value to i64 the way Python's `int(float(raw))` does:
/// integers and floats pass; numeric strings parse; `null`/non-numeric fail.
fn coerce_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as i64),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

fn coerce_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coerce_strings_and_numbers() {
        assert_eq!(coerce_i64(&json!(50)), Some(50));
        assert_eq!(coerce_i64(&json!("50")), Some(50));
        assert_eq!(coerce_i64(&json!(50.9)), Some(50)); // int(float(...))
        assert_eq!(coerce_i64(&json!(true)), None);
        assert_eq!(coerce_i64(&json!(null)), None);
        assert_eq!(coerce_f64(&json!("-250")), Some(-250.0));
    }
}
