//! Rich Presence: "Playing clipr" in the user's Discord profile, with the game clipr clips and its icon.
//!
//! It talks to the Discord app on this PC over its local IPC pipe (`\\.\pipe\discord-ipc-N`), as the
//! official clipr application ([`APP_ID`]). Nothing goes over the network and no login is needed; when Discord
//! isn't running, the presence thread quietly tries again now and then. Dropping the connection clears the
//! presence, so that is how it's cleared.

use std::io::{Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// The official clipr application on Discord (its name and art show in the profile). Public, not a secret.
pub const APP_ID: &str = "1572404034802032700";

/// What clipr is doing, as far as the profile shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// The game's name as Discord's list has it (never a window title), `None` on the desktop.
    pub game: Option<String>,
    /// The game's icon (a Discord CDN URL), when Discord knows the game.
    pub icon: Option<String>,
    /// A full recording runs, since these Unix seconds.
    pub recording_since: Option<u64>,
}

/// Shows clipr in the Discord profile. Cheap: [`set`](Self::set) only hands the status to a thread.
pub struct Presence {
    tx: Sender<Option<Status>>,
}

/// How long to wait before trying Discord again when it isn't running or didn't answer.
const RETRY: Duration = Duration::from_secs(20);

/// How often a shown presence is sent again, so a restarted Discord gets it back.
const REFRESH: Duration = Duration::from_secs(60);

/// The least time between two updates (Discord takes about 5 per 20 s).
const MIN_GAP: Duration = Duration::from_secs(4);

/// How long Discord gets to answer before the connection counts as dead.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Presence {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<Option<Status>>();
        let spawned = std::thread::Builder::new()
            .name("discord-presence".into())
            .spawn(move || run(&rx));
        if let Err(e) = spawned {
            tracing::warn!("can't start Rich Presence: {e}");
        }
        Self { tx }
    }

    /// What to show (`None`: nothing, e.g. Rich Presence off or clipr idle).
    pub fn set(&self, status: Option<Status>) {
        let _ = self.tx.send(status);
    }
}

