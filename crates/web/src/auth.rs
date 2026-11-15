//! LAN access: requests from any address but loopback need the PIN (`POST /api/login`), after which a
//! `clipr_session` cookie lets them through. Loopback never needs it.
//!
//! The UI's own files and `/api/login` stay open so the PIN screen can load.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Extensions, HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::AppState;
use crate::api::ApiError;

const COOKIE: &str = "clipr_session";
const SESSION_LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);
const MAX_SESSIONS: usize = 1000;
const ATTEMPT_WINDOW: Duration = Duration::from_secs(60);
const MAX_WRONG_PINS: usize = 5;

/// A login: when it was made and the PIN it was made with.
struct Session {
    made: Instant,
    pin: String,
}

/// What a login attempt came to.
enum Attempt {
    LockedOut,
    Wrong,
    Ok,
}

/// Sessions and login attempts, in memory only.
#[derive(Default)]
pub struct Auth {
    sessions: Mutex<HashMap<String, Session>>,
    wrong: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

/// Fills `buf` with random bytes from the OS.
pub fn random_bytes(buf: &mut [u8]) {
    use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    // SAFETY: `buf` is a valid writable slice for the whole call; no algorithm handle is needed with the
    // system-preferred RNG flag.
    let status = unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    assert!(
        status.is_ok(),
        "the OS random number generator failed: {status:?}"
    );
}

/// A random 6-digit PIN.
pub fn generate_pin() -> String {
    let mut b = [0u8; 4];
    random_bytes(&mut b);
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
}

fn random_token() -> String {
    let mut b = [0u8; 32];
    random_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Compares without stopping at the first difference.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

/// The peer's address, if the server recorded it (tests that skip the socket have none).
fn peer_ip(ext: &Extensions) -> Option<IpAddr> {
    ext.get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_canonical())
}

/// Is the request from another machine? (No recorded peer counts as local, like in tests.)
pub fn is_remote(ext: &Extensions) -> bool {
    peer_ip(ext).is_some_and(|ip| !ip.is_loopback())
}

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(COOKIE)?.strip_prefix('='))
}

impl Auth {
    /// A session counts only while LAN access is on and the PIN is the one it was made with.
    fn valid(&self, token: &str, lan: bool, pin: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        match sessions.get(token) {
            Some(s) if s.made.elapsed() < SESSION_LIFETIME => {
                if lan && !pin.is_empty() && s.pin == pin {
                    true
                } else {
                    sessions.remove(token);
                    false
                }
            }
            Some(_) => {
                sessions.remove(token);
                false
            }
            None => false,
        }
    }

    fn new_session(&self, pin: &str) -> String {
        let token = random_token();
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.retain(|_, s| s.made.elapsed() < SESSION_LIFETIME);
        if sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, s)| s.made)
                .map(|(k, _)| k.clone())
            {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            token.clone(),
            Session {
                made: Instant::now(),
                pin: pin.to_string(),
            },
        );
        token
    }

    /// Checks the lockout, runs `correct` and records a miss, all under one lock so parallel guesses can't
    /// all slip past the limit.
    fn attempt(&self, ip: IpAddr, correct: impl FnOnce() -> bool) -> Attempt {
        let mut wrong = self.wrong.lock().unwrap_or_else(PoisonError::into_inner);
        wrong.retain(|_, tries| tries.iter().any(|t| t.elapsed() < ATTEMPT_WINDOW));
        let tries = wrong.entry(ip).or_default();
        tries.retain(|t| t.elapsed() < ATTEMPT_WINDOW);
        if tries.len() >= MAX_WRONG_PINS {
            return Attempt::LockedOut;
        }
        if correct() {
            return Attempt::Ok;
        }
        tries.push(Instant::now());
        Attempt::Wrong
    }
}

/// The middleware: lets loopback through, and everyone else only with a valid session (or for UI files and
/// the login call).
pub async fn guard(
    State(state): State<AppState>,
    axum::Extension(auth): axum::Extension<Arc<Auth>>,
    req: Request,
    next: Next,
) -> Response {
    if is_remote(req.extensions()) {
        let path = req.uri().path();
        let protected =
            path.starts_with("/api/") || path.starts_with("/media/") || path.starts_with("/thumbs/");
        let open = path == "/api/login";
        let allowed = !protected || open || {
            let web = state.control.config().web;
            session_cookie(req.headers()).is_some_and(|t| auth.valid(t, web.lan, &web.lan_pin))
        };
        if !allowed {
            return ApiError::new(StatusCode::UNAUTHORIZED, "enter the PIN to continue").into_response();
        }
    }
    next.run(req).await
}

/// The host name part of a `Host` header value (without the port), lower-cased.
fn host_name(host: &str) -> String {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or(rest, |(ip, _)| ip),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    name.to_ascii_lowercase()
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host_name(host).as_str(), "127.0.0.1" | "localhost" | "::1")
}

/// Is `value` a JSON content type (`application/json`, maybe with `; charset=...`)?
fn is_json(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
}

fn forbidden(message: &str) -> Response {
    ApiError::new(StatusCode::FORBIDDEN, message).into_response()
}

