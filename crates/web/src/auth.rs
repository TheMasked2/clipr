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
use axum::http::{Extensions, HeaderMap, StatusCode, header};
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

/// Sessions and login attempts, in memory only.
#[derive(Default)]
pub struct Auth {
    sessions: Mutex<HashMap<String, Instant>>,
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

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(COOKIE)?.strip_prefix('='))
}

impl Auth {
    fn valid(&self, token: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        match sessions.get(token) {
            Some(made) if made.elapsed() < SESSION_LIFETIME => true,
            Some(_) => {
                sessions.remove(token);
                false
            }
            None => false,
        }
    }

    fn new_session(&self) -> String {
        let token = random_token();
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.retain(|_, made| made.elapsed() < SESSION_LIFETIME);
        if sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = sessions.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone()) {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(token.clone(), Instant::now());
        token
    }

    /// Has this address got its wrong guesses for the minute used up?
    fn locked_out(&self, ip: IpAddr) -> bool {
        let mut wrong = self.wrong.lock().unwrap_or_else(PoisonError::into_inner);
        let tries = wrong.entry(ip).or_default();
        tries.retain(|t| t.elapsed() < ATTEMPT_WINDOW);
        tries.len() >= MAX_WRONG_PINS
    }

    fn record_wrong(&self, ip: IpAddr) {
        let mut wrong = self.wrong.lock().unwrap_or_else(PoisonError::into_inner);
        wrong.retain(|_, tries| tries.iter().any(|t| t.elapsed() < ATTEMPT_WINDOW));
        wrong.entry(ip).or_default().push(Instant::now());
    }
}

/// The middleware: lets loopback through, and everyone else only with a valid session (or for UI files and
/// the login call).
pub async fn guard(State(auth): State<Arc<Auth>>, req: Request, next: Next) -> Response {
    let remote = peer_ip(req.extensions()).is_some_and(|ip| !ip.is_loopback());
    if remote {
        let path = req.uri().path();
        let protected =
            path.starts_with("/api/") || path.starts_with("/media/") || path.starts_with("/thumbs/");
        let open = path == "/api/login";
        let allowed = !protected || open || session_cookie(req.headers()).is_some_and(|t| auth.valid(t));
        if !allowed {
            return ApiError::new(StatusCode::UNAUTHORIZED, "enter the PIN to continue").into_response();
        }
    }
    next.run(req).await
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
    if auth.locked_out(ip) {
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
    let pin = state.control.config().web.lan_pin;
    if pin.is_empty() || !constant_time_eq(login.pin.trim(), &pin) {
        auth.record_wrong(ip);
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "wrong PIN"));
    }
    let token = auth.new_session();
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
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=b; clipr_session=abc; c=d".parse().unwrap());
        assert_eq!(session_cookie(&h), Some("abc"));
        assert_eq!(session_cookie(&HeaderMap::new()), None);
    }
}
