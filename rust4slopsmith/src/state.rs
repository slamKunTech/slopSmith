//! Shared application state held in an `Arc` and passed to every handler via
//! `State<AppState>`. Mirrors the module-level singletons in server.py
//! (`meta_db`, `_scan_status`, the cache dirs, `_extract_cache`, etc.).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::caches::ExtractEntry;
use crate::config::Config;
use crate::db::MetadataDb;

/// Scan progress, read by HTTP and WS handlers. Mirrors `_scan_status`
/// (server.py:472-473). All fields are `pub` because the scan task and the
/// status endpoints both touch them.
#[derive(Debug, Clone)]
pub struct ScanStatus {
    pub running: bool,
    pub stage: String,
    pub total: usize,
    pub done: usize,
    pub current: String,
    pub error: Option<String>,
}

impl ScanStatus {
    pub fn init() -> Self {
        Self {
            running: false,
            stage: "idle".to_string(),
            total: 0,
            done: 0,
            current: String::new(),
            error: None,
        }
    }
}

pub struct AppState {
    pub cfg: Config,
    pub db: MetadataDb,
    /// Scan progress, guarded because the scan task writes while HTTP reads.
    pub scan_status: Mutex<ScanStatus>,
    /// Parsed `config.json` cache for `default_arrangement` etc. Cheap to
    /// re-read; held only to avoid re-reading on every WS connection.
    pub settings: Mutex<Value>,
    /// Unpacked-PSARC cache for the highway WS (max 10, 5-min TTL).
    pub extract_cache: Mutex<HashMap<String, ExtractEntry>>,
}

impl AppState {
    pub fn new(cfg: Config, db: MetadataDb) -> Arc<Self> {
        let settings = cfg.load_config().map(Value::Object).unwrap_or_else(|| cfg.default_settings());
        Arc::new(Self {
            cfg,
            db,
            scan_status: Mutex::new(ScanStatus::init()),
            settings: Mutex::new(settings),
            extract_cache: Mutex::new(HashMap::new()),
        })
    }
}
