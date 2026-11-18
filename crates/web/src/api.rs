//! The REST handlers (see `docs/api.md`). Blocking work (library, trim, cleanup) runs in `spawn_blocking`.

use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{Extensions, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use clipr_core::config::{AudioConfig, Config, FilterKind};
use clipr_library::{Clip, ClipFilter, LibraryError};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::AppState;
use crate::auth;

/// The UI lists clips often; rescan only when the last scan is older than this.
const SCAN_MAX_AGE: Duration = Duration::from_secs(10);

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast_ref::<LibraryError>() {
            Some(LibraryError::NotFound(m)) => Self::new(StatusCode::NOT_FOUND, m.clone()),
            Some(LibraryError::Invalid(m)) => Self::bad_request(m.clone()),
            None => {
                tracing::warn!("request failed: {e:#}");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

pub(crate) type ApiResult<T> = std::result::Result<T, ApiError>;

pub fn parse_json<T: DeserializeOwned>(body: &Bytes) -> ApiResult<T> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("can't read the request: {e}")))
}

/// Like [`parse_json`], but an empty body means `T::default()`.
fn parse_json_or_default<T: DeserializeOwned + Default>(body: &Bytes) -> ApiResult<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    parse_json(body)
}

fn query<T: DeserializeOwned>(uri: &Uri) -> ApiResult<T> {
    Query::<T>::try_from_uri(uri)
        .map(|q| q.0)
        .map_err(|e| ApiError::bad_request(format!("bad query: {e}")))
}

pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> ApiResult<T> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("internal error: {e}"),
        )),
    }
}

fn no_content() -> StatusCode {
    StatusCode::NO_CONTENT
}

// ---- actions ------------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct ClipReq {
    seconds: Option<u32>,
}

pub async fn save_clip(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: ClipReq = parse_json_or_default(&body)?;
    if req.seconds.is_some_and(|n| !(1..=3600).contains(&n)) {
        return Err(ApiError::bad_request("seconds must be between 1 and 3600"));
    }
    s.control.save_clip(req.seconds);
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct OnReq {
    on: bool,
}

pub async fn set_buffer(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: OnReq = parse_json(&body)?;
    s.control.set_buffering(req.on);
    Ok(no_content())
}

pub async fn set_recording(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: OnReq = parse_json(&body)?;
    s.control.set_recording(req.on);
    Ok(no_content())
}

pub async fn mark(State(s): State<AppState>) -> StatusCode {
    s.control.mark();
    no_content()
}

#[derive(Deserialize)]
struct MutedReq {
    muted: bool,
}

pub async fn set_mic(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: MutedReq = parse_json(&body)?;
    s.control.set_mic_muted(req.muted);
    Ok(no_content())
}

#[derive(Deserialize)]
struct MuteReq {
    source: String,
    muted: bool,
}

/// Mutes or unmutes one source in clips (the mixer's Mute buttons).
pub async fn set_mute(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: MuteReq = parse_json(&body)?;
    let source = req.source.trim();
    if source.is_empty() || source.len() > 260 || source.contains(['/', '\\', ':']) {
        return Err(ApiError::bad_request("Not an audio source."));
    }
    s.control.set_muted(source, req.muted);
    Ok(no_content())
}

/// Performance samples for Home's graphs.
pub async fn perf(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(s.control.perf())
}

/// A page shows the level meters (see `docs/api.md`).
pub async fn watch_levels(State(s): State<AppState>) -> StatusCode {
    s.control.watch_levels();
    no_content()
}

#[derive(Deserialize)]
struct SourceVolumeReq {
    source: String,
    volume: u32,
}

/// Sets one audio source's volume, saved and applied right away without restarting audio (the mixer's
/// faders). Returns the saved settings.
pub async fn set_volume(State(s): State<AppState>, body: Bytes) -> ApiResult<Json<Config>> {
    let req: SourceVolumeReq = parse_json(&body)?;
    if req.volume > 200 {
        return Err(ApiError::bad_request("The volume must be between 0 and 200"));
    }
    let source = req.source.trim().to_ascii_lowercase();
    if source.is_empty() || source.contains(['/', '\\', ':']) {
        return Err(ApiError::bad_request("Unknown audio source"));
    }
    // Read, change and save under one lock, so two faders moved at once don't undo each other.
    static VOLUMES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let st = s.clone();
    let saved = blocking(move || {
        let _one_at_a_time = VOLUMES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut config = st.control.config();
        config.audio.set_volume(&source, req.volume);
        validate_config(&config).map_err(|e| LibraryError::Invalid(format!("{e:#}")))?;
        st.control
            .set_config(config)
            .map_err(|e| LibraryError::Invalid(format!("{e:#}")))?;
        Ok(st.control.config())
    })
    .await?;
    Ok(Json(saved))
}

#[derive(Deserialize)]
struct VolumeReq {
    volume: u32,
}

/// Plays the save sound at a volume that isn't saved yet (the settings page's Test button).
pub async fn test_sound(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: VolumeReq = parse_json(&body)?;
    if req.volume > 100 {
        return Err(ApiError::bad_request("The volume must be between 0 and 100"));
    }
    s.control.test_sound(req.volume);
    Ok(no_content())
}

/// The live preview of what is captured: a small JPEG, or 204 while nothing is captured yet. The page asks
/// a few times a second while it shows the preview; asking is what keeps snapshots coming. It shows the whole
/// screen, so other devices only get it when the PC's user allowed that (`web.lan_preview`).
#[derive(Deserialize, Default)]
pub struct PreviewQuery {
    /// Pictures a second the page wants (1–30; default 5).
    fps: Option<u32>,
}

pub async fn preview(
    State(s): State<AppState>,
    ext: Extensions,
    Query(q): Query<PreviewQuery>,
) -> ApiResult<Response> {
    if auth::is_remote(&ext) && !s.control.config().web.lan_preview {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "The live preview shows your whole screen, so it only shows on the PC clipr runs on. \
             To see it here, turn on \"Live preview on other devices\" under Settings → Network on the PC.",
        ));
    }
    let control = s.control.clone();
    let fps = q.fps.unwrap_or(5).clamp(1, 30);
    let jpeg = tokio::task::spawn_blocking(move || control.preview(fps))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "the preview failed"))?
        .map_err(|e| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("The preview isn't available: {e}"),
            )
        })?;
    Ok(match jpeg {
        Some(jpeg) => (
            [
                (axum::http::header::CONTENT_TYPE, "image/jpeg"),
                (axum::http::header::CACHE_CONTROL, "no-store"),
            ],
            Body::from(jpeg.as_ref().clone()),
        )
            .into_response(),
        None => no_content().into_response(),
    })
}

