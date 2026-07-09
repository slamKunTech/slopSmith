//! Environment + on-disk layout. Mirrors server.py:28-46, 321-339, 615-627.
//!
//! Paths are derived the same way as the Python backend so the Rust binary is
//! a drop-in: same `CONFIG_DIR`, same caches, same `config.json`, same
//! `DLC_DIR`-env-vs-config fallback semantics.

use std::path::{Path, PathBuf};

/// A snapshot of the process environment + derived paths, captured once at
/// startup and shared via [`AppState`](crate::state::AppState).
pub struct Config {
    /// Repo/static root: parent of the executable, used to locate `static/`
    /// and `VERSION`. In packaged installs this is the app bundle's resource
    /// dir; in dev it's `target/<profile>`.
    pub root_dir: PathBuf,
    pub static_dir: PathBuf,

    /// Raw `DLC_DIR` env string (empty if unset/blank). Kept separate from
    /// [`Self::dlc_dir`] because `Path("")` collapses to `.` whose `is_dir()`
    /// is true — the raw string is what distinguishes "unset" from "cwd".
    pub dlc_dir_env: String,
    /// `DLC_DIR` as a Path, only meaningful when `dlc_dir_env` is non-empty.
    pub dlc_dir: PathBuf,

    pub config_dir: PathBuf,
    pub art_cache_dir: PathBuf,
    pub audio_cache_dir: PathBuf,
    pub sloppak_cache_dir: PathBuf,

    pub app_version_env: Option<String>,
    pub rscli_path: Option<PathBuf>,
    /// Command line to launch the Python plugin/GP sidecar, if configured.
    pub sidecar: Option<String>,

    /// Resolved version string for `GET /api/version` (env > VERSION file >
    /// crate version).
    pub version: String,
}

impl Config {
    /// Read the process environment and derive all paths.
    pub fn from_env() -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
        // dev: target/<profile>/slopsmith-rs → repo root is two levels up.
        // packaged: next to the binary. Either way `static/` and `VERSION`
        // live alongside the binary's parent.
        let root_dir = exe
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let static_dir = root_dir.join("static");

        let dlc_dir_env = std::env::var("DLC_DIR").unwrap_or_default().trim().to_string();
        let dlc_dir = if dlc_dir_env.is_empty() {
            PathBuf::new()
        } else {
            PathBuf::from(&dlc_dir_env)
        };

        let config_dir = std::env::var("CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs_compat_home().join(".local").join("share").join("rocksmith-cdlc")
            });

        let art_cache_dir = config_dir.join("art_cache");
        let audio_cache_dir = config_dir.join("audio_cache");
        let sloppak_cache_dir = config_dir.join("sloppak_cache");

        let app_version_env = std::env::var("APP_VERSION")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let rscli_path = std::env::var("RSCLI_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        let sidecar = std::env::var("SLOPSMITH_SIDECAR")
            .ok()
            .filter(|s| !s.is_empty());

        let version = resolve_version(&root_dir, app_version_env.as_deref());

        Self {
            root_dir,
            static_dir,
            dlc_dir_env,
            dlc_dir,
            config_dir,
            art_cache_dir,
            audio_cache_dir,
            sloppak_cache_dir,
            app_version_env,
            rscli_path,
            sidecar,
            version,
        }
    }

    /// Resolve the active DLC directory: `DLC_DIR` env (if set + is a dir) →
    /// `config.json` `dlc_dir` (if a dir) → `None`. Mirrors `_get_dlc_dir()`
    /// (server.py:321-339).
    pub fn get_dlc_dir(&self) -> Option<PathBuf> {
        if !self.dlc_dir_env.is_empty() && self.dlc_dir.is_dir() {
            return Some(self.dlc_dir.clone());
        }
        let config_file = self.config_dir.join("config.json");
        if config_file.exists() {
            if let Ok(text) = std::fs::read_to_string(&config_file) {
                if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(serde_json::Value::String(raw)) = map.get("dlc_dir") {
                        let raw = raw.trim();
                        if !raw.is_empty() {
                            let p = PathBuf::from(raw);
                            if p.is_dir() {
                                return Some(p);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Default settings returned when `config.json` is missing/unreadable.
    /// Mirrors `_default_settings()` (server.py:778-788): surfaces `dlc_dir`
    /// only when the env var is genuinely set + a real dir.
    pub fn default_settings(&self) -> serde_json::Value {
        let dlc = if !self.dlc_dir_env.is_empty() && self.dlc_dir.is_dir() {
            self.dlc_dir.to_string_lossy().to_string()
        } else {
            String::new()
        };
        serde_json::json!({ "dlc_dir": dlc })
    }

    /// Load `config.json` as a JSON object, or `None` if missing/unreadable/
    /// not an object. Mirrors `_load_config()` (server.py:791-803).
    pub fn load_config(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let config_file = self.config_dir.join("config.json");
        if !config_file.exists() {
            return None;
        }
        let text = std::fs::read_to_string(&config_file).ok()?;
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            _ => None,
        }
    }
}

fn resolve_version(root_dir: &Path, app_version_env: Option<&str>) -> String {
    if let Some(v) = app_version_env {
        if !v.is_empty() {
            return v.to_string();
        }
    }
    let version_file = root_dir.join("VERSION");
    if version_file.exists() {
        if let Ok(text) = std::fs::read_to_string(&version_file) {
            let v = text.trim().to_string();
            if !v.is_empty() {
                return v;
            }
        }
    }
    // Final fallback: the crate version (set in Cargo.toml to match VERSION).
    env!("CARGO_PKG_VERSION").to_string()
}

/// `std::env::home_dir()` was deprecated; replicate its behavior without
/// pulling the `dirs` crate for a single call. Honors `$HOME` on Unix.
fn dirs_compat_home() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    PathBuf::from(".")
}
