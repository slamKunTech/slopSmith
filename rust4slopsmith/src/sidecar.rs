//! Python sidecar manager + reverse-proxy. The plugin system and Guitar Pro
//! import stay in Python (they can't run natively in Rust); Rust spawns/manages
//! a Python sidecar process and reverse-proxies `/api/plugins/*` (and any
//! plugin-registered routes) to it.
//!
//! Configuration (env):
//! - `SLOPSMITH_SIDECAR_URL` — base URL of the sidecar (default
//!   `http://127.0.0.1:8002`). If unset AND no spawn command is given, plugin
//!   routes return 503.
//! - `SLOPSMITH_SIDECAR` — shell command to launch the sidecar (e.g.
//!   `python server.py --port 8002`). Lazily spawned on the first plugin
//!   request. The process is expected to listen at `SIDECAR_URL`.

use std::sync::Mutex;

/// Global sidecar state: the spawned child (kept alive) + readiness.
struct Sidecar {
    base_url: String,
    spawn_cmd: Option<String>,
    child: Mutex<Option<std::process::Child>>,
}

static SIDECAR: Mutex<Option<Sidecar>> = Mutex::new(None);

/// 503 response body for "no sidecar configured/available".
pub fn unavailable_body() -> serde_json::Value {
    serde_json::json!({ "error": "plugins unavailable — set SLOPSMITH_SIDECAR_URL (and optionally SLOPSMITH_SIDECAR) to enable the Python sidecar" })
}

/// Lazily initialize + start the sidecar, returning its base URL if available.
/// Returns `None` if no sidecar is configured or it can't be reached.
pub fn ensure_started() -> Option<String> {
    let mut guard = SIDECAR.lock().unwrap();
    let sidecar = guard.get_or_insert_with(|| {
        let base_url = std::env::var("SLOPSMITH_SIDECAR_URL").unwrap_or_else(|_| "http://127.0.0.1:8002".to_string());
        let spawn_cmd = std::env::var("SLOPSMITH_SIDECAR").ok().filter(|s| !s.is_empty());
        Sidecar {
            base_url,
            spawn_cmd,
            child: Mutex::new(None),
        }
    });

    // Spawn the sidecar process if a command is configured and not yet started.
    if let Some(cmd) = &sidecar.spawn_cmd {
        let mut child_guard = sidecar.child.lock().unwrap();
        if child_guard.is_none() {
            // `sh -c` so the command string can include args/redirects.
            if let Ok(child) = std::process::Command::new("sh").arg("-c").arg(cmd).spawn() {
                *child_guard = Some(child);
                tracing::info!("spawned sidecar: {cmd}");
            } else {
                tracing::warn!("failed to spawn sidecar: {cmd}");
            }
        }
    }

    // Probe readiness (best-effort, blocking — only on plugin requests).
    if probe(&sidecar.base_url) {
        Some(sidecar.base_url.clone())
    } else {
        None
    }
}

/// Best-effort health probe of the sidecar (short TCP-level check). Returns
/// true if the sidecar accepts a connection.
fn probe(base_url: &str) -> bool {
    // Parse host:port from the URL and attempt a TCP connect.
    let parsed = match reqwest::Url::parse(base_url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port_or_known_default().unwrap_or(80);
    std::net::TcpStream::connect_timeout(
        &format!("{host}:{port}").parse().unwrap_or(([127, 0, 0, 1], 8002).into()),
        std::time::Duration::from_millis(500),
    )
    .is_ok()
}

/// Forward a request to the sidecar. `method`, `path` (URL path + query), and
/// `body` are relayed; the sidecar's response is returned as bytes + status +
/// headers.
pub async fn proxy(
    method: &str,
    path_and_query: &str,
    body: Vec<u8>,
    content_type: Option<String>,
) -> Option<ProxyResponse> {
    let base = ensure_started()?;
    let url = format!("{base}{path_and_query}");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .ok()?;
    let m = match method {
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        "PATCH" => reqwest::Method::PATCH,
        _ => reqwest::Method::GET,
    };
    let mut builder = client.request(m, &url);
    if !body.is_empty() {
        builder = builder.body(body);
    }
    if let Some(ct) = content_type {
        builder = builder.header(reqwest::header::CONTENT_TYPE, ct);
    }
    let resp = builder.send().await.ok()?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.bytes().await.ok().map(|b| b.to_vec()).unwrap_or_default();
    Some(ProxyResponse { status, headers, bytes })
}

pub struct ProxyResponse {
    pub status: reqwest::StatusCode,
    pub headers: reqwest::header::HeaderMap,
    pub bytes: Vec<u8>,
}