// ---- settings -----------------------------------------------------------------------------------------

pub async fn get_config(State(s): State<AppState>, ext: Extensions) -> Json<Config> {
    let mut config = s.control.config();
    if auth::is_remote(&ext) {
        crate::discord::hide_urls(&mut config);
        crate::cloud::hide_secret(&mut config.cloud);
    }
    Json(config)
}

pub async fn system(State(s): State<AppState>, ext: Extensions) -> Json<Value> {
    let mut info = s.control.system_info();
    // Whether this UI is another device (webhook URLs are hidden from it).
    info["remote"] = auth::is_remote(&ext).into();
    // Access from the local network is on, but its listener couldn't open.
    info["lan_error"] = s.lan.error().into();
    // The address other devices at home use, for the Network tab.
    info["lan_ip"] = lan_ip().map(|ip| ip.to_string()).into();
    Json(info)
}

/// This PC's address on the local network: the one Windows would send from towards the internet. Connecting a
/// UDP socket only picks the route; nothing is sent.
fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 80)).ok()?; // TEST-NET-1: never reached
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

// ---- crash reports ------------------------------------------------------------------------------------

pub async fn crashes(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    let control = s.control.clone();
    Ok(Json(blocking(move || Ok(control.crashes())).await?))
}

#[derive(Deserialize)]
struct CrashReq {
    id: String,
}

pub async fn dismiss_crash(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: CrashReq = parse_json(&body)?;
    s.control
        .dismiss_crash(&req.id)
        .map_err(|m| ApiError::new(StatusCode::NOT_FOUND, m))?;
    Ok(no_content())
}

/// Opens Explorer at the dump file: only on the PC itself.
pub async fn reveal_crash(State(s): State<AppState>, ext: Extensions, body: Bytes) -> ApiResult<StatusCode> {
    if auth::is_remote(&ext) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "The dump file can only be opened on the PC clipr runs on.",
        ));
    }
    let req: CrashReq = parse_json(&body)?;
    s.control
        .reveal_crash(&req.id)
        .map_err(|m| ApiError::new(StatusCode::NOT_FOUND, m))?;
    Ok(no_content())
}

// ---- updates ------------------------------------------------------------------------------------------

pub async fn update_status(State(s): State<AppState>) -> Json<Value> {
    Json(s.control.update_status())
}

pub async fn check_update(State(s): State<AppState>) -> StatusCode {
    s.control.check_update();
    StatusCode::ACCEPTED
}

/// Installs the update, which quits and restarts clipr: only from this PC, never from a LAN device.
pub async fn apply_update(State(s): State<AppState>, ext: Extensions) -> ApiResult<StatusCode> {
    if auth::is_remote(&ext) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Updates can only be installed from the PC clipr runs on.",
        ));
    }
    s.control
        .apply_update()
        .map_err(|message| ApiError::new(StatusCode::CONFLICT, message))?;
    Ok(StatusCode::ACCEPTED)
}

