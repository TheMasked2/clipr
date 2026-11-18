//! The local web server: the REST + WebSocket API and the Svelte UI embedded in the exe.
//!
//! It listens on `127.0.0.1`; with LAN access on, the app also calls [`bind_lan`] and every request from
//! another machine needs the PIN (see `auth`). The API is described in `docs/api.md` (the UI's
//! `src/lib/api.ts` mirrors it).

mod api;
mod auth;
mod cloud;
mod control;
mod discord;
#[cfg(test)]
mod tests;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use clipr_core::events::{Bus, Event};
use clipr_core::status::Status;
use clipr_library::Library;
use rust_embed::RustEmbed;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;

pub use control::{Control, NoopControl};

/// How many ports after the preferred one to try before giving up.
const PORT_ATTEMPTS: u16 = 20;

/// Waits this long after a library change before telling the UI, so a burst becomes one message.
const LIBRARY_DEBOUNCE: Duration = Duration::from_millis(300);

/// How often connected UIs get the audio levels (10 times a second).
const LEVELS_EVERY: Duration = Duration::from_millis(100);

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
    /// What the API asks the rest of clipr to do.
    pub control: Arc<dyn Control>,
    /// Where `ClipSaved` / `ClipFailed` come from.
    pub bus: Bus,
    pub library: Arc<Library>,
    /// Where the library writes thumbnails (`library.thumbs_dir()`).
    pub thumbs_dir: PathBuf,
    /// Posts clips to Discord.
    pub discord: clipr_discord::Poster,
    /// Copies clips to the user's cloud bucket.
    pub cloud: clipr_cloud::Uploader,
    /// Opens and closes access from the local network as `web.lan` changes.
    pub lan: Lan,
}

/// Access from the local network (`web.lan`). [`serve_many`] opens a listener on `0.0.0.0:<port>` while it's on
/// and closes it when it goes off, so saving the setting applies at once, with no restart. Other machines
/// still need the PIN, and their open event streams end when access goes off.
#[derive(Clone)]
pub struct Lan {
    port: u16,
    want: watch::Sender<bool>,
    /// Why the listener couldn't open (the port is taken on another address, say), while on.
    error: Arc<Mutex<Option<String>>>,
}

impl Lan {
    /// `port` is the one [`bind`] got: the LAN listener uses the same.
    pub fn new(port: u16, on: bool) -> Self {
        Self {
            port,
            want: watch::Sender::new(on),
            error: Arc::default(),
        }
    }

    /// Saving with access on while it couldn't open tries again (after the user freed the port, say).
    pub fn set(&self, on: bool) {
        let retry = on && self.error.lock().unwrap().is_some();
        self.want
            .send_if_modified(|v| std::mem::replace(v, on) != on || retry);
    }

    /// The port the web UI listens on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Why access from the local network is on but not working, if it isn't.
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
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

/// Binds `0.0.0.0:<port>` for LAN access, with the port [`bind`] got. [`serve_many`] does this through [`Lan`].
pub async fn bind_lan(port: u16) -> Result<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .await
        .with_context(|| {
            format!("can't listen on the local network at port {port} (is it taken or blocked?)")
        })
}

