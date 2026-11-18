//! `/api/discord/*`: the post queue, sharing a clip by hand, testing a webhook, and Connect Discord (see
//! `docs/api.md`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{Extensions, StatusCode};
use axum::response::IntoResponse;
use clipr_core::config::{Config, DiscordConfig};
use clipr_discord::{ClipInfo, account, webhook};
use clipr_library::{Clip, Library};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::api::{ApiError, ApiResult, blocking, parse_json};
use crate::auth;

/// What the queue needs to know about a library clip.
pub(crate) fn clip_info(library: &Library, thumbs_dir: &std::path::Path, clip: &Clip) -> Result<ClipInfo> {
    Ok(ClipInfo {
        id: clip.id.clone(),
        path: library.resolve(&clip.id)?,
        title: clip.title.clone(),
        game: clip.game.clone(),
        duration_secs: clip.duration_secs,
        size_bytes: clip.size_bytes,
        thumb: Some(thumbs_dir.join(clipr_library::thumb_file_name(&clip.id, false))),
        kind: clip.kind.clone(),
    })
}

/// The jobs, newest first, without the clips' local paths (the UI links clips by id).
pub async fn posts(State(s): State<AppState>) -> Json<Vec<serde_json::Value>> {
    let jobs = s
        .discord
        .jobs()
        .into_iter()
        .map(|j| {
            let mut v = serde_json::to_value(j).unwrap_or_default();
            if let Some(clip) = v.get_mut("clip").and_then(|c| c.as_object_mut()) {
                clip.remove("path");
                clip.remove("thumb");
            }
            v
        })
        .collect();
    Json(jobs)
}

#[derive(Deserialize)]
struct ShareReq {
    id: String,
    webhook: String,
}

pub async fn share(State(s): State<AppState>, body: Bytes) -> ApiResult<Json<serde_json::Value>> {
    let req: ShareReq = parse_json(&body)?;
    let st = s.clone();
    let info = blocking(move || {
        let clip = st.library.get(&req.id)?;
        clip_info(&st.library, &st.thumbs_dir, &clip)
    })
    .await?;
    let job = s
        .discord
        .share(info, &req.webhook)
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(serde_json::json!({ "job": job })))
}

#[derive(Deserialize)]
struct AnswerReq {
    job: u64,
    yes: bool,
}

pub async fn answer(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: AnswerReq = parse_json(&body)?;
    s.discord
        .answer(req.job, req.yes)
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct TestReq {
    /// A URL typed in but not saved yet.
    #[serde(default)]
    url: String,
    /// Or a saved webhook, by name (other devices don't get the URLs).
    #[serde(default)]
    name: String,
}

pub async fn test(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: TestReq = parse_json(&body)?;
    let url = if req.url.trim().is_empty() {
        s.control
            .config()
            .discord
            .webhooks
            .into_iter()
            .find(|w| w.name == req.name)
            .map(|w| w.url)
            .ok_or_else(|| ApiError::bad_request("Paste the webhook URL first"))?
    } else {
        req.url.trim().to_string()
    };
    webhook::check_url(&url).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    match tokio::task::spawn_blocking(move || webhook::test(&url)).await {
        Ok(Ok(())) => Ok(StatusCode::NO_CONTENT),
        // Discord's answer, for the user (it never holds the URL).
        Ok(Err(e)) => Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("{e:#}"))),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("internal error: {e}"),
        )),
    }
}

/// Webhook URLs are secrets: only the PC's own UI sees them. Other devices get them blanked.
pub(crate) fn hide_urls(config: &mut Config) {
    for w in &mut config.discord.webhooks {
        w.url.clear();
    }
}