/// Lower-cased, backslashes, no trailing slash, no `\\?\` prefix: for comparing paths as text.
fn norm_path(p: &str) -> String {
    let p = p.replace('/', "\\");
    let p = p.strip_prefix("\\\\?\\").unwrap_or(&p);
    p.trim_end_matches('\\').to_lowercase()
}

/// The audio settings beyond the volume: sources, timing, push-to-talk and every filter.
fn check_audio(a: &AudioConfig) -> Result<()> {
    let int = |name: &str, v: i64, lo: i64, hi: i64| -> Result<()> {
        if (lo..=hi).contains(&v) {
            Ok(())
        } else {
            bail!("{name} must be between {lo} and {hi}")
        }
    };
    int("System audio volume", a.system_volume.into(), 0, 200)?;
    if a.volumes.len() > 16 {
        bail!("Too many source volumes");
    }
    for (source, &v) in &a.volumes {
        int(&format!("The volume of {source}"), v.into(), 0, 200)?;
    }
    int("Microphone offset", a.mic_offset_ms.into(), -5000, 5000)?;
    int("System audio offset", a.system_offset_ms.into(), -5000, 5000)?;
    int("Push-to-talk release", a.ptt_release_ms.into(), 0, 5000)?;
    if a.apps.len() > 4 {
        bail!("Record at most 4 apps on their own tracks");
    }
    for app in &a.apps {
        let app = app.trim();
        if app.is_empty() || app.contains(['/', '\\', ':']) {
            bail!("An app must be a program name like discord.exe, not {app:?}");
        }
    }
    let ptt = a
        .push_to_talk
        .rsplit('+')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if ![
        "mouse4",
        "mouse5",
        "mousemiddle",
        "mouse3",
        "xbutton1",
        "xbutton2",
    ]
    .contains(&ptt.as_str())
    {
        clipr_core::hotkey::parse(&ptt).map_err(|e| anyhow::anyhow!("Push-to-talk key: {e}"))?;
    }
    // Filters of an app that was removed are kept (and unused), so adding it back brings them back.
    if a.filters.len() > 32 {
        bail!("Too many sources with filters");
    }
    for (source, filters) in &a.filters {
        if source.trim().is_empty() || source.len() > 100 || source.trim() != source {
            bail!("Filters are set for a source with no proper name: {source:?}");
        }
        if filters.len() > 16 {
            bail!("A source can have at most 16 filters");
        }
        for f in filters {
            match f.kind {
                FilterKind::NoiseSuppression { strength } => {
                    int("Noise suppression strength", strength.into(), 0, 100)?
                }
                FilterKind::NoiseGate {
                    open_db,
                    close_db,
                    attack_ms,
                    hold_ms,
                    release_ms,
                } => {
                    int("Gate open level", open_db.into(), -96, 0)?;
                    int("Gate close level", close_db.into(), -96, 0)?;
                    // A close level above the open level is treated as equal to it (see the gate).
                    int("Gate attack", attack_ms.into(), 1, 500)?;
                    int("Gate hold", hold_ms.into(), 0, 5000)?;
                    int("Gate release", release_ms.into(), 1, 5000)?;
                }
                FilterKind::Compressor {
                    threshold_db,
                    ratio,
                    attack_ms,
                    release_ms,
                    makeup_db,
                } => {
                    int("Compressor threshold", threshold_db.into(), -60, 0)?;
                    int("Compressor ratio", ratio.into(), 1, 20)?;
                    int("Compressor attack", attack_ms.into(), 1, 500)?;
                    int("Compressor release", release_ms.into(), 1, 5000)?;
                    int("Compressor output gain", makeup_db.into(), -30, 30)?;
                }
                FilterKind::Limiter {
                    ceiling_db,
                    release_ms,
                } => {
                    int("Limiter ceiling", ceiling_db.into(), -30, 0)?;
                    int("Limiter release", release_ms.into(), 1, 5000)?;
                }
                FilterKind::Gain { db } => int("Gain", db.into(), -30, 30)?,
            }
        }
    }
    Ok(())
}