fn run(rx: &mpsc::Receiver<Option<Status>>) {
    let mut want: Option<Status> = None;
    let mut shown: Option<Status> = None;
    let mut conn: Option<Ipc> = None;
    // When this session with the game began (the elapsed timer while not recording). Reset when the
    // presence is cleared, so a game started again later counts from then.
    let mut session: Option<(Option<String>, u64)> = None;
    let start_at = Instant::now();
    let mut retry_at = start_at;
    let mut hold_until = start_at;
    let mut sent_at = start_at;
    loop {
        let now = Instant::now();
        let due = match &want {
            None => None,
            Some(_) if want != shown || conn.is_none() => Some(retry_at.max(hold_until)),
            // Shown: check now and then that Discord still has it (it may have restarted).
            Some(_) => Some(sent_at + REFRESH),
        };
        let wait = due.map_or(Duration::from_secs(3600), |t| t.saturating_duration_since(now));
        match rx.recv_timeout(wait) {
            Ok(next) => {
                want = next;
                // Take every queued update; only the newest counts.
                while let Ok(next) = rx.try_recv() {
                    want = next;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let Some(status) = want.clone() else {
            if conn.take().is_some() {
                tracing::debug!("Rich Presence cleared");
            }
            shown = None;
            session = None;
            retry_at = Instant::now();
            continue;
        };
        let now = Instant::now();
        let refresh_due = now >= sent_at + REFRESH;
        if Some(&status) == shown.as_ref() && conn.is_some() && !refresh_due {
            continue;
        }
        if now < retry_at || now < hold_until {
            continue;
        }
        let game_start = match &session {
            Some((game, t)) if *game == status.game => *t,
            _ => now_secs(),
        };
        session = Some((status.game.clone(), game_start));
        let start = status.recording_since.unwrap_or(game_start);
        let result = (|| -> Result<bool> {
            if conn.is_none() {
                conn = Some(Ipc::connect()?);
                tracing::info!("Rich Presence: connected to Discord");
            }
            conn.as_mut()
                .expect("just set")
                .set_activity(activity(&status, start))
        })();
        match result {
            Ok(taken) => {
                if !taken && shown.as_ref() != Some(&status) {
                    tracing::warn!(
                        "Rich Presence: Discord didn't take the status for {:?}",
                        status.game
                    );
                }
                // Shown or refused, it isn't sent again until something changes (or the next refresh).
                shown = Some(status);
                sent_at = now;
                hold_until = now + MIN_GAP;
            }
            Err(e) => {
                tracing::debug!("Rich Presence: {e:#}");
                conn = None;
                shown = None;
                retry_at = now + RETRY;
            }
        }
    }
}

/// `text` as Discord accepts it: 2 to 128 characters.
fn fit(text: &str) -> String {
    let text = text.trim();
    match text.chars().count() {
        0 => "clipr".into(),
        1 => format!("Game {text}"),
        n if n > 128 => text.chars().take(127).chain(['…']).collect(),
        _ => text.to_string(),
    }
}

/// The activity Discord shows: the game on the first line, what clipr does on the second, the elapsed time,
/// and the game's icon with clipr's logo on top.
pub fn activity(status: &Status, start: u64) -> Value {
    let doing = if status.recording_since.is_some() {
        "Recording with clipr"
    } else {
        "Clipping with clipr"
    };
    let details = fit(status.game.as_deref().unwrap_or("Desktop"));
    let assets = match &status.icon {
        Some(icon) => json!({
            "large_image": icon,
            "large_text": details,
            "small_image": "clipr",
            "small_text": "clipr",
        }),
        None => json!({ "large_image": "clipr", "large_text": "clipr" }),
    };
    json!({
        "details": details,
        "state": doing,
        "timestamps": { "start": start },
        "assets": assets,
    })
}

// ---- the IPC pipe -------------------------------------------------------------------------------------

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

struct Ipc {
    pipe: std::fs::File,
    nonce: u64,
}

impl Ipc {
    /// Opens the first Discord pipe that answers (Discord, PTB and Canary each take the next number; a stale
    /// or busy one is skipped).
    fn connect() -> Result<Self> {
        let mut last: Option<anyhow::Error> = None;
        for i in 0..10 {
            let path = format!(r"\\.\pipe\discord-ipc-{i}");
            let Ok(pipe) = std::fs::OpenOptions::new().read(true).write(true).open(&path) else {
                continue;
            };
            let mut ipc = Self { pipe, nonce: 0 };
            match ipc.handshake() {
                Ok(()) => return Ok(ipc),
                Err(e) => last = Some(e.context(path)),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("Discord isn't running")))
    }

    fn handshake(&mut self) -> Result<()> {
        self.send(OP_HANDSHAKE, &json!({ "v": 1, "client_id": APP_ID }))?;
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        loop {
            let (op, reply) = self.receive(deadline)?;
            if op == OP_CLOSE {
                bail!(
                    "Discord refused the connection: {}",
                    reply["message"].as_str().unwrap_or("no reason")
                );
            }
            if reply["evt"] == "READY" {
                return Ok(());
            }
        }
    }

    /// Sends the activity. `Ok(false)`: Discord answered but didn't take it (the connection stays fine).
    fn set_activity(&mut self, activity: Value) -> Result<bool> {
        self.nonce += 1;
        let nonce = self.nonce.to_string();
        let cmd = json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": std::process::id(), "activity": activity },
            "nonce": nonce,
        });
        self.send(OP_FRAME, &cmd)?;
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        loop {
            let (op, reply) = self.receive(deadline)?;
            if op == OP_CLOSE {
                bail!("Discord closed the connection");
            }
            // Other frames (events) can come in between; the answer carries our nonce.
            if reply["nonce"] != nonce.as_str() {
                continue;
            }
            if reply["evt"] == "ERROR" {
                tracing::debug!(
                    "Discord didn't take the activity: {}",
                    reply["data"]["message"].as_str().unwrap_or("no reason")
                );
                return Ok(false);
            }
            return Ok(true);
        }
    }

    fn send(&mut self, op: u32, payload: &Value) -> Result<()> {
        self.pipe
            .write_all(&frame(op, payload))
            .context("can't write to Discord")
    }

    /// The next frame that isn't a ping (pings are answered here). Fails at `deadline`, so a Discord that
    /// opened its pipe but doesn't answer can't hang the thread.
    fn receive(&mut self, deadline: Instant) -> Result<(u32, Value)> {
        loop {
            let mut header = [0u8; 8];
            self.read_by(&mut header, deadline)?;
            let (op, len) = parse_header(header);
            if len > 1 << 20 {
                bail!("Discord sent an oversized frame ({len} bytes)");
            }
            let mut body = vec![0u8; len as usize];
            self.read_by(&mut body, deadline)?;
            let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            if op == OP_PING {
                self.send(OP_PONG, &value)?;
                continue;
            }
            return Ok((op, value));
        }
    }

    /// Fills `buf`, reading only what the pipe already holds so a read never blocks past `deadline`.
    fn read_by(&mut self, buf: &mut [u8], deadline: Instant) -> Result<()> {
        let mut filled = 0;
        while filled < buf.len() {
            let available = available(&self.pipe)?;
            if available == 0 {
                if Instant::now() >= deadline {
                    bail!("Discord didn't answer");
                }
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            let take = available.min(buf.len() - filled);
            self.pipe
                .read_exact(&mut buf[filled..filled + take])
                .context("can't read from Discord")?;
            filled += take;
        }
        Ok(())
    }
}

/// How many bytes the pipe holds right now. Fails when the pipe broke (Discord quit).
#[cfg(windows)]
fn available(pipe: &std::fs::File) -> Result<usize> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    let mut avail = 0u32;
    // SAFETY: a valid pipe handle; with no buffer only the counts are written.
    let ok = unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut avail,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("the connection to Discord broke");
    }
    Ok(avail as usize)
}

