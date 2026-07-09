//! `GET /api/version`, `/api/scan-status`, `/api/startup-status`,
//! `POST /api/rescan`, `POST /api/rescan/full`. Mirrors server.py:615-692.
//!
//! Wave 1 ships these as stubs that report scan status; the actual background
//! scan is wired up in Wave 2 (`scan.rs`), so `trigger_rescan` / `trigger_full_rescan`
//! will start the scan task once that lands. For now they report "not running".

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;

pub async fn get_version(State(state): State<std::sync::Arc<AppState>>) -> Json<Value> {
    Json(json!({ "version": state.cfg.version }))
}

pub async fn scan_status(State(state): State<std::sync::Arc<AppState>>) -> Json<Value> {
    let s = state.scan_status.lock().unwrap();
    Json(scan_status_to_json(&s))
}

pub async fn startup_status(State(state): State<std::sync::Arc<AppState>>) -> Json<Value> {
    let s = state.scan_status.lock().unwrap();
    let stage = s.stage.clone();
    let (phase, running, message) = match stage.as_str() {
        "error" => (
            "error".to_string(),
            false,
            s.error.clone().unwrap_or_else(|| "Scan failed".to_string()),
        ),
        "complete" | "idle" => ("complete".to_string(), false, "Ready".to_string()),
        _ => (
            "scanning".to_string(),
            true,
            format!("Scanning library {}/{}", s.done, s.total),
        ),
    };
    Json(json!({
        "running": running,
        "phase": phase,
        "message": message,
        "current_plugin": "",
        "loaded": s.done,
        "total": s.total,
        "error": if stage == "error" { s.error.clone() } else { None },
    }))
}

pub async fn trigger_rescan(State(state): State<std::sync::Arc<AppState>>) -> Json<Value> {
    if crate::scan::trigger_rescan(state.clone()) {
        Json(json!({ "message": "Rescan started" }))
    } else {
        Json(json!({ "message": "Scan already in progress" }))
    }
}

pub async fn trigger_full_rescan(State(state): State<std::sync::Arc<AppState>>) -> Json<Value> {
    {
        let s = state.scan_status.lock().unwrap();
        if s.running {
            return Json(json!({ "message": "Scan already in progress" }));
        }
    }
    let _ = state.db.clear_songs();
    crate::scan::trigger_rescan(state.clone());
    Json(json!({ "message": "Full rescan started" }))
}

/// Render a `ScanStatus` as the JSON shape `_scan_status` is returned as by
/// `GET /api/scan-status` (server.py:630-632 returns the dict directly).
pub(crate) fn scan_status_to_json(s: &crate::state::ScanStatus) -> Value {
    json!({
        "running": s.running,
        "stage": s.stage,
        "total": s.total,
        "done": s.done,
        "current": s.current,
        "error": s.error,
    })
}
