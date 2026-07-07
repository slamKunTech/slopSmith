//! Plugin discovery + API — Rust port of `plugins/__init__.py`.
//!
//! Rust has no dynamic import system like Python, so this port keeps only the
//! parts that make sense statically: discovering plugin directories that ship a
//! `plugin.json` manifest, recording their metadata, and serving their
//! screen / script / settings assets (plus git-update checks) through the API.
//! Executable backend routes (`routes.py`) are intentionally *not* loaded —
//! native plugins would instead be compiled in.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use axum::{
    extract::{Path as AxPath, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::AppState;

/// Metadata describing a single discovered plugin.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub nav: Option<Value>,
    pub plugin_type: Option<String>,
    pub has_screen: bool,
    pub has_script: bool,
    pub has_settings: bool,
    pub dir: PathBuf,
    pub manifest: Value,
}

/// Shared context handed to plugins at load time (subset of the Python dict).
#[derive(Clone)]
pub struct PluginContext {
    pub config_dir: PathBuf,
}

/// Discover and load plugins from the given directories.
///
/// Directories earlier in the list win on id collisions (user plugins override
/// built-ins, matching the Python loader's ordering). Each subdirectory that
/// contains a readable `plugin.json` with a non-empty string `id` becomes a
/// `PluginInfo`.
pub fn load_plugins(plugin_dirs: &[PathBuf], _context: &PluginContext) -> Vec<PluginInfo> {
    let mut loaded_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut plugins: Vec<PluginInfo> = Vec::new();

    for base in plugin_dirs {
        if !base.is_dir() {
            continue;
        }
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(base) {
            Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
            Err(_) => continue,
        };
        entries.sort();

        for plugin_dir in entries {
            if !plugin_dir.is_dir() {
                continue;
            }
            let manifest_path = plugin_dir.join("plugin.json");
            if !manifest_path.exists() {
                continue;
            }
            let manifest: Value = match std::fs::read_to_string(&manifest_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
            {
                Some(m) => m,
                None => {
                    eprintln!("[Plugin] Failed to read {}", manifest_path.display());
                    continue;
                }
            };

            // `id` must be present and a non-empty string.
            let plugin_id = match manifest.get("id") {
                None => continue,
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                Some(Value::String(_)) => continue, // empty string → silently skip
                Some(other) => {
                    eprintln!(
                        "[Plugin] Skipping {}: 'id' must be a string, got {}",
                        manifest_path.display(),
                        other
                    );
                    continue;
                }
            };

            if loaded_ids.contains(&plugin_id) {
                eprintln!(
                    "[Plugin] Skipping duplicate '{}' from {}",
                    plugin_id,
                    base.display()
                );
                continue;
            }
            loaded_ids.insert(plugin_id.clone());

            let name = manifest
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&plugin_id)
                .to_string();

            let info = PluginInfo {
                id: plugin_id.clone(),
                name: name.clone(),
                nav: manifest.get("nav").cloned(),
                plugin_type: manifest.get("type").and_then(|v| v.as_str()).map(String::from),
                has_screen: manifest.get("screen").is_some(),
                has_script: manifest.get("script").is_some(),
                has_settings: manifest.get("settings").is_some(),
                dir: plugin_dir.clone(),
                manifest,
            };
            plugins.push(info);
            println!("[Plugin] Registered '{}' ({})", plugin_id, name);
        }
    }

    plugins
}

/// Register the plugin discovery API endpoints onto an existing router.
pub fn register_plugin_api(router: Router<Arc<AppState>>) -> Router<Arc<AppState>> {
    router
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/updates", get(check_updates))
        .route("/api/plugins/:plugin_id/update", post(update_plugin))
        .route("/api/plugins/:plugin_id/screen.html", get(plugin_screen_html))
        .route("/api/plugins/:plugin_id/screen.js", get(plugin_screen_js))
        .route("/api/plugins/:plugin_id/settings.html", get(plugin_settings_html))
}

async fn list_plugins(State(state): State<Arc<AppState>>) -> Json<Value> {
    let plugins = state.plugins.lock().await;
    let list: Vec<Value> = plugins
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "nav": p.nav,
                "type": p.plugin_type,
                "has_screen": p.has_screen,
                "has_script": p.has_script,
                "has_settings": p.has_settings,
            })
        })
        .collect();
    Json(Value::Array(list))
}

async fn check_updates(State(state): State<Arc<AppState>>) -> Json<Value> {
    let specs: Vec<(String, String, PathBuf)> = {
        let plugins = state.plugins.lock().await;
        plugins
            .iter()
            .map(|p| (p.id.clone(), p.name.clone(), p.dir.clone()))
            .collect()
    };

    let mut updates = serde_json::Map::new();
    for (id, name, dir) in specs {
        // git work is blocking — offload it.
        let info = tokio::task::spawn_blocking(move || check_plugin_update(&dir))
            .await
            .ok()
            .flatten();
        if let Some(info) = info {
            if info.behind > 0 {
                updates.insert(
                    id,
                    json!({
                        "name": name,
                        "behind": info.behind,
                        "local": info.local,
                        "remote": info.remote,
                    }),
                );
            }
        }
    }
    Json(json!({ "updates": Value::Object(updates) }))
}