#[cfg(not(windows))]
fn available(_pipe: &std::fs::File) -> Result<usize> {
    bail!("Rich Presence needs Windows")
}

/// One IPC frame: opcode and length (both little-endian `u32`), then the JSON.
fn frame(op: u32, payload: &Value) -> Vec<u8> {
    let body = payload.to_string().into_bytes();
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn parse_header(h: [u8; 8]) -> (u32, u32) {
    (
        u32::from_le_bytes([h[0], h[1], h[2], h[3]]),
        u32::from_le_bytes([h[4], h[5], h[6], h[7]]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let f = frame(OP_FRAME, &json!({"a": 1}));
        let (op, len) = parse_header(f[..8].try_into().unwrap());
        assert_eq!((op, len as usize), (OP_FRAME, f.len() - 8));
        assert_eq!(&f[8..], br#"{"a":1}"#);
    }

    #[test]
    fn the_activity_shows_the_game_and_what_clipr_does() {
        let game = Status {
            game: Some("VALORANT".into()),
            icon: Some("https://cdn.discordapp.com/app-icons/1/2.png".into()),
            recording_since: None,
        };
        let a = activity(&game, 100);
        assert_eq!(a["details"], "VALORANT");
        assert_eq!(a["state"], "Clipping with clipr");
        assert_eq!(a["timestamps"]["start"], 100);
        assert_eq!(a["assets"]["large_image"], game.icon.as_deref().unwrap());
        assert_eq!(a["assets"]["small_image"], "clipr");

        let rec = Status {
            game: None,
            icon: None,
            recording_since: Some(5),
        };
        let a = activity(&rec, 5);
        assert_eq!(a["details"], "Desktop");
        assert_eq!(a["state"], "Recording with clipr");
        assert_eq!(a["assets"]["large_image"], "clipr");
    }

    #[test]
    fn texts_fit_discords_limits() {
        assert_eq!(fit("  VALORANT "), "VALORANT");
        assert_eq!(fit("X"), "Game X");
        assert_eq!(fit(""), "clipr");
        let long = "é".repeat(200);
        let f = fit(&long);
        assert_eq!(f.chars().count(), 128);
        assert!(f.ends_with('…'));
    }
}
