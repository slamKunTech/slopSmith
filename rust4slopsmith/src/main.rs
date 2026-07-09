//! Slopsmith Rust backend — drop-in replacement for the Python `server.py`.
//!
//! Wave 1: skeleton + config + SQLite metadata DB + settings/version/scan
//! endpoints. Later waves add the library scan, binary format cores, the
//! WebSocket highway, retune, art serving, and the Python sidecar proxy.

// `src/lib/` mirrors `slopsmith/lib/`; the folder name trips the
// `special_module_name` lint, but this is a binary crate with no library
// target, so there's no real ambiguity.
#![allow(special_module_name)]

mod caches;
mod config;
mod db;
mod engine;
mod routes;
mod scan;
mod sidecar;
mod state;
mod ws;

use std::net::SocketAddr;

use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::db::MetadataDb;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cfg = Config::from_env();

    // Ensure cache dirs exist (best-effort; STATIC_DIR may be read-only in
    // packaged installs, but the CONFIG_DIR caches are always writable).
    let _ = std::fs::create_dir_all(&cfg.config_dir);
    let _ = std::fs::create_dir_all(&cfg.art_cache_dir);
    let _ = std::fs::create_dir_all(&cfg.audio_cache_dir);
    let _ = std::fs::create_dir_all(&cfg.sloppak_cache_dir);

    let db = MetadataDb::open(&cfg.config_dir)?;
    let state = AppState::new(cfg, db);

    // Kick off the background metadata scan + periodic rescan (daemon threads).
    scan::startup_scan(state.clone());

    let app = routes::router(state.clone()).layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], 8001));
    tracing::info!("slopsmith-rs listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
