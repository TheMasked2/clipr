//! The REST handlers (see `docs/api.md`). Blocking work (library, trim, cleanup) runs in `spawn_blocking`.

use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use clipr_core::config::Config;
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

    fn bad_request(message: impl Into<String>) -> Self {
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

type ApiResult<T> = std::result::Result<T, ApiError>;

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

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> ApiResult<T> {
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

// ---- settings -----------------------------------------------------------------------------------------

pub async fn get_config(State(s): State<AppState>) -> Json<Config> {
    Json(s.control.config())
}

pub async fn system(State(s): State<AppState>) -> Json<Value> {
    Json(s.control.system_info())
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

pub async fn put_config(State(s): State<AppState>, body: Bytes) -> ApiResult<Json<Config>> {
    let mut config: Config = parse_json(&body)?;
    validate_config(&config).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    if config.web.lan && config.web.lan_pin.is_empty() {
        config.web.lan_pin = auth::generate_pin();
    }
    let before = s.control.config().clips.dir();
    let after = config.clips.dir();
    let st = s.clone();
    let saved = blocking(move || {
        st.control
            .set_config(config)
            .map_err(|e| LibraryError::Invalid(format!("{e:#}")))?;
        if before != after {
            st.library.set_clips_dir(after);
            st.library.scan()?;
            st.library.notify_changed();
        }
        Ok(st.control.config())
    })
    .await?;
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
    let lib = s.library.clone();
    let clip = blocking(move || {
        lib.update(
            &req.id,
            req.title.as_deref(),
            req.favorite,
            req.shelves.as_deref(),
        )
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
                anyhow::anyhow!("couldn't replace the original (is it open in another program?): {e}")
            })?;
            src
        } else {
            dst
        };
        st.library.write_markers(&finished, &markers)?;
        let out = st.library.add_file(&finished)?;
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
        Ok(response) => response.map(Body::new).into_response(),
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
    }
}