pub fn router(state: AppState) -> Router {
    let auth = Arc::new(auth::Auth::default());
    Router::new()
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        .route("/api/clip", post(api::save_clip))
        .route("/api/buffer", post(api::set_buffer))
        .route("/api/recording", post(api::set_recording))
        .route("/api/mark", post(api::mark))
        .route("/api/mic", post(api::set_mic))
        .route("/api/sound/test", post(api::test_sound))
        .route("/api/levels/watch", post(api::watch_levels))
        .route("/api/perf", get(api::perf))
        .route("/api/audio/volume", post(api::set_volume))
        .route("/api/preview", get(api::preview))
        .route("/api/config", get(api::get_config).put(api::put_config))
        .route("/api/system", get(api::system))
        .route("/api/crashes", get(api::crashes))
        .route("/api/crashes/dismiss", post(api::dismiss_crash))
        .route("/api/crashes/reveal", post(api::reveal_crash))
        .route("/api/update", get(api::update_status))
        .route("/api/update/check", post(api::check_update))
        .route("/api/update/apply", post(api::apply_update))
        .route(
            "/api/clips",
            get(api::list_clips)
                .patch(api::patch_clip)
                .delete(api::delete_clip),
        )
        .route("/api/clips/trim", post(api::trim_clip))
        .route("/api/clips/reveal", post(api::reveal))
        .route("/api/games", get(api::games))
        .route("/api/shelves", get(api::shelves))
        .route("/api/cleanup", post(api::cleanup))
        .route("/api/disk", get(api::disk))
        .route("/api/discord/posts", get(discord::posts))
        .route("/api/cloud/uploads", get(cloud::uploads))
        .route("/api/cloud/status", get(cloud::status))
        .route("/api/cloud/upload", post(cloud::upload))
        .route("/api/cloud/cancel", post(cloud::cancel))
        .route("/api/cloud/remove", post(cloud::remove))
        .route("/api/cloud/test", post(cloud::test))
        .route("/api/discord/share", post(discord::share))
        .route("/api/discord/answer", post(discord::answer))
        .route("/api/discord/test", post(discord::test))
        .route("/api/discord/connect", post(discord::connect))
        .route("/api/discord/connect/{state}", get(discord::connect_result))
        .route("/discord/callback", get(discord::callback))
        .route("/api/login", post(auth::login))
        .route("/media/{*id}", get(api::media))
        .route("/thumbs/{*name}", get(api::thumb))
        .route("/api/{*rest}", any(api_not_found))
        .fallback(static_file)
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(state, auth::guard))
        .layer(axum::Extension(auth))
        .layer(axum::middleware::from_fn(auth::origin_guard))
}

/// Serves until `shutdown` flips to `true`.
pub async fn serve(listener: TcpListener, state: AppState) -> Result<()> {
    serve_many(vec![listener], state).await
}

/// Like [`serve`], on several listeners at once. Also serves the local network while `state.lan` is on, and
/// runs the background jobs: the first scan, indexing of saved clips (thumbnails), and the hourly cleanup.
pub async fn serve_many(listeners: Vec<TcpListener>, state: AppState) -> Result<()> {
    let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let mut tasks: JoinSet<Result<()>> = JoinSet::new();
    tasks.spawn(serve_lan(app.clone(), state.clone()));
    for listener in listeners {
        let mut shutdown = state.shutdown.clone();
        let app = app.clone();
        tasks.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown.wait_for(|stop| *stop).await;
                })
                .await
                .context("web server stopped")
        });
    }
    tasks.spawn(first_scan(state.clone()));
    tasks.spawn(index_saved_clips(state.clone()));
    tasks.spawn(hourly_cleanup(state));
    let mut result = Ok(());
    while let Some(done) = tasks.join_next().await {
        match done {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if result.is_ok() {
                    result = Err(e);
                }
            }
            Err(e) => tracing::warn!("a web task panicked: {e}"),
        }
    }
    result
}

async fn first_scan(state: AppState) -> Result<()> {
    let library = state.library.clone();
    match tokio::task::spawn_blocking(move || library.scan()).await {
        Ok(Ok(report)) if report.changed() => state.library.notify_changed(),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::warn!("can't scan the clips folder: {e:#}"),
        Err(e) => tracing::warn!("the clip scan failed: {e}"),
    }
    Ok(())
}

