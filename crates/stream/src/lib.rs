//! Live streaming: clipr's own H.264 and AAC sent over RTMP(S) to Twitch, YouTube or any RTMP server.
//!
//! [`Streamer::start`] runs a sender thread that connects, publishes the key and sends what the [`Sink`]
//! hands it. It never blocks capture: packets go through a queue, and when the connection can't keep up the
//! queued video is dropped back to the next keyframe (counted in [`Health::dropped`]). A dropped connection is
//! retried by itself with a growing pause; a refused key or address is not.

pub mod amf;
pub mod flv;
pub mod rtmp;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;

use amf::Amf;
use flv::AvcConfig;
use rtmp::{Connection, Endpoint, Refused};

/// What to stream and how it's encoded (for the metadata players read). No `Debug`: it holds the key.
#[derive(Clone, PartialEq)]
pub struct Settings {
    /// `rtmp://host/app` or `rtmps://host/app`.
    pub url: String,
    pub key: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_kbps: u32,
    pub audio_kbps: u32,
}

/// Where the stream stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Connecting,
    Live,
    /// Lost the connection; trying again. Carries what went wrong.
    Reconnecting(String),
    /// Gave up: the server refused the key or the address is wrong.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Health {
    pub state: State,
    /// When the stream first went live, in seconds since the Unix epoch.
    pub since: Option<u64>,
    /// What went out over the last few seconds.
    pub kbps: u32,
    /// Video frames dropped because the connection couldn't keep up.
    pub dropped: u64,
}

/// 100 ns units per millisecond.
const MS: i64 = 10_000;
/// Queued video beyond this much time means the connection can't keep up: drop it.
const MAX_LAG: i64 = 2_500 * MS;
const MAX_QUEUED_AUDIO: usize = 2_000;

enum Item {
    Video {
        pts: i64,
        keyframe: bool,
        data: Arc<[u8]>,
    },
    Audio {
        pts: i64,
        data: Arc<[u8]>,
    },
}

#[derive(Default)]
struct Queue {
    items: VecDeque<Item>,
    /// Dropping video until the next keyframe.
    skip_to_key: bool,
    /// ...because the connection fell behind (those count as dropped), not because the stream (re)started.
    behind: bool,
    /// Connected and sending. While connecting or waiting to reconnect, old video is let go without counting
    /// as dropped: nothing could have gone out.
    sending: bool,
}

struct Inner {
    queue: Mutex<Queue>,
    wake: Condvar,
    stop: AtomicBool,
    health: Mutex<Health>,
    /// Bytes sent, with when, for the bitrate.
    samples: Mutex<VecDeque<(Instant, u64)>>,
    /// Parameter sets from the encoder's output type, for encoders that don't repeat them in keyframes.
    avc: Mutex<Option<AvcConfig>>,
    settings: Mutex<Settings>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Hands encoded packets to the stream. Cheap to clone; never blocks for long.
#[derive(Clone)]
pub struct Sink(Arc<Inner>);

impl Sink {
    /// One encoded H.264 picture (Annex B), at `pts` on clipr's clock (100 ns units).
    pub fn video(&self, pts: i64, keyframe: bool, data: Arc<[u8]>) {
        let mut q = lock(&self.0.queue);
        let oldest = q.items.iter().find_map(|i| match i {
            Item::Video { pts, .. } => Some(*pts),
            Item::Audio { .. } => None,
        });
        if oldest.is_some_and(|o| pts - o > MAX_LAG) {
            // Behind: throw the queued video away and pick up again at a keyframe.
            let before = q.items.len();
            q.items.retain(|i| matches!(i, Item::Audio { .. }));
            let gone = (before - q.items.len()) as u64;
            q.skip_to_key = true;
            q.behind = q.sending;
            if q.sending {
                lock(&self.0.health).dropped += gone;
            }
        }
        if q.skip_to_key && !keyframe {
            if q.behind {
                lock(&self.0.health).dropped += 1;
            }
            return;
        }
        q.skip_to_key = false;
        q.behind = false;
        q.items.push_back(Item::Video { pts, keyframe, data });
        drop(q);
        self.0.wake.notify_one();
    }

    /// The encoder's parameter sets (Annex B SPS and PPS), when it has them outside the pictures.
    pub fn video_config(&self, annex_b: &[u8]) {
        if let Some(c) = AvcConfig::find(&flv::nal_units(annex_b)) {
            *lock(&self.0.avc) = Some(c);
        }
    }

