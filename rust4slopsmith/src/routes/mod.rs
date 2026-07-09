//! HTTP route handlers. Each submodule mirrors a section of server.py.

pub mod audio;
pub mod favorites;
pub mod library;
pub mod loops;
pub mod plugins;
pub mod settings;
pub mod sloppak_files;
pub mod song;
pub mod static_files;
pub mod version;

use axum::Router;
use tower_http::services::ServeDir;

use crate::state::AppState;

/// Build the full HTTP router. Routes are added incrementally per wave; this
/// function is the single registration point.
pub fn router(state: std::sync::Arc<AppState>) -> Router {
    let static_dir = state.cfg.static_dir.clone();

    Router::new()
        // Core / meta
        .route("/api/version", axum::routing::get(version::get_version))
        .route("/api/scan-status", axum::routing::get(version::scan_status))
        .route("/api/startup-status", axum::routing::get(version::startup_status))
        .route("/api/rescan", axum::routing::post(version::trigger_rescan))
        .route("/api/rescan/full", axum::routing::post(version::trigger_full_rescan))
        // Settings
        .route("/api/settings", axum::routing::get(settings::get_settings))
        .route("/api/settings", axum::routing::post(settings::save_settings))
        // Library
        .route("/api/library", axum::routing::get(library::list_library))
        .route("/api/library/artists", axum::routing::get(library::list_artists))
        .route("/api/library/stats", axum::routing::get(library::library_stats))
        // Favorites
        .route("/api/favorites/toggle", axum::routing::post(favorites::toggle_favorite))
        // Loops
        .route("/api/loops", axum::routing::get(loops::list_loops))
        .route("/api/loops", axum::routing::post(loops::save_loop))
        .route("/api/loops/{loop_id}", axum::routing::delete(loops::delete_loop))
        // Song metadata + art (dispatch on {*rest} suffix)
        .route("/api/song/{*rest}", axum::routing::get(song::song_get))
        .route("/api/song/{*rest}", axum::routing::post(song::song_post))
        // Sloppak file serving (stems / cover / json)
        .route("/api/sloppak/{*rest}", axum::routing::get(sloppak_files::serve_sloppak_file))
        // Audio cache
        .route("/audio/{*filename}", axum::routing::get(audio::serve_audio))
        // Plugin API + plugin-registered routes → Python sidecar (503 if none)
        .route("/api/plugins", axum::routing::any(plugins::plugins_proxy))
        .route("/api/plugins/{*rest}", axum::routing::any(plugins::plugins_proxy))
        // Highway + retune WebSockets
        .route("/ws/highway/{*filename}", axum::routing::get(crate::ws::highway::highway_ws))
        .route("/ws/retune", axum::routing::get(crate::ws::retune::retune_ws))
        // Index
        .route("/", axum::routing::get(static_files::index))
        // Static files (JS/CSS/HTML assets served to the frontend unchanged)
        .nest_service("/static", ServeDir::new(static_dir))
        .with_state(state)
}
