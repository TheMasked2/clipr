//! `/api/cloud/*`: the upload list, each clip's cloud state, uploading and removing by hand, and trying
//! the bucket settings (see `docs/api.md`).

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Extensions, StatusCode};
use clipr_core::config::CloudConfig;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::api::{ApiError, ApiResult, blocking, parse_json};
use crate::auth;

/// The jobs, newest first, without the clips' local paths.
pub async fn uploads(State(s): State<AppState>) -> Json<Vec<Value>> {
    let jobs = s
        .cloud
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

/// Clip id → its cloud state (`uploaded` with its link, `queued`, `uploading` with progress, `failed`).
pub async fn status(State(s): State<AppState>) -> Json<Value> {
    Json(serde_json::to_value(s.cloud.statuses()).unwrap_or_default())
}

#[derive(Deserialize)]
struct IdReq {
    id: String,
}

/// Uploads a clip now (or retries it).
pub async fn upload(State(s): State<AppState>, body: Bytes) -> ApiResult<Json<Value>> {
    let req: IdReq = parse_json(&body)?;
    let st = s.clone();
    let info = blocking(move || {
        let clip = st.library.get(&req.id)?;
        crate::discord::clip_info(&st.library, &st.thumbs_dir, &clip)
    })
    .await?;
    let job = s
        .cloud
        .upload(info)
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct JobReq {
    job: u64,
}

pub async fn cancel(State(s): State<AppState>, body: Bytes) -> ApiResult<StatusCode> {
    let req: JobReq = parse_json(&body)?;
    s.cloud
        .cancel(req.job)
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes a clip's copy from the bucket (the file on this PC stays). Only from this PC: links to it stop
/// working for everyone.
pub async fn remove(State(s): State<AppState>, ext: Extensions, body: Bytes) -> ApiResult<StatusCode> {
    if auth::is_remote(&ext) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Clips can only be removed from the cloud on the PC running clipr",
        ));
    }
    let req: IdReq = parse_json(&body)?;
    let cloud = s.cloud.clone();
    match tokio::task::spawn_blocking(move || cloud.remove(&req.id)).await {
        Ok(Ok(())) => Ok(StatusCode::NO_CONTENT),
        Ok(Err(e)) => Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("{e:#}"))),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("internal error: {e}"),
        )),
    }
}

/// Tries bucket settings that may not be saved yet. A blank secret means the saved one. Only from this PC,
/// since the settings say where clips go.
pub async fn test(State(s): State<AppState>, ext: Extensions, body: Bytes) -> ApiResult<Json<Value>> {
    if auth::is_remote(&ext) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Cloud storage can only be set up on the PC running clipr",
        ));
    }
    let mut config: CloudConfig = parse_json(&body)?;
    if config.secret_access_key.trim().is_empty() {
        config.secret_access_key = s.control.config().cloud.secret_access_key;
    }
    clipr_cloud::testable(&config).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    match tokio::task::spawn_blocking(move || clipr_cloud::test(&config)).await {
        Ok(Ok(message)) => Ok(Json(json!({ "message": message }))),
        Ok(Err(e)) => Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("{e:#}"))),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("internal error: {e}"),
        )),
    }
}

/// The secret only reaches the PC's own UI.
pub(crate) fn hide_secret(config: &mut CloudConfig) {
    config.secret_access_key.clear();
}

/// From another device, the bucket can't be changed (it decides where every clip goes) and a blank secret
/// keeps the saved one. Turning uploads on or off, the timing and the speed limit are fine.
pub(crate) fn keep_bucket(ext: &Extensions, now: &CloudConfig, new: &mut CloudConfig) -> ApiResult<()> {
    if !auth::is_remote(ext) {
        return Ok(());
    }
    if new.secret_access_key.is_empty() {
        new.secret_access_key.clone_from(&now.secret_access_key);
    }
    let same = new.provider == now.provider
        && new.endpoint == now.endpoint
        && new.region == now.region
        && new.bucket == now.bucket
        && new.access_key_id == now.access_key_id
        && new.secret_access_key == now.secret_access_key
        && new.public_url == now.public_url
        && new.prefix == now.prefix;
    if !same {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "The cloud bucket can only be set up or changed on the PC running clipr",
        ));
    }
    Ok(())
}