    /// One raw AAC-LC frame (48 kHz stereo) at `pts`.
    pub fn audio(&self, pts: i64, data: Arc<[u8]>) {
        let mut q = lock(&self.0.queue);
        let audio = q.items.iter().filter(|i| matches!(i, Item::Audio { .. })).count();
        if audio >= MAX_QUEUED_AUDIO {
            if let Some(at) = q.items.iter().position(|i| matches!(i, Item::Audio { .. })) {
                q.items.remove(at);
            }
        }
        q.items.push_back(Item::Audio { pts, data });
        drop(q);
        self.0.wake.notify_one();
    }

    /// Whether both hand packets to the same stream.
    pub fn same(&self, other: &Sink) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// The picture size changed (capture restarted with another shape): the next metadata says so.
    pub fn set_size(&self, width: u32, height: u32) {
        let mut s = lock(&self.0.settings);
        s.width = width;
        s.height = height;
    }
}

/// A running stream. Stop it with [`Streamer::stop`]; dropping it stops it too.
pub struct Streamer {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl Streamer {
    pub fn start(settings: Settings) -> Self {
        let inner = Arc::new(Inner {
            queue: Mutex::default(),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            health: Mutex::new(Health {
                state: State::Connecting,
                since: None,
                kbps: 0,
                dropped: 0,
            }),
            samples: Mutex::default(),
            avc: Mutex::default(),
            settings: Mutex::new(settings),
        });
        let worker = inner.clone();
        let thread = std::thread::Builder::new()
            .name("clipr-stream".into())
            .spawn(move || run(&worker))
            .map_err(|e| tracing::error!("can't start the stream thread: {e}"))
            .ok();
        Self { inner, thread }
    }

    pub fn sink(&self) -> Sink {
        Sink(self.inner.clone())
    }

    pub fn health(&self) -> Health {
        let mut h = lock(&self.inner.health).clone();
        let samples = lock(&self.inner.samples);
        if let (Some(a), Some(b)) = (samples.front(), samples.back()) {
            let secs = b.0.duration_since(a.0).as_secs_f64();
            if secs > 0.5 {
                h.kbps = ((b.1 - a.1) as f64 * 8.0 / 1000.0 / secs).round() as u32;
            }
        }
        if h.state != State::Live {
            h.kbps = 0;
        }
        h
    }

    pub fn stop(mut self) {
        self.shut_down();
    }

