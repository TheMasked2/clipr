//! Posting to a Discord webhook: one message with an embed (title, game, length, thumbnail) and the clip.
//!
//! Webhook URLs carry their own secret, so they're never logged. Mentions are switched off for every post,
//! so a clip title can't ping `@everyone`.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// clipr's accent colour, for the embed's side bar.
const EMBED_COLOR: u32 = 0x8B5CF6;

/// How often a rate-limited post is retried.
const RATE_LIMIT_TRIES: u32 = 3;

/// Checks that `url` is a whole Discord webhook URL. clipr sends clips nowhere else.
pub fn check_url(url: &str) -> Result<()> {
    let rest = [
        "https://discord.com/api/webhooks/",
        "https://discordapp.com/api/webhooks/",
    ]
    .iter()
    .chain(&[
        "https://ptb.discord.com/api/webhooks/",
        "https://canary.discord.com/api/webhooks/",
    ])
    .find_map(|prefix| url.strip_prefix(prefix))
    .context("A webhook URL starts with https://discord.com/api/webhooks/")?;
    let mut parts = rest.split('/');
    let id = parts.next().unwrap_or_default();
    let token = parts.next().unwrap_or_default();
    let id_ok = !id.is_empty() && id.len() <= 25 && id.bytes().all(|b| b.is_ascii_digit());
    let token_ok = (20..=100).contains(&token.len())
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !id_ok || !token_ok || parts.any(|p| !p.is_empty()) {
        bail!(
            "That isn't a whole webhook URL; copy it again from Discord (Edit Channel → Integrations → Webhooks)"
        );
    }
    Ok(())
}

/// What goes into one post.
pub struct Post<'a> {
    pub title: &'a str,
    pub game: &'a str,
    pub duration_secs: Option<f64>,
    pub video: &'a Path,
    /// A JPEG for the embed's thumbnail.
    pub thumb: Option<&'a Path>,
}