/// The outermost middleware, against web pages on other sites driving the API:
/// - a request from this machine must name a loopback host (DNS rebinding makes a hostile name resolve to
///   127.0.0.1);
/// - a request that changes things, and the WebSocket upgrade, must have an `Origin` (when the browser sends
///   one, and it always does cross-site) that is this server's own address;
/// - a body must be JSON, which a page on another site can't send without a CORS preflight.
///
/// It also hides error details from other machines.
pub async fn origin_guard(req: Request, next: Next) -> Response {
    let remote = is_remote(req.extensions());
    let headers = req.headers();
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if !remote && host.is_some_and(|h| !is_loopback_host(h)) {
        return forbidden("this address isn't allowed (use 127.0.0.1 or localhost)");
    }
    let writes = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let upgrade = headers.contains_key(header::UPGRADE);
    if writes || upgrade {
        if let Some(origin) = headers.get(header::ORIGIN) {
            let same = match (origin.to_str().ok(), host) {
                (Some(o), Some(h)) => o.eq_ignore_ascii_case(&format!("http://{h}")),
                _ => false,
            };
            if !same {
                return forbidden("requests from other sites aren't allowed");
            }
        }
    }
    if writes {
        let body_expected = headers.get(header::CONTENT_LENGTH).is_some_and(|v| v != "0")
            || headers.contains_key(header::TRANSFER_ENCODING);
        let bad = match headers.get(header::CONTENT_TYPE) {
            Some(ct) => !ct.to_str().is_ok_and(is_json),
            None => body_expected,
        };
        if bad {
            return ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "send the body as application/json",
            )
            .into_response();
        }
    }
    let response = next.run(req).await;
    if remote && response.status() == StatusCode::INTERNAL_SERVER_ERROR {
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "something went wrong (details are in clipr's log)",
        )
        .into_response();
    }
    response
}

#[derive(Deserialize)]
struct Login {
    pin: String,
}

pub async fn login(
    State(state): State<AppState>,
    axum::Extension(auth): axum::Extension<Arc<Auth>>,
    ext: Extensions,
    body: Bytes,
) -> Result<Response, ApiError> {
    let login: Login = crate::api::parse_json(&body)?;
    let ip = peer_ip(&ext).unwrap_or(IpAddr::from([127, 0, 0, 1]));
    let web = state.control.config().web;
    let attempt = auth.attempt(ip, || {
        web.lan && !web.lan_pin.is_empty() && constant_time_eq(login.pin.trim(), &web.lan_pin)
    });
    match attempt {
        Attempt::LockedOut => {
            let mut response = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many wrong PINs, wait a minute",
            )
            .into_response();
            response.headers_mut().insert(
                header::RETRY_AFTER,
                ATTEMPT_WINDOW.as_secs().to_string().parse().expect("a number"),
            );
            return Ok(response);
        }
        Attempt::Wrong => return Err(ApiError::new(StatusCode::UNAUTHORIZED, "wrong PIN")),
        Attempt::Ok => {}
    }
    let token = auth.new_session(&web.lan_pin);
    let cookie = format!(
        "{COOKIE}={token}; Max-Age={}; Path=/; HttpOnly; SameSite=Strict",
        SESSION_LIFETIME.as_secs()
    );
    Ok(([(header::SET_COOKIE, cookie)], Json(serde_json::json!({}))).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_is_six_digits_and_compare_works() {
        for _ in 0..20 {
            let pin = generate_pin();
            assert_eq!(pin.len(), 6);
            assert!(pin.bytes().all(|b| b.is_ascii_digit()));
        }
        assert!(constant_time_eq("123456", "123456"));
        assert!(!constant_time_eq("123456", "123457"));
        assert!(!constant_time_eq("123456", "12345"));
        assert_ne!(random_token(), random_token());
    }

    #[test]
    fn hosts_and_content_types() {
        for ok in [
            "127.0.0.1:7845",
            "localhost",
            "LOCALHOST:80",
            "[::1]:7845",
            "[::1]",
        ] {
            assert!(is_loopback_host(ok), "{ok}");
        }
        for bad in [
            "evil.com",
            "127.0.0.1.evil.com:80",
            "localhost.evil.com",
            "[::2]:1",
            "",
            "10.0.0.1:7845",
        ] {
            assert!(!is_loopback_host(bad), "{bad}");
        }
        assert!(is_json("application/json"));
        assert!(is_json("Application/JSON; charset=utf-8"));
        assert!(!is_json("text/plain"));
        assert!(!is_json("application/jsonp"));
    }

    #[test]
    fn lockout_is_decided_under_one_lock() {
        let auth = Auth::default();
        let ip = IpAddr::from([10, 0, 0, 1]);
        for _ in 0..MAX_WRONG_PINS {
            assert!(matches!(auth.attempt(ip, || false), Attempt::Wrong));
        }
        // Locked: the check isn't even run, so a right guess can't get through.
        assert!(matches!(
            auth.attempt(ip, || panic!("compared while locked")),
            Attempt::LockedOut
        ));
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=b; clipr_session=abc; c=d".parse().unwrap());
        assert_eq!(session_cookie(&h), Some("abc"));
        assert_eq!(session_cookie(&HeaderMap::new()), None);
    }
}
