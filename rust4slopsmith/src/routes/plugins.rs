//! `ANY /api/plugins` and `/api/plugins/{*rest}` — reverse-proxy to the
//! Python sidecar (plugin management + plugin-registered routes + Guitar Pro
//! import). Returns 503 if no sidecar is configured/reachable. Mirrors the
//! plugin API registered in `plugins/__init__.py:register_plugin_api`.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::state::AppState;

pub async fn plugins_proxy(State(_state): State<Arc<AppState>>, req: Request) -> Response {
    let method = req.method().to_string();
    let uri = req.uri();
    // Forward the original path + query verbatim — the sidecar (Python
    // server.py) has the same routes.
    let path_and_query = match uri.query() {
        Some(q) => format!("{}?{q}", uri.path()),
        None => uri.path().to_string(),
    };
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = axum::body::to_bytes(req.into_body(), 16 * 1024 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default();

    match crate::sidecar::proxy(&method, &path_and_query, body, content_type).await {
        Some(resp) => {
            let mut out = Response::builder().status(resp.status);
            if let Some(ct) = resp.headers.get(header::CONTENT_TYPE) {
                out = out.header(header::CONTENT_TYPE, ct);
            }
            out.body(axum::body::Body::from(resp.bytes))
                .unwrap_or_else(|_| (StatusCode::BAD_GATEWAY, "proxy build failed").into_response())
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(crate::sidecar::unavailable_body()),
        )
            .into_response(),
    }
}