/// `m:ss` (or `h:mm:ss`).
pub fn length(secs: f64) -> String {
    let s = secs.round().max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// A file name Discord shows well: the title in plain characters, or `clip`.
fn file_name(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_matches('_');
    let clean: String = clean.chars().take(60).collect();
    format!("{}.mp4", if clean.is_empty() { "clip" } else { &clean })
}

/// The JSON part of a post.
fn payload(post: &Post, video_name: &str) -> Value {
    let mut description = post.game.to_string();
    if let Some(d) = post.duration_secs {
        description.push_str(&format!(" · {}", length(d)));
    }
    let mut embed = json!({
        "title": post.title.chars().take(256).collect::<String>(),
        "description": description,
        "color": EMBED_COLOR,
        "footer": { "text": "clipr" },
    });
    let mut attachments = vec![json!({ "id": 0, "filename": video_name })];
    if post.thumb.is_some() {
        embed["thumbnail"] = json!({ "url": "attachment://thumb.jpg" });
        attachments.push(json!({ "id": 1, "filename": "thumb.jpg" }));
    }
    json!({
        "embeds": [embed],
        "attachments": attachments,
        "allowed_mentions": { "parse": [] },
    })
}

/// A `multipart/form-data` body: the JSON payload, then each file as `files[i]`.
fn multipart(boundary: &str, json: &Value, files: &[(String, &str, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\n\
             Content-Type: application/json\r\n\r\n{json}\r\n"
        )
        .as_bytes(),
    );
    for (i, (name, mime, data)) in files.iter().enumerate() {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"files[{i}]\"; filename=\"{name}\"\r\n\
                 Content-Type: {mime}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // A 500 MB upload on a slow line takes a while.
        .timeout_global(Some(Duration::from_secs(30 * 60)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .user_agent(format!("clipr/{}", clipr_core::VERSION))
        .build()
        .into()
}

/// Posts the clip. Retries a few times when Discord says to slow down.
pub fn post(url: &str, post: &Post) -> Result<()> {
    check_url(url)?;
    let video_name = file_name(post.title);
    let video = std::fs::read(post.video).with_context(|| format!("can't read {}", post.video.display()))?;
    let mut files = vec![(video_name.clone(), "video/mp4", video)];
    if let Some(thumb) = post.thumb
        && let Ok(data) = std::fs::read(thumb)
    {
        files.push(("thumb.jpg".into(), "image/jpeg", data));
    }
    let has_thumb = files.len() > 1;
    let body_json = payload(
        &Post {
            thumb: post.thumb.filter(|_| has_thumb),
            ..*post
        },
        &video_name,
    );
    let boundary = format!("clipr{:016x}", rand_u64());
    let body = multipart(&boundary, &body_json, &files);
    send(url, &format!("multipart/form-data; boundary={boundary}"), &body)
}

/// Sends a short text message, to check a webhook works.
pub fn test(url: &str) -> Result<()> {
    check_url(url)?;
    let body = json!({
        "content": "clipr is connected. Clips will show up here.",
        "allowed_mentions": { "parse": [] },
    });
    send(url, "application/json", body.to_string().as_bytes())
}

fn send(url: &str, content_type: &str, body: &[u8]) -> Result<()> {
    let agent = agent();
    let target = format!("{url}?wait=true");
    for attempt in 1..=RATE_LIMIT_TRIES {
        // The URL holds the webhook's secret: errors from here never include it.
        let mut response = agent
            .post(&target)
            .header("Content-Type", content_type)
            .send(body)
            .map_err(|e| anyhow::anyhow!("can't reach Discord: {}", redact(&e.to_string(), url)))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_string()
            .unwrap_or_default();
        match status {
            200..=299 => return Ok(()),
            429 if attempt < RATE_LIMIT_TRIES => {
                let wait = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v["retry_after"].as_f64())
                    .unwrap_or(2.0)
                    .clamp(0.5, 60.0);
                tracing::info!("Discord asks to wait {wait:.1} s before posting");
                std::thread::sleep(Duration::from_secs_f64(wait));
            }
            _ => bail!(explain(status, &text)),
        }
    }
    bail!("Discord kept asking clipr to slow down; try again in a minute")
}

/// What a failed post means, for the user.
fn explain(status: u16, body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_owned));
    match status {
        413 => "The clip is bigger than this server accepts. Lower the upload limit in Settings → Discord.".into(),
        401 | 403 | 404 => "Discord doesn't know this webhook any more (was it deleted?). Paste a new URL in Settings → Discord.".into(),
        _ => match message {
            Some(m) => format!("Discord refused the post ({status}): {m}"),
            None => format!("Discord refused the post ({status})"),
        },
    }
}

fn redact(text: &str, url: &str) -> String {
    text.replace(url, "<webhook>")
}

/// Enough randomness for a multipart boundary.
fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str =
        "https://discord.com/api/webhooks/123456789012345678/abcdefghijklmnopqrstuvwxyz_-0123456789";

    #[test]
    fn webhook_urls() {
        assert!(check_url(GOOD).is_ok());
        assert!(check_url(&GOOD.replace("discord.com", "discordapp.com")).is_ok());
        assert!(check_url(&format!("{GOOD}/")).is_ok());
        assert!(check_url("https://discord.com/api/webhooks/123/short").is_err());
        assert!(check_url(&GOOD.replace("https://", "http://")).is_err());
        assert!(check_url(&GOOD.replace("discord.com", "evil.example")).is_err());
        assert!(check_url(&format!("{GOOD}/messages/1")).is_err());
        assert!(check_url("https://discord.com/api/webhooks/abc/abcdefghijklmnopqrstuvwxyz").is_err());
    }

    #[test]
    fn payload_has_embed_no_mentions_and_attachments() {
        let p = Post {
            title: "@everyone ace",
            game: "VALORANT",
            duration_secs: Some(31.6),
            video: Path::new("x.mp4"),
            thumb: Some(Path::new("t.jpg")),
        };
        let v = payload(&p, "ace.mp4");
        assert_eq!(v["allowed_mentions"]["parse"], json!([]));
        assert_eq!(v["embeds"][0]["description"], "VALORANT · 0:32");
        assert_eq!(v["embeds"][0]["thumbnail"]["url"], "attachment://thumb.jpg");
        assert_eq!(v["attachments"][1]["filename"], "thumb.jpg");
        let no_thumb = payload(&Post { thumb: None, ..p }, "ace.mp4");
        assert!(no_thumb["embeds"][0].get("thumbnail").is_none());
    }

    #[test]
    fn names_and_lengths() {
        assert_eq!(file_name("Clutch 1v4 — ranked!"), "Clutch_1v4___ranked.mp4");
        assert_eq!(file_name("!!!"), "clip.mp4");
        assert_eq!(length(65.0), "1:05");
        assert_eq!(length(3725.0), "1:02:05");
    }

    #[test]
    fn multipart_layout() {
        let body = multipart(
            "B",
            &json!({"a": 1}),
            &[("c.mp4".into(), "video/mp4", b"DATA".to_vec())],
        );
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with("--B\r\nContent-Disposition: form-data; name=\"payload_json\""));
        assert!(
            text.contains("name=\"files[0]\"; filename=\"c.mp4\"\r\nContent-Type: video/mp4\r\n\r\nDATA\r\n")
        );
        assert!(text.ends_with("--B--\r\n"));
    }

    #[test]
    fn errors_never_show_the_url() {
        assert_eq!(
            redact(&format!("failed to connect to {GOOD}"), GOOD),
            "failed to connect to <webhook>"
        );
        assert!(explain(404, "").contains("doesn't know this webhook"));
        assert_eq!(
            explain(400, r#"{"message":"Invalid Form Body"}"#),
            "Discord refused the post (400): Invalid Form Body"
        );
    }
}