/// A clips or recordings folder may be empty (the default) or an absolute path that isn't a drive root or somewhere
/// Windows or the user's profile lives.
fn check_folder(what: &str, folder: &str) -> Result<()> {
    let folder = folder.trim();
    if folder.is_empty() {
        return Ok(());
    }
    let path = FsPath::new(folder);
    if !path.is_absolute() {
        bail!("{what} must be a full path like D:\\Clips");
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("{what} can't contain ..");
    }
    if path.parent().is_none() {
        bail!("{what} can't be a whole drive; pick a folder on it");
    }
    let here = norm_path(folder);
    let inside = |vars: &[&str]| {
        vars.iter().filter_map(|v| std::env::var(v).ok()).any(|d| {
            let d = norm_path(&d);
            !d.is_empty() && (here == d || here.starts_with(&format!("{d}\\")))
        })
    };
    if inside(&[
        "SystemRoot",
        "windir",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
    ]) {
        bail!("{what} can't be inside Windows or Program Files");
    }
    let profile = std::env::var("USERPROFILE")
        .map(|p| norm_path(&p))
        .unwrap_or_default();
    if !profile.is_empty() && here == profile {
        bail!("{what} can't be your whole user folder; pick a folder inside it");
    }
    Ok(())
}

/// Checks a whole `Config` the way the settings page needs: sane ranges, hotkeys that parse, no two actions on
/// the same hotkey.
pub fn validate_config(c: &Config) -> Result<()> {
    let in_range = |name: &str, v: u32, lo: u32, hi: u32| -> Result<()> {
        if (lo..=hi).contains(&v) {
            Ok(())
        } else {
            bail!("{name} must be between {lo} and {hi}")
        }
    };
    in_range("Frame rate", c.video.fps, 10, 240)?;
    in_range("Clip length", c.replay.clip_secs, 5, 600)?;
    in_range("Save sound volume", c.replay.sound_volume, 0, 100)?;
    in_range("Buffer length", c.replay.buffer_secs, c.replay.clip_secs, 1800).map_err(|_| {
        anyhow::anyhow!(
            "Buffer length must be between the clip length ({} s) and 1800 s",
            c.replay.clip_secs
        )
    })?;
    in_range("Video bitrate", c.video.bitrate_kbps, 1000, 150_000)?;
    if c.video.height != 0 {
        in_range("Video height", c.video.height, 360, 4320)?;
    }
    for p in &c.games.profiles {
        if p.game.trim().is_empty() {
            bail!("A per-game profile needs a game name");
        }
        if let Some(v) = p.fps {
            in_range("A profile's frame rate", v, 10, 240)?;
        }
        if let Some(v) = p.bitrate_kbps {
            in_range("A profile's bitrate", v, 1000, 150_000)?;
        }
        if let Some(v) = p.height.filter(|&h| h != 0) {
            in_range("A profile's height", v, 360, 4320)?;
        }
    }
    in_range("Microphone volume", c.audio.mic_volume, 0, 200)?;
    check_audio(&c.audio)?;
    if ![96, 128, 160, 192].contains(&c.audio.bitrate_kbps) {
        bail!("Audio bitrate must be 96, 128, 160 or 192");
    }
    if c.web.port == 0 {
        bail!("The web port can't be 0");
    }
    if !c.web.lan_pin.is_empty()
        && !(c.web.lan_pin.len() == 6 && c.web.lan_pin.bytes().all(|b| b.is_ascii_digit()))
    {
        bail!("The LAN PIN must be 6 digits");
    }
    check_folder("The clips folder", &c.clips.folder)?;
    check_folder("The recordings folder", &c.recording.folder)?;
    if let Some(rec) = c.recording.own_dir(&c.clips) {
        let (rec, clips) = (
            norm_path(&rec.to_string_lossy()),
            norm_path(&c.clips.dir().to_string_lossy()),
        );
        if rec == clips || rec.starts_with(&format!("{clips}\\")) || clips.starts_with(&format!("{rec}\\")) {
            bail!("The recordings folder can't be inside the clips folder (or the other way round)");
        }
    }
    crate::discord::check(&c.discord)?;
    clipr_cloud::check(&c.cloud)?;
    let accent = c.ui.accent.as_bytes();
    if !(accent.len() == 7 && accent[0] == b'#' && accent[1..].iter().all(u8::is_ascii_hexdigit)) {
        bail!("The accent colour must look like #8B5CF6");
    }
    let h = &c.hotkeys;
    let keys = [
        ("Save clip", &h.save_clip),
        ("Save long clip", &h.save_long_clip),
        ("Toggle buffer", &h.toggle_buffer),
        ("Toggle recording", &h.toggle_recording),
        ("Mark", &h.mark),
        ("Mute microphone", &h.toggle_mic),
    ];
    let mut used: Vec<(u32, u32, &str)> = Vec::new();
    for (name, text) in keys {
        let parsed = clipr_core::hotkey::parse(text).map_err(|e| anyhow::anyhow!("{name} hotkey: {e}"))?;
        if let Some(k) = parsed {
            if let Some((_, _, other)) = used.iter().find(|(m, v, _)| (*m, *v) == (k.mods, k.vk)) {
                bail!("{other} and {name} both use the hotkey {text}");
            }
            used.push((k.mods, k.vk, name));
        }
    }
    Ok(())
}