/// Adds every saved clip to the library (and queues its thumbnails) even when no browser is open, and hands
/// it to the Discord and cloud queues.
async fn index_saved_clips(state: AppState) -> Result<()> {
    let mut bus = state.bus.subscribe();
    let mut shutdown = state.shutdown.clone();
    loop {
        tokio::select! {
            event = bus.recv() => match event {
                Ok(Event::ClipSaved { path }) => {
                    let st = state.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        match st.library.add_file(&path) {
                            Ok(clip) => match discord::clip_info(&st.library, &st.thumbs_dir, &clip) {
                                Ok(info) => {
                                    // The cloud queue first: a Discord post may want the clip's link.
                                    st.cloud.clip_saved(&info);
                                    st.discord.clip_saved(&info);
                                }
                                Err(e) => tracing::warn!("can't queue {} for Discord: {e:#}", path.display()),
                            },
                            Err(e) => tracing::warn!("can't add {} to the library: {e:#}", path.display()),
                        }
                    })
                    .await;
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            () = stopped(&mut shutdown) => return Ok(()),
        }
    }
}

/// Runs the age/size cleanup a minute after start and then every hour.
async fn hourly_cleanup(state: AppState) -> Result<()> {
    let mut shutdown = state.shutdown.clone();
    let mut wait = Duration::from_secs(60);
    loop {
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = stopped(&mut shutdown) => return Ok(()),
        }
        wait = Duration::from_secs(3600);
        let library = state.library.clone();
        let clips = state.control.config().clips;
        match tokio::task::spawn_blocking(move || library.cleanup(&clips)).await {
            Ok(Ok((removed, freed))) if removed > 0 => {
                tracing::info!("cleanup removed {removed} clips, freeing {freed} bytes");
                state.library.notify_changed();
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!("cleanup failed: {e:#}"),
            Err(e) => tracing::warn!("cleanup panicked: {e}"),
        }
    }
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(state.status.snapshot())
}

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        axum::Json(json!({ "error": "no such API call" })),
    )
}

async fn events(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    ext: axum::http::Extensions,
) -> Response {
    let remote = auth::is_remote(&ext);
    ws.on_upgrade(move |socket| push_events(socket, state, remote))
}

async fn send_json(socket: &mut WebSocket, value: &Value) -> bool {
    socket.send(Message::Text(value.to_string().into())).await.is_ok()
}

/// Serves the local network while `state.lan` is on: opens the listener when it goes on, and closes it (with
/// the idle connections) when it goes off, until shutdown.
async fn serve_lan(app: IntoMakeServiceWithConnectInfo<Router, SocketAddr>, state: AppState) -> Result<()> {
    let lan = state.lan.clone();
    let mut want = lan.want.subscribe();
    let mut shutdown = state.shutdown.clone();
    loop {
        if *want.borrow_and_update() {
            match bind_lan_retrying(lan.port).await {
                Ok(listener) => {
                    *lan.error.lock().unwrap() = None;
                    tracing::info!(
                        "web UI open to the local network on port {} (PIN required)",
                        lan.port
                    );
                    // Graceful shutdown drops the listener at once but waits for requests in flight (a long
                    // video stream) to finish; let that drain on its own so switching back on isn't held up.
                    let (mut off, mut stop) = (want.clone(), shutdown.clone());
                    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
                    let server = axum::serve(listener, app.clone()).with_graceful_shutdown(async move {
                        tokio::select! {
                            _ = off.wait_for(|on| !*on) => {}
                            () = stopped(&mut stop) => {}
                        }
                        let _ = closed_tx.send(());
                    });
                    tokio::spawn(async move {
                        if let Err(e) = server.await {
                            tracing::warn!("the local network server stopped: {e}");
                        }
                    });
                    let _ = closed_rx.await;
                    if *shutdown.borrow() {
                        return Ok(());
                    }
                    tracing::info!("web UI closed to the local network");
                    continue;
                }
                Err(e) => {
                    tracing::warn!("LAN access is on but can't open: {e:#}");
                    *lan.error.lock().unwrap() = Some(format!("{e:#}"));
                }
            }
        } else {
            *lan.error.lock().unwrap() = None;
        }
        tokio::select! {
            changed = want.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
            }
            () = stopped(&mut shutdown) => return Ok(()),
        }
    }
}

