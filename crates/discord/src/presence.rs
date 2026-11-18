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
    /// The game's name, `None` on the desktop.
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

/// How long to wait before trying Discord again when it isn't running or refused.
const RETRY: Duration = Duration::from_secs(20);

/// How often a shown presence is sent again, so a restarted Discord gets it back.
const REFRESH: Duration = Duration::from_secs(60);

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
    // The start of the elapsed timer: when the game (or recording) began, kept across updates.
    let mut since: Option<(Option<String>, u64)> = None;
    let mut retry_at: Option<Instant> = None;
    loop {
        let wait = match (&want, retry_at) {
            (Some(_), Some(t)) => t.saturating_duration_since(Instant::now()),
            _ if want != shown => Duration::ZERO,
            // Shown: check now and then that Discord still has it (it may have restarted).
            (Some(_), None) => REFRESH,
            (None, _) => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(next) => {
                want = next;
                // Take every queued update; only the newest counts.
                while let Ok(next) = rx.try_recv() {
                    want = next;
                }
            }
            Err(RecvTimeoutError::Timeout) => shown = None,
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let Some(status) = want.clone() else {
            if conn.take().is_some() {
                tracing::debug!("Rich Presence cleared");
            }
            shown = None;
            retry_at = None;
            continue;
        };
        if Some(&status) == shown.as_ref() && conn.is_some() {
            continue;
        }
        if retry_at.is_some_and(|t| Instant::now() < t) {
            continue;
        }
        let start = match (&since, status.recording_since) {
            (_, Some(rec)) => rec,
            (Some((game, t)), None) if *game == status.game => *t,
            _ => now_secs(),
        };
        since = Some((status.game.clone(), start));
        let result = (|| -> Result<()> {
            if conn.is_none() {
                conn = Some(Ipc::connect()?);
                tracing::info!("Rich Presence: connected to Discord");
            }
            conn.as_mut()
                .expect("just set")
                .set_activity(activity(&status, start))
        })();
        match result {
            Ok(()) => {
                shown = Some(status);
                retry_at = None;
            }
            Err(e) => {
                tracing::debug!("Rich Presence: {e:#}");
                conn = None;
                shown = None;
                retry_at = Some(Instant::now() + RETRY);
            }
        }
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
    let details = status.game.clone().unwrap_or_else(|| "Desktop".into());
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

struct Ipc {
    pipe: std::fs::File,
    nonce: u64,
}

impl Ipc {
    /// Opens the first Discord pipe that answers (Discord, PTB and Canary each take the next number).
    fn connect() -> Result<Self> {
        let mut last = None;
        for i in 0..10 {
            let path = format!(r"\\.\pipe\discord-ipc-{i}");
            match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
                Ok(pipe) => {
                    let mut ipc = Self { pipe, nonce: 0 };
                    ipc.handshake()?;
                    return Ok(ipc);
                }
                Err(e) => last = Some(e),
            }
        }
        match last {
            Some(e) => Err(e).context("Discord isn't running"),
            None => bail!("Discord isn't running"),
        }
    }

    fn handshake(&mut self) -> Result<()> {
        self.send(OP_HANDSHAKE, &json!({ "v": 1, "client_id": APP_ID }))?;
        let (op, reply) = self.receive()?;
        if op == OP_CLOSE || reply["evt"] != "READY" {
            bail!(
                "Discord refused the connection: {}",
                reply["message"].as_str().unwrap_or("no reason")
            );
        }
        Ok(())
    }

    fn set_activity(&mut self, activity: Value) -> Result<()> {
        self.nonce += 1;
        let cmd = json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": std::process::id(), "activity": activity },
            "nonce": self.nonce.to_string(),
        });
        self.send(OP_FRAME, &cmd)?;
        let (op, reply) = self.receive()?;
        if op == OP_CLOSE {
            bail!("Discord closed the connection");
        }
        if reply["evt"] == "ERROR" {
            bail!(
                "Discord didn't take the activity: {}",
                reply["data"]["message"].as_str().unwrap_or("no reason")
            );
        }
        Ok(())
    }

    fn send(&mut self, op: u32, payload: &Value) -> Result<()> {
        self.pipe
            .write_all(&frame(op, payload))
            .context("can't write to Discord")
    }

    fn receive(&mut self) -> Result<(u32, Value)> {
        let mut header = [0u8; 8];
        self.pipe
            .read_exact(&mut header)
            .context("can't read from Discord")?;
        let (op, len) = parse_header(header);
        if len > 1 << 20 {
            bail!("Discord sent an oversized reply ({len} bytes)");
        }
        let mut body = vec![0u8; len as usize];
        self.pipe
            .read_exact(&mut body)
            .context("can't read from Discord")?;
        Ok((op, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }
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
}