pub async fn put_config(State(s): State<AppState>, ext: Extensions, body: Bytes) -> ApiResult<Json<Config>> {
    let mut config: Config = parse_json(&body)?;
    for w in &mut config.discord.webhooks {
        w.name = w.name.trim().to_string();
        w.url = w.url.trim().to_string();
    }
    crate::discord::keep_urls(&ext, &s.control.config().discord, &mut config.discord)?;
    for text in [
        &mut config.cloud.endpoint,
        &mut config.cloud.region,
        &mut config.cloud.bucket,
        &mut config.cloud.access_key_id,
        &mut config.cloud.secret_access_key,
        &mut config.cloud.public_url,
        &mut config.cloud.prefix,
    ] {
        *text = text.trim().to_string();
    }
    crate::cloud::keep_bucket(&ext, &s.control.config().cloud, &mut config.cloud)?;
    validate_config(&config).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    if auth::is_remote(&ext) {
        // Where files go and who can reach the API are for the PC's own user to decide.
        let now = s.control.config();
        if config.clips.folder != now.clips.folder
            || config.recording.folder != now.recording.folder
            || config.web != now.web
        {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "the clips and recordings folders and the web settings can only be changed on the PC running clipr",
            ));
        }
    }
    if config.web.lan && config.web.lan_pin.is_empty() {
        config.web.lan_pin = auth::generate_pin();
    }
    let folders = |c: &Config| (c.clips.dir(), c.recording.own_dir(&c.clips));
    let before = folders(&s.control.config());
    let after = folders(&config);
    let st = s.clone();
    let saved = blocking(move || {
        st.control
            .set_config(config)
            .map_err(|e| LibraryError::Invalid(format!("{e:#}")))?;
        if before != after {
            st.library.set_clips_dir(after.0);
            st.library.set_recordings_dir(after.1);
            st.library.scan()?;
            st.library.notify_changed();
        }
        Ok(st.control.config())
    })
    .await?;
    s.lan.set(saved.web.lan);
    let mut saved = saved;
    if auth::is_remote(&ext) {
        crate::discord::hide_urls(&mut saved);
        crate::cloud::hide_secret(&mut saved.cloud);
    }
    Ok(Json(saved))
}

// ---- library ------------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct ClipsQuery {
    q: Option<String>,
    game: Option<String>,
    shelf: Option<String>,
    favorite: Option<String>,
    kind: Option<String>,
}

pub async fn list_clips(State(s): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Clip>>> {
    let q: ClipsQuery = query(&uri)?;
    let non_empty = |v: Option<String>| v.filter(|v| !v.is_empty());
    let filter = ClipFilter {
        q: non_empty(q.q),
        game: non_empty(q.game),
        shelf: non_empty(q.shelf),
        favorite: q.favorite.as_deref().is_some_and(|f| f == "true" || f == "1"),
        kind: non_empty(q.kind),
    };
    if let Some(k) = &filter.kind {
        if k != "clip" && k != "recording" {
            return Err(ApiError::bad_request("kind must be clip or recording"));
        }
    }
    let lib = s.library.clone();
    let clips = blocking(move || {
        lib.scan_if_stale(SCAN_MAX_AGE)?;
        lib.list(&filter)
    })
    .await?;
    Ok(Json(clips))
}

pub async fn games(State(s): State<AppState>) -> ApiResult<Json<Vec<clipr_library::GameInfo>>> {
    let lib = s.library.clone();
    Ok(Json(blocking(move || lib.games()).await?))
}

pub async fn shelves(State(s): State<AppState>) -> ApiResult<Json<Vec<String>>> {
    let lib = s.library.clone();
    Ok(Json(blocking(move || lib.shelves()).await?))
}

#[derive(Deserialize)]
struct PatchReq {
    id: String,
    title: Option<String>,
    favorite: Option<bool>,
    shelves: Option<Vec<String>>,
}

pub async fn patch_clip(State(s): State<AppState>, body: Bytes) -> ApiResult<Json<Clip>> {
    let req: PatchReq = parse_json(&body)?;
    let old_id = req.id.clone();
    let st = s.clone();
    let clip = blocking(move || {
        let clip = st.library.update(
            &req.id,
            req.title.as_deref(),
            req.favorite,
            req.shelves.as_deref(),
        )?;
        // A new title renames the file: posts still waiting and the cloud copy follow it.
        if req.title.is_some() {
            match crate::discord::clip_info(&st.library, &st.thumbs_dir, &clip) {
                Ok(info) => {
                    st.discord.clip_renamed(&old_id, &info);
                    st.cloud.clip_renamed(&old_id, &info);
                }
                Err(e) => tracing::warn!("can't update Discord posts for a renamed clip: {e:#}"),
            }
        }
        Ok(clip)
    })
    .await?;
    s.library.notify_changed();
    Ok(Json(clip))
}

#[derive(Deserialize)]
struct IdQuery {
    id: String,
}

pub async fn delete_clip(State(s): State<AppState>, uri: Uri) -> ApiResult<StatusCode> {
    let q: IdQuery = query(&uri)?;
    let lib = s.library.clone();
    blocking(move || lib.trash(&q.id)).await?;
    s.library.notify_changed();
    Ok(no_content())
}

#[derive(Deserialize)]
struct TrimReq {
    id: String,
    start: f64,
    end: f64,
    #[serde(default)]
    replace: bool,
}

/// `dir/<stem>.<ext>`, or `dir/<stem> 2.<ext>`, … whichever is free.
fn free_name(dir: &FsPath, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem} {n}.{ext}")))
        .find(|p| !p.exists())
        .expect("an endless range")
}

