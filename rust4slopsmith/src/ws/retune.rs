//! `WS /ws/retune` — pitch-shift a PSARC to a target tuning with real-time
//! progress. Port of `ws_retune` (server.py:907-1026). Sloppaks are rejected
//! (retune depends on the PSARC SNG/encryption pipeline). Progress flows from
//! the blocking retune task through an mpsc channel to the WS sender.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::engine::retune;
use crate::engine::sloppak;
use crate::state::AppState;

#[derive(serde::Deserialize)]
pub struct RetuneQuery {
    filename: String,
    #[serde(default = "default_target")]
    target: String,
}
fn default_target() -> String {
    "E Standard".to_string()
}

pub async fn retune_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(q): Query<RetuneQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| run_retune(socket, state, q.filename, q.target))
}

async fn send_json(socket: &mut WebSocket, v: &Value) {
    let text = serde_json::to_string(v).unwrap_or_else(|_| "{}".into());
    let _ = socket.send(Message::Text(text.into())).await;
}

async fn run_retune(mut socket: WebSocket, state: Arc<AppState>, filename: String, target: String) {
    let dlc = match state.cfg.get_dlc_dir() {
        Some(d) => d,
        None => return send_error(&mut socket, "DLC folder not configured").await,
    };
    let psarc_path = dlc.join(&filename);
    if !psarc_path.exists() {
        return send_error(&mut socket, "File not found").await;
    }
    if filename.to_lowercase().ends_with(".sloppak") || sloppak::is_sloppak(&psarc_path) {
        return send_error(&mut socket, "Retune is not supported for .sloppak files").await;
    }

    let (tx, mut rx) = mpsc::channel::<Value>(32);
    let target_clone = target.clone();
    let ppath = psarc_path.clone();
    let st = state.clone();

    let build_task = tokio::task::spawn_blocking(move || {
        let result = (|| -> anyhow::Result<String> {
            let _ = tx.try_send(json!({ "stage": "Checking tuning...", "progress": 5 }));
            let (offsets, _uniform) = retune::get_tuning(&ppath)?;

            let target_offsets: Vec<i64> = if target_clone == "Drop D" {
                vec![-2, 0, 0, 0, 0, 0]
            } else {
                vec![0, 0, 0, 0, 0, 0]
            };

            if offsets == target_offsets {
                let _ = tx.try_send(json!({ "error": format!("Already in {target_clone}") }));
                return Ok(String::new());
            }

            let shift: Vec<i64> = (0..6).map(|i| target_offsets[i] - offsets[i]).collect();
            let uniform = shift.iter().all(|&x| x == shift[0]);
            if !uniform {
                let _ = tx.try_send(json!({ "error": format!("Cannot uniformly retune {offsets:?} to {target_clone} — shift varies per string") }));
                return Ok(String::new());
            }

            let suffix = if target_clone == "E Standard" { "_EStd" } else { "_DropD" };
            let stem = ppath.file_stem().and_then(|s| s.to_str()).unwrap_or("").replace("_p", "");
            let out_path: PathBuf = ppath.parent().unwrap_or(&ppath).join(format!("{stem}{suffix}_p.psarc"));

            let tx3 = tx.clone();
            retune::retune_to_standard(&ppath, &out_path, &|stage, pct| {
                let _ = tx3.try_send(json!({ "stage": stage, "progress": pct }));
            })?;

            Ok(out_path.to_string_lossy().to_string())
        })();

        match result {
            Ok(out_path) if !out_path.is_empty() => {
                // Cache metadata for the new file.
                let new_path = PathBuf::from(&out_path);
                if new_path.exists() {
                    if let Some(meta) = crate::scan::extract_meta_for_file(&new_path) {
                        if let Ok(stat) = std::fs::metadata(&new_path) {
                            let mtime = mtime(&stat);
                            let size = stat.len() as i64;
                            let _ = st.db.put(&new_path.to_string_lossy(), mtime, size, &meta);
                        }
                    }
                }
                let _ = tx.try_send(json!({
                    "done": true, "progress": 100,
                    "stage": "Complete!",
                    "filename": new_path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
                }));
            }
            Ok(_) => { /* error already sent inline */ }
            Err(e) => {
                let _ = tx.try_send(json!({ "error": e.to_string() }));
            }
        }
    });

    // Forward progress to the WS until done/error.
    while let Some(msg) = rx.recv().await {
        send_json(&mut socket, &msg).await;
        if msg.get("done").is_some() || msg.get("error").is_some() {
            break;
        }
    }
    let _ = build_task.await;
}

async fn send_error(socket: &mut WebSocket, msg: &str) {
    send_json(socket, &json!({ "error": msg })).await;
}

fn mtime(meta: &std::fs::Metadata) -> f64 {
    use std::time::SystemTime;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
