//! `/api/discord/*`: the post queue, sharing a clip by hand, and testing a webhook (see `docs/api.md`).

use anyhow::Result;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Extensions, StatusCode};
use clipr_core::config::{Config, DiscordConfig};
use clipr_discord::{ClipInfo, webhook};
use clipr_library::{Clip, Library};
use serde::Deserialize;

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