pub async fn trim_clip(State(s): State<AppState>, body: Bytes) -> ApiResult<Json<Clip>> {
    let req: TrimReq = parse_json(&body)?;
    if !(req.start.is_finite() && req.end.is_finite() && req.start >= 0.0 && req.end > req.start) {
        return Err(ApiError::bad_request(
            "the end of the trim must be after the start",
        ));
    }
    let st = s.clone();
    let clip = blocking(move || {
        let src = st.library.resolve(&req.id)?;
        let clip = st.library.get(&req.id)?;
        if !req.id.to_ascii_lowercase().ends_with(".mp4") {
            return Err(LibraryError::Invalid("only MP4 clips can be trimmed".into()).into());
        }
        let end = clip.duration_secs.map_or(req.end, |d| req.end.min(d));
        if end - req.start < 0.1 {
            return Err(LibraryError::Invalid("nothing is left after trimming".into()).into());
        }
        let dir = src
            .parent()
            .ok_or_else(|| anyhow::anyhow!("the clip has no folder"))?;
        let dst = if req.replace {
            dir.join(format!(".clipr-trim-{}.mp4", clip.title))
        } else {
            free_name(dir, &format!("{}_trim", clip.title), "mp4")
        };
        if let Err(e) = st.control.trim(&src, &dst, req.start, end) {
            let _ = std::fs::remove_file(&dst);
            return Err(e.context("trimming failed"));
        }
        let markers: Vec<f64> = clip
            .markers
            .iter()
            .filter(|m| (req.start..=end).contains(*m))
            .map(|m| m - req.start)
            .collect();
        let finished = if req.replace {
            std::fs::rename(&dst, &src).map_err(|e| {
                let _ = std::fs::remove_file(&dst);
                anyhow::anyhow!("couldn't replace the original (is it open in another program?): {e}")
            })?;
            src
        } else {
            dst
        };
        st.library.write_markers(&finished, &markers)?;
        let out = st.library.add_file(&finished)?;
        // The trimmed file replaced the original: its cloud copy is out of date.
        if req.replace {
            match crate::discord::clip_info(&st.library, &st.thumbs_dir, &out) {
                Ok(info) => st.cloud.clip_replaced(&info),
                Err(e) => tracing::warn!("can't update the cloud copy of a trimmed clip: {e:#}"),
            }
        }
        st.library.notify_changed();
        Ok(out)
    })
    .await?;
    Ok(Json(clip))
}

#[derive(Deserialize, Default)]
struct RevealReq {
    id: Option<String>,
}

pub async fn reveal(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: RevealReq = parse_json_or_default(&body)?;
    let st = s.clone();
    blocking(move || {
        let path = match req.id {
            Some(id) => st.library.resolve(&id)?,
            None => st.library.clips_dir(),
        };
        st.control.reveal(&path);
        Ok(())
    })
    .await?;
    Ok(no_content())
}

