//! Connect Discord: the user signs in on Discord's own page as the official clipr app ([`crate::presence::APP_ID`]),
//! which is a *public client*: the sign-in uses PKCE instead of a client secret, so there is no secret in
//! clipr and no server of ours. Discord sends the browser back to clipr on this PC, which trades the code for
//! a token, reads the user's name and avatar, and (with [`Purpose::Channel`]) gets a webhook for the channel the
//! user picked in Discord's own picker. The token isn't kept: it lasts an hour and nothing else needs it.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::presence::APP_ID;

const AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";
const TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
const ME_URL: &str = "https://discord.com/api/v10/users/@me";

/// The port the clipr app's redirect is registered for: Discord only sends the browser back to exactly
/// `http://127.0.0.1:7845/discord/callback`.
pub const REDIRECT_PORT: u16 = 7845;
pub const REDIRECT_URI: &str = "http://127.0.0.1:7845/discord/callback";

/// Why the user signs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Show who is connected.
    Account,
    /// Also pick a channel: Discord makes a webhook for it.
    Channel,
}

impl Purpose {
    fn scope(self) -> &'static str {
        match self {
            Purpose::Account => "identify",
            Purpose::Channel => "identify webhook.incoming",
        }
    }
}

/// The signed-in user, as clipr shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: String,
    /// The unique handle (`name`, no `#0000` any more).
    pub username: String,
    /// The display name, when set.
    pub global_name: Option<String>,
    /// The avatar on Discord's CDN, when set.
    pub avatar: Option<String>,
}

/// The webhook Discord made for the picked channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewWebhook {
    pub url: String,
    pub channel_id: String,
    pub guild_id: Option<String>,
}

/// What a finished sign-in brings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connected {
    pub user: User,
    pub webhook: Option<NewWebhook>,
}

/// The PKCE challenge for `verifier`: base64url (no padding) of its SHA-256.
pub fn challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// Discord's sign-in page for `purpose`. `state` comes back with the redirect and must match.
pub fn authorize_url(purpose: Purpose, state: &str, verifier: &str) -> String {
    let q = [
        ("response_type", "code"),
        ("client_id", APP_ID),
        ("scope", purpose.scope()),
        ("state", state),
        ("redirect_uri", REDIRECT_URI),
        ("code_challenge", &challenge(verifier)),
        ("code_challenge_method", "S256"),
        ("prompt", "consent"),
    ];
    let query: Vec<String> = q.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect();
    format!("{AUTHORIZE_URL}?{}", query.join("&"))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    webhook: Option<WebhookObject>,
}

#[derive(Deserialize)]
struct WebhookObject {
    url: String,
    #[serde(default)]
    channel_id: String,
    #[serde(default)]
    guild_id: Option<String>,
}

#[derive(Deserialize)]
struct MeResponse {
    id: String,
    username: String,
    #[serde(default)]
    global_name: Option<String>,
    #[serde(default)]
    avatar: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(format!("clipr/{}", clipr_core::VERSION))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Trades the code from the redirect for a token (PKCE, no secret), then reads who signed in.
pub fn finish(code: &str, verifier: &str) -> Result<Connected> {
    let agent = agent();
    let mut response = agent
        .post(TOKEN_URL)
        .send_form([
            ("grant_type", "authorization_code"),
            ("client_id", APP_ID),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", verifier),
        ])
        .context("can't reach Discord")?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .context("can't read Discord's answer")?;
    if status != 200 {
        bail!(
            "Discord didn't accept the sign-in ({status}): {}",
            oauth_error(&body)
        );
    }
    let token: TokenResponse =
        serde_json::from_str(&body).context("Discord's answer has an unexpected format")?;

    let mut response = agent
        .get(ME_URL)
        .header("Authorization", &format!("Bearer {}", token.access_token))
        .call()
        .context("can't reach Discord")?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .context("can't read Discord's answer")?;
    if status != 200 {
        bail!("Discord didn't say who signed in ({status})");
    }
    let me: MeResponse = serde_json::from_str(&body).context("Discord's answer has an unexpected format")?;
    Ok(Connected {
        user: user(me),
        webhook: token.webhook.map(|w| NewWebhook {
            url: w.url,
            channel_id: w.channel_id,
            guild_id: w.guild_id,
        }),
    })
}

fn user(me: MeResponse) -> User {
    let avatar = me
        .avatar
        .filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .filter(|_| me.id.bytes().all(|b| b.is_ascii_digit()))
        .map(|h| format!("https://cdn.discordapp.com/avatars/{}/{h}.png?size=128", me.id));
    User {
        id: me.id,
        username: me.username,
        global_name: me.global_name.filter(|n| !n.trim().is_empty()),
        avatar,
    }
}

/// The `error_description` (or `error`) of an OAuth error body, for the user.
fn oauth_error(body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    v["error_description"]
        .as_str()
        .or(v["error"].as_str())
        .unwrap_or("no reason given")
        .to_string()
}

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Percent-encodes a query value (RFC 3986 unreserved characters stay).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_the_rfc_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
    }

    #[test]
    fn the_sign_in_page_asks_for_the_right_things() {
        let url = authorize_url(Purpose::Channel, "st4te", "v");
        assert!(url.starts_with(
            "https://discord.com/oauth2/authorize?response_type=code&client_id=1572404034802032700&"
        ));
        assert!(url.contains("scope=identify%20webhook.incoming"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A7845%2Fdiscord%2Fcallback"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st4te"));
        assert!(authorize_url(Purpose::Account, "s", "v").contains("scope=identify&"));
    }

    #[test]
    fn users_and_errors_read_safely() {
        let u = user(MeResponse {
            id: "80351110224678912".into(),
            username: "nelly".into(),
            global_name: Some(" ".into()),
            avatar: Some("8342729096ea3675442027381ff50dfe".into()),
        });
        assert_eq!(u.global_name, None);
        assert_eq!(
            u.avatar.as_deref(),
            Some(
                "https://cdn.discordapp.com/avatars/80351110224678912/8342729096ea3675442027381ff50dfe.png?size=128"
            )
        );
        let bad = user(MeResponse {
            id: "1".into(),
            username: "x".into(),
            global_name: None,
            avatar: Some("../evil".into()),
        });
        assert_eq!(bad.avatar, None);
        assert_eq!(
            oauth_error(r#"{"error":"invalid_grant","error_description":"Invalid code"}"#),
            "Invalid code"
        );
        assert_eq!(oauth_error("nope"), "no reason given");
    }
}