/// From another device, a blank URL means "keep the saved one", and new URLs can't be set (so a phone
/// with the PIN can't send every clip somewhere new).
pub(crate) fn keep_urls(ext: &Extensions, now: &DiscordConfig, new: &mut DiscordConfig) -> ApiResult<()> {
    if !auth::is_remote(ext) {
        return Ok(());
    }
    for w in &mut new.webhooks {
        let saved = now
            .webhooks
            .iter()
            .find(|o| o.name == w.name)
            .map(|o| o.url.as_str());
        match saved {
            Some(url) if w.url.is_empty() || w.url == url => w.url = url.to_string(),
            // A new channel without a URL is harmless: the URL is added on the PC.
            None if w.url.is_empty() => {}
            _ => {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "Discord webhooks can only be added or changed on the PC running clipr",
                ));
            }
        }
    }
    Ok(())
}

/// The Discord settings' ranges.
pub(crate) fn check(d: &DiscordConfig) -> Result<()> {
    use anyhow::bail;
    if d.webhooks.len() > 10 {
        bail!("At most 10 Discord channels");
    }
    if !(1..=500).contains(&d.upload_limit_mb) {
        bail!("The Discord upload limit must be between 1 and 500 MB");
    }
    let mut names: Vec<&str> = Vec::new();
    for w in &d.webhooks {
        let name = w.name.trim();
        if name.is_empty() {
            bail!("Every Discord channel needs a name");
        }
        if name.chars().count() > 50 {
            bail!("A Discord channel's name can be at most 50 characters");
        }
        if names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            bail!("Two Discord channels are called {name:?}");
        }
        names.push(name);
        if !w.url.is_empty() {
            webhook::check_url(&w.url).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
        }
        if w.games.len() > 100 || w.games.iter().any(|g| g.chars().count() > 100) {
            bail!("{name}: too many games, or a game name that's too long");
        }
    }
    Ok(())
}

// ---- Connect Discord ------------------------------------------------------------------------------------

/// A sign-in on its way: started by the UI, finished by Discord's redirect, picked up by the UI.
struct Login {
    verifier: String,
    purpose: account::Purpose,
    started: Instant,
    /// Set by the redirect.
    result: Option<Result<serde_json::Value, String>>,
}

/// How long a sign-in may take before it's forgotten.
const LOGIN_TTL: Duration = Duration::from_secs(15 * 60);

static LOGINS: Mutex<Option<HashMap<String, Login>>> = Mutex::new(None);

fn logins() -> std::sync::MutexGuard<'static, Option<HashMap<String, Login>>> {
    LOGINS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn random_text(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    auth::random_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn only_here(ext: &Extensions) -> ApiResult<()> {
    if auth::is_remote(ext) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Connect Discord on the PC running clipr: Discord sends you back to it.",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct ConnectReq {
    /// Also pick a channel (Discord makes a webhook for it).
    #[serde(default)]
    channel: bool,
}

/// Starts a sign-in: the UI opens `url` (Discord's page) and then asks `/api/discord/connect/{state}`.
pub async fn connect(
    State(s): State<AppState>,
    ext: Extensions,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    only_here(&ext)?;
    let req: ConnectReq = parse_json(&body)?;
    if s.lan.port() != account::REDIRECT_PORT {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "Connect Discord needs clipr on port {} (Discord only sends you back there), but it runs on {}. \
                 Close what uses port {} and restart clipr.",
                account::REDIRECT_PORT,
                s.lan.port(),
                account::REDIRECT_PORT
            ),
        ));
    }
    let purpose = if req.channel {
        account::Purpose::Channel
    } else {
        account::Purpose::Account
    };
    // 64 hex characters: within PKCE's 43-128 and all unreserved.
    let verifier = random_text(32);
    let state = random_text(16);
    let url = account::authorize_url(purpose, &state, &verifier);
    let mut all = logins();
    let map = all.get_or_insert_with(HashMap::new);
    map.retain(|_, l| l.started.elapsed() < LOGIN_TTL);
    if map.len() >= 20 {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sign-ins at once; try again in a while.",
        ));
    }
    map.insert(
        state.clone(),
        Login {
            verifier,
            purpose,
            started: Instant::now(),
            result: None,
        },
    );
    Ok(Json(json!({ "url": url, "state": state })))
}