pub async fn cleanup(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    let st = s.clone();
    let (removed, freed) = blocking(move || st.library.cleanup(&st.control.config().clips)).await?;
    if removed > 0 {
        s.library.notify_changed();
    }
    Ok(Json(json!({ "removed": removed, "freed_bytes": freed })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CleanupPreviewReq {
    /// The limits to try (the settings page's unsaved ones); the saved ones when missing.
    clips: Option<clipr_core::config::ClipsConfig>,
    /// How far ahead to look, in hours.
    hours: Option<u32>,
}

/// What the cleanup would delete in the next `hours` (default 24) with the given or saved limits. Deletes
/// nothing. `{count, bytes, clips}` with the first few clips, oldest first.
pub async fn cleanup_preview(State(s): State<AppState>, body: Bytes) -> ApiResult<Json<Value>> {
    let req: CleanupPreviewReq = if body.is_empty() {
        CleanupPreviewReq::default()
    } else {
        parse_json(&body)?
    };
    let mut clips = req.clips.unwrap_or_else(|| s.control.config().clips);
    // Only the limits come from the request; the folder is always the saved one.
    clips.folder = s.control.config().clips.folder;
    let hours = req.hours.unwrap_or(24).min(24 * 365);
    let lib = s.library.clone();
    let doomed = blocking(move || {
        lib.cleanup_preview(&clips, std::time::Duration::from_secs(u64::from(hours) * 3600))
    })
    .await?;
    let bytes: u64 = doomed.iter().map(|c| c.size_bytes).sum();
    let count = doomed.len();
    let first: Vec<Clip> = doomed.into_iter().take(5).collect();
    Ok(Json(json!({ "count": count, "bytes": bytes, "clips": first })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PickFolderReq {
    title: String,
    start: String,
}

/// Opens the Windows folder picker on the PC and waits for it: `{path}`, null when cancelled. Only on the PC
/// itself; one picker at a time.
pub async fn pick_folder(State(s): State<AppState>, ext: Extensions, body: Bytes) -> ApiResult<Json<Value>> {
    if auth::is_remote(&ext) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Folders can only be picked on the PC clipr runs on.",
        ));
    }
    static OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if OPEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "A folder picker is already open.",
        ));
    }
    struct Close;
    impl Drop for Close {
        fn drop(&mut self) {
            OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let close = Close;
    let req: PickFolderReq = if body.is_empty() {
        PickFolderReq::default()
    } else {
        parse_json(&body)?
    };
    let control = s.control.clone();
    let picked = tokio::task::spawn_blocking(move || {
        let _close = close;
        let title = if req.title.trim().is_empty() {
            "Pick a folder"
        } else {
            req.title.trim()
        };
        control.pick_folder(title, req.start.trim())
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the folder picker failed: {e}"),
        )
    })?
    .map_err(|m| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, m))?;
    Ok(Json(json!({ "path": picked })))
}

pub async fn disk(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    let lib = s.library.clone();
    let (free, total, clips) = blocking(move || lib.disk()).await?;
    Ok(Json(
        json!({ "free_bytes": free, "total_bytes": total, "clips_bytes": clips }),
    ))
}

// ---- files --------------------------------------------------------------------------------------------

async fn serve_file(path: PathBuf, mime: &str, req: Request) -> Response {
    let mime: mime_guess::mime::Mime = mime.parse().expect("a valid mime type");
    match ServeFile::new_with_mime(path, &mime).oneshot(req).await {
        Ok(response) => {
            let mut response = response.map(Body::new).into_response();
            // Ask the file's date each time (a cheap 304 when unchanged): a clip replaced by a trim keeps
            // its name, and the browser must not keep playing the old one.
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-cache"),
            );
            response
        }
        Err(never) => match never {},
    }
}

/// `GET /media/<id>`: the video, with Range support.
pub async fn media(State(s): State<AppState>, Path(id): Path<String>, req: Request) -> ApiResult<Response> {
    let path = s.library.resolve(&id)?;
    if !path.is_file() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("there is no clip \"{id}\""),
        ));
    }
    let mime = if id.to_ascii_lowercase().ends_with(".mkv") {
        "video/x-matroska"
    } else {
        "video/mp4"
    };
    Ok(serve_file(path, mime, req).await)
}