    fn shut_down(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        self.inner.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Streamer {
    fn drop(&mut self) {
        self.shut_down();
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn set_state(inner: &Inner, state: State) {
    let mut h = lock(&inner.health);
    if state == State::Live && h.since.is_none() {
        h.since = Some(unix_now());
    }
    h.state = state;
}

/// Pauses between reconnects.
const BACKOFF: [u64; 5] = [2, 4, 8, 15, 30];

/// Takes the key out of a message that goes to the status (other devices see it) or the log: some servers
/// echo the stream name back when they refuse it.
fn mask(message: &str, key: &str) -> String {
    if key.trim().is_empty() {
        message.to_string()
    } else {
        message.replace(key.trim(), "<stream key>")
    }
}

/// How often a refusal is tried again before it counts as a wrong key: a hang-up can also be a hiccup, or the
/// server still holding the last stream with this key.
const REFUSALS: usize = 2;

fn run(inner: &Inner) {
    let mut failures = 0usize;
    let mut refusals = 0usize;
    let mut ever_live = false;
    while !inner.stop.load(Ordering::Relaxed) {
        let settings = lock(&inner.settings).clone();
        let mut went_live = false;
        let result = session(inner, &settings, &mut went_live);
        lock(&inner.queue).sending = false;
        if went_live {
            // On air again: the next hiccup starts the pauses over.
            ever_live = true;
            failures = 0;
        }
        let error = match result {
            Ok(()) => return,
            Err(e) => e,
        };
        if let Some(refused) = error.downcast_ref::<Refused>()
            && !ever_live
        {
            refusals += 1;
            let why = mask(&refused.0, &settings.key);
            tracing::warn!("stream refused: {why}");
            if refusals >= REFUSALS {
                set_state(inner, State::Failed(why));
                return;
            }
        }
        let message = mask(&format!("{error:#}"), &settings.key);
        tracing::warn!("stream connection lost: {message}");
        if !ever_live && failures >= 3 {
            // Never got on air: the address or the network is wrong, not a hiccup.
            set_state(inner, State::Failed(message));
            return;
        }
        set_state(inner, State::Reconnecting(message));
        let pause = Duration::from_secs(BACKOFF[failures.min(BACKOFF.len() - 1)]);
        failures += 1;
        let q = lock(&inner.queue);
        let _ = inner
            .wake
            .wait_timeout_while(q, pause, |_| !inner.stop.load(Ordering::Relaxed));
    }
}

/// One connection: publish, then send until stopped (`Ok`) or the connection fails.
fn session(inner: &Inner, settings: &Settings, went_live: &mut bool) -> Result<()> {
    let endpoint = Endpoint::parse(&settings.url).map_err(|e| Refused(format!("{e:#}")))?;
    let mut conn = Connection::publish(&endpoint, &settings.key)?;
    // The address only: a custom server's path can hold a key.
    tracing::info!("streaming to {}:{}", endpoint.host, endpoint.port);
    *went_live = true;
    lock(&inner.samples).clear();
    {
        // Anything queued while connecting is old by now: start fresh at the next keyframe.
        let mut q = lock(&inner.queue);
        q.items.clear();
        q.skip_to_key = true;
        q.behind = false;
        q.sending = true;
    }
    conn.send(rtmp::DATA, 0, metadata(settings))?;
    conn.send(
        rtmp::AUDIO,
        0,
        flv::aac_sequence_header(&flv::aac_lc_config(48_000, 2)),
    )?;

    let mut base: Option<i64> = None;
    let mut sent_config: Option<AvcConfig> = None;
    let mut last_video = 0u32;
    let mut last_audio = 0u32;
    let mut last_poll = Instant::now();
    loop {
        let items: Vec<Item> = {
            let q = lock(&inner.queue);
            let (mut q, _) = inner
                .wake
                .wait_timeout_while(q, Duration::from_millis(250), |q| {
                    q.items.is_empty() && !inner.stop.load(Ordering::Relaxed)
                })
                .unwrap_or_else(|e| e.into_inner());
            q.items.drain(..).collect()
        };
        if inner.stop.load(Ordering::Relaxed) {
            conn.close();
            return Ok(());
        }
        for item in items {
            match item {
                Item::Video { pts, keyframe, data } => {
                    let nals = flv::nal_units(&data);
                    if keyframe {
                        let config = AvcConfig::find(&nals).or_else(|| lock(&inner.avc).clone());
                        if let Some(c) = config
                            && sent_config.as_ref() != Some(&c)
                        {
                            let at = base.map_or(0, |b| ms(pts - b, last_video));
                            conn.send(rtmp::VIDEO, at, c.sequence_header())?;
                            sent_config = Some(c);
                        }
                    }
                    if sent_config.is_none() || (base.is_none() && !keyframe) {
                        continue;
                    }
                    let first = base.is_none();
                    let b = *base.get_or_insert(pts);
                    last_video = ms(pts - b, last_video);
                    conn.send(rtmp::VIDEO, last_video, flv::avc_frame(keyframe, &nals))?;
                    if first {
                        // Live once viewers get a picture, not just once the server said yes.
                        set_state(inner, State::Live);
                    }
                }
                Item::Audio { pts, data } => {
                    // Audio starts with the first picture, so both begin at 0.
                    let Some(b) = base.filter(|&b| pts >= b) else {
                        continue;
                    };
                    last_audio = ms(pts - b, last_audio);
                    conn.send(rtmp::AUDIO, last_audio, flv::aac_frame(&data))?;
                }
            }
        }
        if last_poll.elapsed() >= Duration::from_millis(250) {
            last_poll = Instant::now();
            conn.poll()?;
            let mut samples = lock(&inner.samples);
            samples.push_back((Instant::now(), conn.sent));
            while samples.len() > 12 {
                samples.pop_front();
            }
        }
    }
}

/// Milliseconds since the stream's start, never going backwards from `last`.
fn ms(since: i64, last: u32) -> u32 {
    ((since.max(0) / MS) as u32).max(last)
}

fn metadata(s: &Settings) -> Vec<u8> {
    amf::encode(&[
        Amf::str("@setDataFrame"),
        Amf::str("onMetaData"),
        Amf::EcmaArray(vec![
            ("duration".into(), Amf::Number(0.0)),
            ("width".into(), Amf::Number(f64::from(s.width))),
            ("height".into(), Amf::Number(f64::from(s.height))),
            ("videodatarate".into(), Amf::Number(f64::from(s.video_kbps))),
            ("framerate".into(), Amf::Number(f64::from(s.fps))),
            ("videocodecid".into(), Amf::Number(7.0)),
            ("audiodatarate".into(), Amf::Number(f64::from(s.audio_kbps))),
            ("audiosamplerate".into(), Amf::Number(48_000.0)),
            ("audiosamplesize".into(), Amf::Number(16.0)),
            ("stereo".into(), Amf::Bool(true)),
            ("audiocodecid".into(), Amf::Number(10.0)),
            (
                "encoder".into(),
                Amf::str(concat!("clipr ", env!("CARGO_PKG_VERSION"))),
            ),
        ]),
    ])
}