/// 202 while the user is still on Discord's page; then the account (and webhook) once, or the error.
pub async fn connect_result(
    ext: Extensions,
    Path(state): Path<String>,
) -> ApiResult<axum::response::Response> {
    only_here(&ext)?;
    let mut all = logins();
    let map = all.get_or_insert_with(HashMap::new);
    let Some(login) = map.get(&state) else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "This sign-in expired. Start again.",
        ));
    };
    if login.result.is_none() {
        if login.started.elapsed() >= LOGIN_TTL {
            map.remove(&state);
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "This sign-in expired. Start again.",
            ));
        }
        return Ok((StatusCode::ACCEPTED, Json(json!({ "pending": true }))).into_response());
    }
    match map.remove(&state).and_then(|l| l.result).expect("checked above") {
        Ok(v) => Ok(Json(v).into_response()),
        Err(e) => Err(ApiError::new(StatusCode::BAD_GATEWAY, e)),
    }
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Where Discord sends the browser back to. Finishes the sign-in and shows a short page to close.
pub async fn callback(ext: Extensions, Query(q): Query<CallbackQuery>) -> axum::response::Response {
    if auth::is_remote(&ext) {
        return page(
            StatusCode::FORBIDDEN,
            "Not here",
            "Discord sign-ins finish on the PC running clipr.",
        );
    }
    let state = q.state.unwrap_or_default();
    let (verifier, purpose) = {
        let mut all = logins();
        let map = all.get_or_insert_with(HashMap::new);
        match map.get(&state) {
            Some(l) if l.result.is_none() && l.started.elapsed() < LOGIN_TTL => {
                (l.verifier.clone(), l.purpose)
            }
            _ => {
                return page(
                    StatusCode::BAD_REQUEST,
                    "This sign-in expired",
                    "Start it again from clipr's Discord settings.",
                );
            }
        }
    };
    let result = match (q.code, q.error) {
        (_, Some(e)) => Err(match e.as_str() {
            "access_denied" => "You cancelled the sign-in on Discord.".to_string(),
            _ => format!("Discord: {}", q.error_description.unwrap_or(e)),
        }),
        (Some(code), None) => tokio::task::spawn_blocking(move || account::finish(&code, &verifier))
            .await
            .map_err(|_| "the sign-in failed".to_string())
            .and_then(|r| r.map_err(|e| format!("{e:#}")))
            .and_then(|c| {
                if purpose == account::Purpose::Channel && c.webhook.is_none() {
                    Err("Discord didn't make a webhook for a channel. Pick a server and channel and try again.".into())
                } else {
                    Ok(connected_json(&c))
                }
            }),
        (None, None) => Err("Discord sent no sign-in code.".to_string()),
    };
    let ok = result.is_ok();
    if let Some(l) = logins().get_or_insert_with(HashMap::new).get_mut(&state) {
        l.result = Some(result.clone());
    }
    if ok {
        page(
            StatusCode::OK,
            "Connected",
            "Back to clipr: you can close this tab.",
        )
    } else {
        page(StatusCode::OK, "Not connected", &result.err().unwrap_or_default())
    }
}

fn connected_json(c: &account::Connected) -> serde_json::Value {
    json!({
        "account": {
            "id": c.user.id,
            "username": c.user.username,
            "name": c.user.global_name,
            "avatar": c.user.avatar,
        },
        "webhook": c.webhook.as_ref().map(|w| json!({
            "url": w.url,
            "channel_id": w.channel_id,
            "guild_id": w.guild_id,
        })),
    })
}

/// A small standalone page for the browser tab Discord sent back (it tries to close itself when done).
fn page(status: StatusCode, title: &str, text: &str) -> axum::response::Response {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let close = if status == StatusCode::OK && title == "Connected" {
        "<script>setTimeout(() => window.close(), 1200)</script>"
    } else {
        ""
    };
    let html = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>clipr</title>\
         <body style=\"background:#0b0b0f;color:#e5e5ea;font:16px system-ui;padding:2rem\">\
         <h1>{}</h1><p>{}</p>{close}",
        esc(title),
        esc(text)
    );
    (
        status,
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response()
}