/// `GET /thumbs/<id>.jpg` and `/thumbs/<id>.sprite.jpg`.
pub async fn thumb(State(s): State<AppState>, Path(name): Path<String>, req: Request) -> ApiResult<Response> {
    let (id, sprite) = match name.strip_suffix(".sprite.jpg") {
        Some(id) => (id, true),
        None => match name.strip_suffix(".jpg") {
            Some(id) => (id, false),
            None => return Err(ApiError::new(StatusCode::NOT_FOUND, "no such image")),
        },
    };
    let path = s.thumbs_dir.join(clipr_library::thumb_file_name(id, sprite));
    if !path.is_file() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such image (yet)"));
    }
    Ok(serve_file(path, "image/jpeg", req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(f: impl FnOnce(&mut Config)) -> Result<()> {
        let mut c = Config::default();
        f(&mut c);
        validate_config(&c)
    }

    #[test]
    fn clips_folder_is_checked() {
        let folder = |f: &str| cfg(|c| c.clips.folder = f.into());
        assert!(folder("").is_ok());
        assert!(folder("  ").is_ok());
        assert!(folder(r"D:\Clips").is_ok());
        assert!(folder(r"D:\Clips\..\x").is_err());
        assert!(folder("clips").is_err());
        assert!(folder(r"\clips").is_err());
        assert!(folder(r"C:\").is_err());
        assert!(folder("D:/").is_err());
        let windir = std::env::var("SystemRoot").unwrap();
        assert!(folder(&windir).is_err());
        assert!(folder(&format!(r"{}\Temp", windir.to_lowercase())).is_err());
        assert!(folder(r"C:\Program Files").is_err());
        let profile = std::env::var("USERPROFILE").unwrap();
        assert!(folder(&profile).is_err());
        assert!(folder(&format!(r"{profile}\Videos\clipr")).is_ok());
    }

    #[test]
    fn game_profiles_are_checked() {
        use clipr_core::config::GameProfile;
        let profile = |p: GameProfile| cfg(|c| c.games.profiles.push(p));
        assert!(
            profile(GameProfile {
                game: "doom.exe".into(),
                fps: Some(9),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            profile(GameProfile {
                game: "doom.exe".into(),
                fps: Some(120),
                ..Default::default()
            })
            .is_ok()
        );
        assert!(
            profile(GameProfile {
                game: " ".into(),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            profile(GameProfile {
                game: "a".into(),
                height: Some(100),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            profile(GameProfile {
                game: "a".into(),
                bitrate_kbps: Some(5),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn default_config_is_valid() {
        validate_config(&Config::default()).unwrap();
    }

    #[test]
    fn ranges_are_enforced() {
        assert!(cfg(|c| c.video.fps = 9).is_err());
        assert!(cfg(|c| c.video.fps = 241).is_err());
        assert!(cfg(|c| c.video.fps = 144).is_ok());
        assert!(cfg(|c| c.replay.clip_secs = 4).is_err());
        assert!(cfg(|c| c.replay.clip_secs = 601).is_err());
        assert!(
            cfg(|c| c.replay.buffer_secs = 20).is_err(),
            "buffer shorter than the clip"
        );
        assert!(cfg(|c| c.replay.buffer_secs = 1801).is_err());
        assert!(cfg(|c| c.video.bitrate_kbps = 999).is_err());
        assert!(cfg(|c| c.video.bitrate_kbps = 150_001).is_err());
        assert!(cfg(|c| c.video.height = 0).is_ok());
        assert!(cfg(|c| c.video.height = 100).is_err());
        assert!(cfg(|c| c.video.height = 4321).is_err());
        assert!(cfg(|c| c.audio.mic_volume = 201).is_err());
        assert!(cfg(|c| c.audio.apps.push(r"C:\x\discord.exe".into())).is_err());
        assert!(cfg(|c| c.audio.push_to_talk = "Banana".into()).is_err());
        assert!(cfg(|c| c.audio.push_to_talk = "Mouse4".into()).is_ok());
        let filter = |kind| clipr_core::config::AudioFilter { enabled: true, kind };
        let gate = |open_db, close_db| FilterKind::NoiseGate {
            open_db,
            close_db,
            attack_ms: 10,
            hold_ms: 150,
            release_ms: 100,
        };
        assert!(
            cfg(|c| {
                c.audio.filters.insert("mic".into(), vec![filter(gate(-30, -40))]);
            })
            .is_ok()
        );
        assert!(
            cfg(|c| {
                c.audio.filters.insert("mic".into(), vec![filter(gate(-40, -30))]);
            })
            .is_ok(),
            "the gate treats it as equal to the open level"
        );
        assert!(
            cfg(|c| {
                c.audio
                    .filters
                    .insert("mic".into(), vec![filter(FilterKind::Gain { db: 31 })]);
            })
            .is_err()
        );
        assert!(
            cfg(|c| {
                c.audio
                    .filters
                    .insert("spotify.exe".into(), vec![filter(FilterKind::Gain { db: 0 })]);
            })
            .is_ok(),
            "a removed app's filters are kept"
        );
        assert!(
            cfg(|c| {
                c.audio.apps.push("Discord.exe".into());
                c.audio
                    .filters
                    .insert("discord.exe".into(), vec![filter(FilterKind::Gain { db: -6 })]);
            })
            .is_ok()
        );
        assert!(cfg(|c| c.audio.bitrate_kbps = 100).is_err());
        assert!(cfg(|c| c.audio.bitrate_kbps = 192).is_ok());
        assert!(cfg(|c| c.web.lan_pin = "12ab".into()).is_err());
    }

    #[test]
    fn hotkeys_are_checked() {
        assert!(cfg(|c| c.hotkeys.mark = "Ctrl+Nope".into()).is_err());
        assert!(cfg(|c| c.hotkeys.mark = "Ctrl+Shift+M".into()).is_ok());
        let err = cfg(|c| c.hotkeys.mark = "alt+f10".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("Save clip") && err.contains("Mark"), "{err}");
        let err = cfg(|c| c.hotkeys.toggle_mic = "Alt+F9".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("Mute microphone"), "{err}");
        assert!(cfg(|c| c.replay.sound_volume = 101).is_err());
        assert!(cfg(|c| c.replay.sound_volume = 0).is_ok());
    }
}
