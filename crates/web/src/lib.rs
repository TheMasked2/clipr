//! The local web server: the REST + WebSocket API and the Svelte UI embedded in the exe.
//!
//! It listens on `127.0.0.1` only. LAN access (opt-in, with a PIN) comes in Phase 4.
//!
//! API (the UI's `src/lib/api.ts` mirrors this):
//! - `GET /api/status` → [`StatusSnapshot`](clipr_core::status::StatusSnapshot) as JSON
//! - `GET /api/events` (WebSocket) → `{"type":"status", ...snapshot}` on connect and on every change

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use clipr_core::status::Status;
use rust_embed::RustEmbed;
use tokio::net::TcpListener;
use tokio::sync::watch;

/// How many ports after the preferred one to try before giving up.
const PORT_ATTEMPTS: u16 = 20;

/// The built UI. In debug builds rust-embed reads these files from disk at runtime, so `npm run build` is
/// picked up without recompiling; release builds bake them into the exe.
#[derive(RustEmbed)]
#[folder = "../../ui/dist/"] // relative to this crate's Cargo.toml
#[allow_missing = true]
struct Ui;

/// Shown when the UI hasn't been built yet.
const NOT_BUILT: &str = "<!doctype html><meta charset=utf-8><title>clipr</title>\
<body style=\"background:#0b0b0f;color:#e5e5ea;font:16px system-ui;padding:2rem\">\
<h1>clipr is running</h1><p>The web UI isn't built yet. Run <code>npm ci &amp;&amp; npm run build</code> \
in <code>ui/</code>, then reload.</p>";

#[derive(Clone)]
pub struct AppState {
    pub status: Arc<Status>,
    /// Flips to `true` when clipr shuts down; open WebSockets close on it.
    pub shutdown: watch::Receiver<bool>,
}

/// Binds `127.0.0.1:<preferred>`, or the next free port after it.
pub async fn bind(preferred: u16) -> Result<(TcpListener, u16)> {
    let mut last_err = None;
    for offset in 0..PORT_ATTEMPTS {
        let Some(port) = preferred.checked_add(offset) else {
            break;
        };
        match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await {
            Ok(listener) => {
                let port = listener.local_addr().map_or(port, |a| a.port());
                if offset > 0 {
                    tracing::info!("port {preferred} is taken, using {port}");
                }
                return Ok((listener, port));
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.map_or_else(|| anyhow::anyhow!("no port to try"), Into::into)).with_context(|| {
        format!(
            "no free port in {preferred}..{}",
            preferred.saturating_add(PORT_ATTEMPTS)
        )
    })
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        .route("/api/{*rest}", get(api_not_found))
        .fallback(static_file)
        .with_state(state)
}

/// Serves until `shutdown` flips to `true`.
pub async fn serve(listener: TcpListener, state: AppState) -> Result<()> {
    let mut shutdown = state.shutdown.clone();
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            let _ = shutdown.wait_for(|stop| *stop).await;
        })
        .await
        .context("web server stopped")
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(state.status.snapshot())
}

async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn events(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| push_status(socket, state))
}

async fn push_status(mut socket: WebSocket, state: AppState) {
    let mut changes = state.status.subscribe();
    let mut shutdown = state.shutdown.clone();
    loop {
        let mut message = serde_json::to_value(state.status.snapshot()).expect("status is always JSON");
        message["type"] = "status".into();
        if socket
            .send(Message::Text(message.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        tokio::select! {
            changed = changes.changed() => if changed.is_err() { break },
            _ = shutdown.wait_for(|stop| *stop) => break,
            incoming = socket.recv() => match incoming {
                // The UI doesn't send anything yet; ignore pings/text, stop when it goes away.
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => continue,
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

/// Serves a file from the embedded UI. Unknown paths get `index.html`, so client-side routes work.
async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(file) = Ui::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        // Vite puts content hashes in asset names, so those can be cached forever.
        let cache = if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            [
                (header::CONTENT_TYPE, mime.as_ref()),
                (header::CACHE_CONTROL, cache),
            ],
            file.data,
        )
            .into_response();
    }
    match Ui::get("index.html") {
        Some(index) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], index.data).into_response(),
        None => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], NOT_BUILT).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use clipr_core::status::{Activity, RecState};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn state() -> (AppState, watch::Sender<bool>) {
        let (tx, rx) = watch::channel(false);
        (
            AppState {
                status: Arc::new(Status::new()),
                shutdown: rx,
            },
            tx,
        )
    }

    async fn get_body(app: Router, uri: &str) -> (StatusCode, String) {
        let response = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let code = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (code, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn status_endpoint_reports_activity() {
        let (state, _tx) = state();
        state.status.set(Activity {
            state: RecState::Buffering,
            game: Some("Minecraft".into()),
            ..Default::default()
        });
        let (code, body) = get_body(router(state), "/api/status").await;
        assert_eq!(code, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["state"], "buffering");
        assert_eq!(json["game"], "Minecraft");
    }

    #[tokio::test]
    async fn unknown_api_path_is_404_not_the_ui() {
        let (state, _tx) = state();
        let (code, _) = get_body(router(state), "/api/nope").await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn client_routes_fall_back_to_html() {
        let (state, _tx) = state();
        let (code, body) = get_body(router(state), "/gallery/some/clip").await;
        assert_eq!(code, StatusCode::OK);
        assert!(body.contains("<"), "expected HTML, got {body:?}");
    }

    #[tokio::test]
    async fn bind_falls_back_to_next_port() {
        // Port 0 means "any free port"; keep it occupied and ask for it again.
        let (first, taken) = bind(0).await.unwrap();
        assert_ne!(taken, 0);
        let (_second, got) = bind(taken).await.unwrap();
        assert_ne!(got, taken);
        drop(first);
    }
}