async fn update_plugin(
    State(state): State<Arc<AppState>>,
    AxPath(plugin_id): AxPath<String>,
) -> Json<Value> {
    let dir = {
        let plugins = state.plugins.lock().await;
        plugins.iter().find(|p| p.id == plugin_id).map(|p| p.dir.clone())
    };
    let dir = match dir {
        Some(d) => d,
        None => return Json(json!({ "error": "Plugin not found" })),
    };

    let result = tokio::task::spawn_blocking(move || pull_plugin(&dir))
        .await
        .unwrap_or_else(|e| json!({ "error": e.to_string() }));
    Json(result)
}

async fn plugin_screen_html(
    State(state): State<Arc<AppState>>,
    AxPath(plugin_id): AxPath<String>,
) -> Response {
    serve_asset(&state, &plugin_id, "screen", "screen.html", AssetKind::Html).await
}

async fn plugin_screen_js(
    State(state): State<Arc<AppState>>,
    AxPath(plugin_id): AxPath<String>,
) -> Response {
    serve_asset(&state, &plugin_id, "script", "screen.js", AssetKind::Js).await
}

async fn plugin_settings_html(
    State(state): State<Arc<AppState>>,
    AxPath(plugin_id): AxPath<String>,
) -> Response {
    // `settings` may be a dict with an `html` key, or absent → default name.
    let plugins = state.plugins.lock().await;
    if let Some(p) = plugins.iter().find(|p| p.id == plugin_id) {
        let rel = p
            .manifest
            .get("settings")
            .and_then(|s| s.get("html"))
            .and_then(|v| v.as_str())
            .unwrap_or("settings.html");
        let file = p.dir.join(rel);
        if let Ok(body) = std::fs::read_to_string(&file) {
            return Html(body).into_response();
        }
    }
    (StatusCode::NOT_FOUND, Html(String::new())).into_response()
}

enum AssetKind {
    Html,
    Js,
}

async fn serve_asset(
    state: &Arc<AppState>,
    plugin_id: &str,
    manifest_key: &str,
    default_name: &str,
    kind: AssetKind,
) -> Response {
    let plugins = state.plugins.lock().await;
    if let Some(p) = plugins.iter().find(|p| p.id == plugin_id) {
        let rel = p
            .manifest
            .get(manifest_key)
            .and_then(|v| v.as_str())
            .unwrap_or(default_name);
        let file = p.dir.join(rel);
        if let Ok(body) = std::fs::read_to_string(&file) {
            return match kind {
                AssetKind::Html => Html(body).into_response(),
                AssetKind::Js => (
                    [(header::CONTENT_TYPE, "application/javascript")],
                    body,
                )
                    .into_response(),
            };
        }
    }
    match kind {
        AssetKind::Html => (StatusCode::NOT_FOUND, Html(String::new())).into_response(),
        AssetKind::Js => (StatusCode::NOT_FOUND, String::new()).into_response(),
    }
}

// ── git helpers (blocking) ────────────────────────────────────────────────────

struct UpdateInfo {
    behind: i64,
    local: String,
    remote: String,
}

fn check_plugin_update(plugin_dir: &Path) -> Option<UpdateInfo> {
    if !plugin_dir.join(".git").exists() {
        return None;
    }
    // Fetch refs (best-effort).
    Command::new("git")
        .args(["fetch", "--quiet"])
        .current_dir(plugin_dir)
        .output()
        .ok();

    let rev_list = Command::new("git")
        .args(["rev-list", "HEAD..@{u}", "--count"])
        .current_dir(plugin_dir)
        .output()
        .ok()?;
    if !rev_list.status.success() {
        return None;
    }
    let behind: i64 = String::from_utf8_lossy(&rev_list.stdout)
        .trim()
        .parse()
        .ok()?;

    let local = git_capture(plugin_dir, &["rev-parse", "--short", "HEAD"]);
    let remote = git_capture(plugin_dir, &["rev-parse", "--short", "@{u}"]);

    Some(UpdateInfo {
        behind,
        local,
        remote,
    })
}

fn git_capture(dir: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn pull_plugin(plugin_dir: &Path) -> Value {
    if !plugin_dir.join(".git").exists() {
        return json!({ "error": "Not a git repository" });
    }
    // Stash local edits so the pull can't fail on a dirty tree.
    Command::new("git")
        .args(["stash", "--quiet"])
        .current_dir(plugin_dir)
        .output()
        .ok();

    match Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(plugin_dir)
        .output()
    {
        Ok(out) if out.status.success() => {
            json!({ "ok": true, "message": String::from_utf8_lossy(&out.stdout).trim() })
        }
        Ok(out) => {
            // Restore stash on failure.
            Command::new("git")
                .args(["stash", "pop", "--quiet"])
                .current_dir(plugin_dir)
                .output()
                .ok();
            let mut err = String::from_utf8_lossy(&out.stderr).to_string();
            err.truncate(500);
            json!({ "error": err })
        }
        Err(e) => json!({ "error": e.to_string() }),
    }
}