/// [`bind_lan`], retried for up to a second: right after access went off and on again, the old listener may
/// not be dropped yet.
async fn bind_lan_retrying(port: u16) -> Result<TcpListener> {
    let mut tries = 0;
    loop {
        match bind_lan(port).await {
            Ok(listener) => return Ok(listener),
            Err(e) if tries >= 10 => return Err(e),
            Err(_) => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Resolves when access from the local network goes off (never, if the switch is gone).
async fn lan_closed(lan: &mut watch::Receiver<bool>) {
    if lan.wait_for(|on| !*on).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Status on connect and on every change, plus saved clips, failures, library changes and audio levels. A stream
/// to another device ends when access from the local network goes off.
async fn push_events(mut socket: WebSocket, state: AppState, remote: bool) {
    let mut lan = state.lan.want.subscribe();
    let mut changes = state.status.subscribe();
    let mut bus = state.bus.subscribe();
    let mut library = state.library.subscribe();
    let mut posts = state.discord.subscribe();
    let mut uploads = state.cloud.subscribe();
    let mut shutdown = state.shutdown.clone();
    let mut send_status = true;
    let mut meter = tokio::time::interval(LEVELS_EVERY);
    meter.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if send_status {
            let mut message = serde_json::to_value(state.status.snapshot()).expect("status is always JSON");
            message["type"] = "status".into();
            if !send_json(&mut socket, &message).await {
                return;
            }
            send_status = false;
        }
        tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() {
                    break;
                }
                send_status = true;
            }
            _ = meter.tick() => {
                let levels = state.control.levels();
                if !levels.is_empty() && !send_json(&mut socket, &json!({ "type": "levels", "levels": levels })).await {
                    return;
                }
            }
            event = bus.recv() => match event {
                Ok(Event::ClipSaved { path }) => {
                    let lib = state.library.clone();
                    match tokio::task::spawn_blocking(move || lib.add_file(&path)).await {
                        Ok(Ok(clip)) => {
                            if !send_json(&mut socket, &json!({ "type": "clip_saved", "clip": clip })).await {
                                return;
                            }
                        }
                        Ok(Err(e)) => tracing::warn!("a saved clip can't be added to the library: {e:#}"),
                        Err(e) => tracing::warn!("adding a saved clip failed: {e}"),
                    }
                }
                Ok(Event::ClipFailed { message }) => {
                    if !send_json(&mut socket, &json!({ "type": "clip_failed", "message": message })).await {
                        return;
                    }
                }
                Ok(_) => {}
                // Missed some events: have the UI refetch the list.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if !send_json(&mut socket, &json!({ "type": "library" })).await {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            changed = library.recv() => {
                if matches!(changed, Err(broadcast::error::RecvError::Closed)) {
                    break;
                }
                // One message for a burst of changes (thumbnails finishing, a bulk cleanup).
                tokio::time::sleep(LIBRARY_DEBOUNCE).await;
                while library.try_recv().is_ok() {}
                if !send_json(&mut socket, &json!({ "type": "library" })).await {
                    return;
                }
            }
            changed = posts.recv() => {
                if matches!(changed, Err(broadcast::error::RecvError::Closed)) {
                    break;
                }
                while posts.try_recv().is_ok() {}
                if !send_json(&mut socket, &json!({ "type": "discord" })).await {
                    return;
                }
            }
            changed = uploads.recv() => {
                if matches!(changed, Err(broadcast::error::RecvError::Closed)) {
                    break;
                }
                while uploads.try_recv().is_ok() {}
                if !send_json(&mut socket, &json!({ "type": "cloud" })).await {
                    return;
                }
            }
            () = stopped(&mut shutdown) => break,
            () = lan_closed(&mut lan), if remote => break,
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

/// Resolves when clipr shuts down. (Returns `()` so no watch guard is held across later awaits.)
async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
}
