//! The capture engine: decides when to capture, runs the pipelines, and saves clips.
//!
//! One supervisor thread owns everything. Every ~200 ms it checks what should be captured (a detected game,
//! or the desktop in "always" mode, unless the user paused the buffer), starts, restarts or stops the video
//! pipeline to match, and handles commands from the tray, hotkeys and web UI. A pipeline that fails is
//! retried after a few seconds; the error shows in the tray and the UI meanwhile.
//!
//! The replay buffer outlives pipelines: when a game closes, its last moments can still be saved, until the
//! next pipeline starts with a fresh buffer.

pub mod preset;
mod record;
mod save;
mod video;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use clipr_audio::AudioCapture;

use crate::sound::{self, Cue};
use clipr_buffer::{Packet, ReplayBuffer, SECOND};
use clipr_core::config::{AudioConfig, Config, GameMode, VideoCodec};
use clipr_core::events::{Bus, Event};
use clipr_core::status::{Activity, Level, RecState, Status};
use clipr_encode::{Codec, EncodeSettings};
use clipr_games::{Game, GameWatcher};
use clipr_mux::MediaFormat;

use video::VideoSettings;

/// How long a failed pipeline waits before it's tried again.
const RETRY_AFTER: Duration = Duration::from_secs(5);
/// How often the supervisor reconsiders what to capture.
const TICK: Duration = Duration::from_millis(200);
/// How often the audio levels are refreshed (the web UI shows them 10 times a second).
const METER_EVERY: Duration = Duration::from_millis(100);
/// How often the foreground game is checked.
const GAME_POLL: Duration = Duration::from_secs(2);

pub enum Command {
    /// Save the last N seconds (`None` = `replay.clip_secs`).
    SaveClip(Option<u32>),
    /// Save the whole buffer.
    SaveLongClip,
    SetBuffering(bool),
    ToggleBuffering,
    Mark,
    /// New settings from the web UI.
    Apply(Box<Config>),
    /// Start or stop a full recording.
    SetRecording(bool),
    ToggleRecording,
    /// Mute (`true`) or unmute the microphone in clips.
    SetMicMuted(bool),
    ToggleMic,
    Stop,
}

/// Frame counters, for the logs and the "your game starves clipr" hint.
#[derive(Default, Debug, Clone, Copy)]
pub struct Stats {
    pub fresh: u64,
    pub repeated: u64,
    pub late: u64,
    pub dropped: u64,
}

/// What the pipelines share with the supervisor.
#[derive(Default)]
pub struct Shared {
    pub buffer: Option<ReplayBuffer>,
    pub video_format: Option<MediaFormat>,
    /// One per audio track; empty without audio.
    pub audio_formats: Vec<MediaFormat>,
    pub encoder: Option<String>,
    pub markers: VecDeque<i64>,
    pub stats: Stats,
    /// The running full recording, if any: it gets every packet the buffer gets.
    pub record: Option<record::RecordSink>,
}

impl Shared {
    fn push_video(&mut self, packet: Packet) {
        if let Some(r) = &self.record {
            r.video(&packet);
        }
        if let Some(b) = self.buffer.as_mut() {
            b.push_video(packet);
        }
    }

    fn push_audio(&mut self, track: usize, packet: Packet) {
        if let Some(r) = &self.record {
            r.audio(track, &packet);
        }
        if let Some(b) = self.buffer.as_mut() {
            b.push_audio(track, packet);
        }
    }
}

/// What a pipeline captures; a change restarts it.
#[derive(Clone, Debug, PartialEq)]
struct Wanted {
    video: VideoSettings,
    audio: AudioConfig,
}

/// The handle the rest of the app holds.
#[derive(Clone)]
pub struct Engine {
    tx: mpsc::Sender<Command>,
    levels: Levels,
}

/// Audio levels in dBFS per source, refreshed by the supervisor ~10 times a second.
type Levels = Arc<Mutex<Vec<Level>>>;

impl Engine {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    /// The latest audio levels; empty when no audio is running.
    pub fn levels(&self) -> Vec<Level> {
        self.levels.lock().unwrap().clone()
    }
}

/// Starts the supervisor thread. Join the handle after sending [`Command::Stop`].
pub fn start(
    config: Config,
    status: Arc<Status>,
    bus: Bus,
    watcher: Arc<GameWatcher>,
) -> (Engine, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    watcher.set_config(&config.games);
    let levels = Levels::default();
    let shared_levels = levels.clone();
    let thread = std::thread::Builder::new()
        .name("clipr-engine".into())
        .spawn(move || {
            let mut sup = Supervisor {
                buffering: config.replay.enabled,
                config,
                status,
                bus,
                watcher,
                game: None,
                buffer_game: None,
                saves: Vec::new(),
                recording: None,
                declined_auto: None,
                next_poll: Instant::now(),
                pipeline: None,
                retry_at: None,
                error: None,
                shared: Arc::default(),
                last_stats: Stats::default(),
                toast: crate::overlay::start(),
                levels: shared_levels,
            };
            sup.run(rx);
        })
        .expect("can't start a thread");
    (Engine { tx, levels }, thread)
}

struct Pipeline {
    stop: Arc<AtomicBool>,
    video: JoinHandle<Result<()>>,
    audio: Option<AudioCapture>,
    settings: Wanted,
}

/// A full recording: wanted until stopped; its file exists while capture runs.
struct Recording {
    /// The game process that auto-started it; it stops when that process is gone.
    auto: Option<u32>,
    file: Option<(record::Recorder, save::Reserved)>,
    next_disk_check: Instant,
}

struct Supervisor {
    config: Config,
    status: Arc<Status>,
    bus: Bus,
    watcher: Arc<GameWatcher>,
    /// The user's pause switch (tray, hotkey, UI).
    buffering: bool,
    game: Option<Game>,
    /// The game whose footage the buffer holds (`None`: the desktop). Kept while capture is paused or
    /// the game has closed, so later saves still land in the right folder.
    buffer_game: Option<String>,
    /// Saves still being written; joined on shutdown so no clip is left as a `.part` file.
    saves: Vec<JoinHandle<()>>,
    /// A full recording the user (or `games.auto_record`) asked for.
    recording: Option<Recording>,
    /// The game process whose auto recording the user stopped: don't start another for it.
    declined_auto: Option<u32>,
    next_poll: Instant,
    pipeline: Option<Pipeline>,
    retry_at: Option<Instant>,
    error: Option<String>,
    shared: Arc<Mutex<Shared>>,
    last_stats: Stats,
    toast: crate::overlay::Toast,
    levels: Levels,
}

impl Supervisor {
    fn run(&mut self, rx: mpsc::Receiver<Command>) {
        // Media Foundation stays up for the engine's life: audio media types are read on this thread.
        // SAFETY: balanced by MFShutdown below.
        let mf = unsafe {
            windows::Win32::Media::MediaFoundation::MFStartup(
                windows::Win32::Media::MediaFoundation::MF_VERSION,
                windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
            )
        };
        record::recover(&self.config.clips.dir());
        let mut stats_at = Instant::now() + Duration::from_secs(10);
        let mut tick_at = Instant::now();
        loop {
            match rx.recv_timeout(METER_EVERY) {
                Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(command) => {
                    self.handle(command);
                    tick_at = Instant::now() + TICK;
                    self.tick();
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.refresh_levels();
            if Instant::now() >= tick_at {
                tick_at = Instant::now() + TICK;
                self.tick();
            }
            if Instant::now() > stats_at {
                stats_at = Instant::now() + Duration::from_secs(10);
                self.check_stats();
            }
        }
        self.stop_pipeline();
        for save in self.saves.drain(..) {
            let _ = save.join();
        }
        if mf.is_ok() {
            // SAFETY: balances the MFStartup above; every MF object of this thread is gone.
            let _ = unsafe { windows::Win32::Media::MediaFoundation::MFShutdown() };
        }
        tracing::info!("capture engine stopped");
    }

    fn refresh_levels(&self) {
        let levels = self
            .pipeline
            .as_ref()
            .and_then(|p| p.audio.as_ref())
            .map(|a| a.levels())
            .unwrap_or_default();
        *self.levels.lock().unwrap() = levels;
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::SaveClip(secs) => self.save(secs.unwrap_or(self.config.replay.clip_secs)),
            Command::SaveLongClip => {
                self.save(self.config.replay.buffer_secs.max(self.config.replay.clip_secs))
            }
            Command::SetBuffering(on) => self.buffering = on,
            Command::ToggleBuffering => self.buffering = !self.buffering,
            Command::Mark => {
                let now = clipr_core::clock::now_100ns();
                if let Some((r, _)) = self.recording.as_ref().and_then(|r| r.file.as_ref()) {
                    r.mark(now);
                }
                let keep = i64::from(self.config.replay.buffer_secs) * SECOND;
                let mut s = self.shared.lock().unwrap();
                s.markers.push_back(now);
                while s.markers.front().is_some_and(|&m| m < now - keep) {
                    s.markers.pop_front();
                }
                tracing::info!("marker added");
            }
            Command::Apply(config) => {
                self.watcher.set_config(&config.games);
                if let Some(b) = self.shared.lock().unwrap().buffer.as_mut() {
                    b.set_keep(f64::from(config.replay.buffer_secs.max(config.replay.clip_secs)));
                }
                if config.replay.enabled != self.config.replay.enabled {
                    self.buffering = config.replay.enabled;
                }
                self.config = *config;
                self.retry_at = None; // new settings deserve a fresh try
            }
            Command::SetRecording(true) => self.start_recording(None),
            Command::SetRecording(false) => self.stop_recording(true),
            Command::ToggleRecording if self.recording.is_some() => self.stop_recording(true),
            Command::ToggleRecording => self.start_recording(None),
            Command::SetMicMuted(muted) => self.set_mic_muted(muted),
            Command::ToggleMic => self.set_mic_muted(!clipr_audio::mic_muted()),
            Command::Stop => {}
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if now >= self.next_poll {
            self.next_poll = now + GAME_POLL;
            self.game = self.watcher.poll();
            // The game audio source records whatever game runs now, unless the game is also one of the apps
            // on their own track: then that track has it, and recording it twice would double it in the mix.
            let apps = &self.config.audio.apps;
            clipr_audio::set_game_pid(
                self.game
                    .as_ref()
                    .filter(|g| !apps.iter().any(|a| a.trim().eq_ignore_ascii_case(&g.exe)))
                    .map(|g| g.pid),
            );
            if self.pipeline.is_some() {
                // What's on screen now is what the buffer's newest footage shows.
                self.buffer_game = self.game.as_ref().map(|g| g.name.clone());
            }
            self.saves.retain(|s| !s.is_finished());
            self.auto_record();
        }

        // A pipeline that ended by itself failed (or its monitor went away).
        if self.pipeline.as_ref().is_some_and(|p| p.video.is_finished()) {
            self.close_recording_file();
            let p = self.pipeline.take().expect("checked");
            let message = match p.video.join() {
                Ok(Ok(())) => "capture stopped unexpectedly".to_string(),
                Ok(Err(e)) => format!("{e:#}"),
                Err(_) => "the capture thread crashed".to_string(),
            };
            tracing::error!("capture failed: {message}");
            self.error = Some(message);
            self.retry_at = Some(now + RETRY_AFTER);
        }

        let wanted = self.wanted();
        let current = self.pipeline.as_ref().map(|p| &p.settings);
        if let (Some(w), Some(c)) = (&wanted, current)
            && w.video == c.video
            && w.audio != c.audio
        {
            // Only the sound changed: keep the video (and the buffered footage) running.
            self.restart_audio(w.audio.clone());
        }
        let current = self.pipeline.as_ref().map(|p| &p.settings);
        if wanted.as_ref() != current {
            if self.pipeline.is_some() {
                self.stop_pipeline();
            }
            if let Some(settings) = wanted
                && self.retry_at.is_none_or(|t| now >= t)
            {
                self.start_pipeline(settings);
            }
        }
        self.drive_recording(now);
        self.publish();
    }

    /// What should be captured right now, if anything.
    fn wanted(&self) -> Option<Wanted> {
        // A recording keeps capture running even with the buffer paused, and records the desktop when no
        // game runs.
        let recording = self.recording.is_some();
        if !self.buffering && !recording {
            return None;
        }
        let monitor = match (&self.game, self.config.games.mode) {
            (Some(game), _) => game.monitor,
            (None, GameMode::Always) => self.configured_monitor(),
            (None, GameMode::Auto) if recording => self.configured_monitor(),
            (None, GameMode::Auto) => return None,
        };
        let v = &self.config.video;
        let profiled;
        let v = match &self.game {
            Some(g) if !self.config.games.profiles.is_empty() => {
                profiled = v.for_game(&self.config.games.profiles, &g.name, &g.exe);
                &profiled
            }
            _ => v,
        };
        let video = VideoSettings {
            monitor,
            encode: EncodeSettings {
                codec: match v.codec {
                    VideoCodec::H264 => Codec::H264,
                    VideoCodec::Hevc => Codec::Hevc,
                    VideoCodec::Av1 => Codec::Av1,
                },
                fps: v.fps.clamp(10, 240),
                bitrate_kbps: v.bitrate_kbps,
                keyframe_interval_secs: 1.0,
                ..Default::default()
            },
            height: v.height,
            high_priority: v.high_priority,
        };
        Some(Wanted {
            video,
            audio: self.config.audio.clone(),
        })
    }

    fn configured_monitor(&self) -> isize {
        let n = self.config.video.monitor as usize;
        if n > 0
            && let Ok(list) = clipr_capture::list_monitors()
            && let Some(m) = list.get(n - 1)
        {
            return m.handle.0 as isize;
        }
        clipr_capture::primary_monitor().0 as isize
    }

    fn start_pipeline(&mut self, settings: Wanted) {
        let keep = self.config.replay.buffer_secs.max(self.config.replay.clip_secs);
        {
            let mut s = self.shared.lock().unwrap();
            s.buffer = Some(ReplayBuffer::new(f64::from(keep)));
            s.video_format = None;
            s.audio_formats.clear();
            s.encoder = None;
            s.stats = Stats::default();
        }
        self.last_stats = Stats::default();
        self.buffer_game = self.game.as_ref().map(|g| g.name.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let video = video::spawn(settings.video.clone(), self.shared.clone(), stop.clone());
        let audio = self.start_audio(&settings.audio);
        tracing::info!("capture started");
        self.pipeline = Some(Pipeline {
            stop,
            video,
            audio,
            settings,
        });
    }

    /// Swaps the audio capture of the running pipeline. The buffered audio goes (its tracks may differ);
    /// the buffered video stays.
    fn restart_audio(&mut self, config: AudioConfig) {
        if self.pipeline.is_none() {
            return;
        }
        // The audio tracks change, so a recording continues in a new file.
        self.close_recording_file();
        let Some(mut p) = self.pipeline.take() else {
            return;
        };
        if let Some(audio) = p.audio.take() {
            audio.stop();
        }
        {
            let mut s = self.shared.lock().unwrap();
            s.audio_formats.clear();
            if let Some(b) = s.buffer.as_mut() {
                b.clear_audio();
            }
        }
        p.audio = self.start_audio(&config);
        p.settings.audio = config;
        tracing::info!("audio restarted with new settings");
        self.pipeline = Some(p);
    }

    /// Starts audio capture into the buffer. A failure only costs the sound: video keeps going.
    fn start_audio(&mut self, config: &AudioConfig) -> Option<AudioCapture> {
        if !config.system && !config.mic && !config.game && config.apps.is_empty() {
            return None;
        }
        let shared = self.shared.clone();
        let capture = AudioCapture::start(config, move |p| {
            let packet = Packet {
                pts: p.pts_100ns,
                duration: p.duration_100ns,
                keyframe: true,
                data: p.data.into(),
            };
            shared.lock().unwrap().push_audio(p.track, packet);
        });
        let capture = match capture {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("audio capture failed, clips will be silent: {e:#}");
                return None;
            }
        };
        let names = capture.tracks();
        let formats: Result<Vec<MediaFormat>> = (0..names.len())
            .map(|t| MediaFormat::from_type(&capture.media_type(t)?))
            .collect();
        match formats {
            Ok(formats) => {
                tracing::info!("audio tracks: {}", names.join(", "));
                self.shared.lock().unwrap().audio_formats = formats;
                Some(capture)
            }
            Err(e) => {
                tracing::error!("audio format unavailable, clips will be silent: {e:#}");
                capture.stop();
                None
            }
        }
    }

    fn stop_pipeline(&mut self) {
        if self.pipeline.is_none() {
            return;
        }
        let Some(p) = self.pipeline.take() else {
            return;
        };
        if let Some(audio) = p.audio {
            audio.stop();
        }
        p.stop.store(true, Ordering::Relaxed);
        match p.video.join() {
            Ok(Err(e)) => tracing::warn!("capture ended with an error: {e:#}"),
            Err(_) => tracing::warn!("the capture thread crashed"),
            Ok(Ok(())) => {}
        }
        // Capture has stopped, so the file has every packet. A recording that is still wanted continues
        // in a new file once capture runs again.
        self.close_recording_file();
        self.shared.lock().unwrap().encoder = None;
        tracing::info!("capture stopped");
    }

    fn publish(&mut self) {
        let running = self.pipeline.is_some();
        if running && self.shared.lock().unwrap().video_format.is_some() {
            self.error = None; // frames flow again
        }
        let file = self.recording.as_ref().and_then(|r| r.file.as_ref());
        let state = match (running, self.recording.is_some()) {
            (_, true) => RecState::Recording,
            (true, false) => RecState::Buffering,
            (false, false) => RecState::Idle,
        };
        let recording_since = file.map(|(r, _)| r.since_unix);
        self.status.set(Activity {
            state,
            game: running
                .then(|| self.game.as_ref().map(|g| g.name.clone()))
                .flatten(),
            encoder: self.shared.lock().unwrap().encoder.clone(),
            error: self.error.clone(),
            recording_since,
            paused: !self.buffering,
            mic_muted: clipr_audio::mic_muted(),
        });
    }

    /// The mute hotkey (or the tray, or the web UI): mutes or unmutes the microphone in clips, with a blip and
    /// a note so the user knows which way it went. It isn't saved: clipr starts unmuted.
    fn set_mic_muted(&mut self, muted: bool) {
        if muted == clipr_audio::mic_muted() {
            return;
        }
        clipr_audio::set_mic_muted(muted);
        tracing::info!("microphone {}", if muted { "muted" } else { "on" });
        let cue = if muted { Cue::MicMuted } else { Cue::MicOn };
        sound::play(cue, self.config.replay.sound_volume);
        if self.config.replay.toast_on_save {
            self.toast
                .show(if muted { "Mic muted in clips" } else { "Mic on" }, !muted);
        }
        self.publish();
    }

    /// The save sound's volume, or `None` with it switched off.
    fn sound(&self) -> Option<u32> {
        let r = &self.config.replay;
        r.sound_on_save.then_some(r.sound_volume)
    }

    /// Logs frame statistics and warns when the game starves the capture.
    fn check_stats(&mut self) {
        let now = self.shared.lock().unwrap().stats;
        let d = Stats {
            fresh: now.fresh - self.last_stats.fresh,
            repeated: now.repeated - self.last_stats.repeated,
            late: now.late - self.last_stats.late,
            dropped: now.dropped - self.last_stats.dropped,
        };
        self.last_stats = now;
        if self.pipeline.is_none() {
            return;
        }
        tracing::debug!("last 10 s: {d:?}");
        if d.dropped + d.late > 30 {
            tracing::warn!(
                "{} late and {} dropped frames in 10 s: something is starving clipr (a game at uncapped \
                 FPS often does; capping it a little below the refresh rate fixes it)",
                d.late,
                d.dropped
            );
        }
    }

    fn save(&mut self, secs: u32) {
        let (cut, video_format, audio_formats, markers) = {
            let s = self.shared.lock().unwrap();
            let cut = s
                .buffer
                .as_ref()
                .map(|b| b.cut(f64::from(secs)))
                .unwrap_or_default();
            (
                cut,
                s.video_format.clone(),
                s.audio_formats.clone(),
                s.markers.iter().copied().collect::<Vec<_>>(),
            )
        };
        let (Some(video_format), false) = (video_format, cut.video.is_empty()) else {
            self.fail(
                "Nothing to save yet: clipr isn't capturing (no game detected, or the buffer is paused).",
                "Nothing to save yet",
            );
            return;
        };
        if let Some(message) = self.low_disk() {
            self.fail(&message, "Disk almost full");
            return;
        }
        let game = self.buffer_game.clone().unwrap_or_else(|| "Desktop".into());
        let reserved = save::clip_path(&self.config.clips.dir(), &game, "");
        let bus = self.bus.clone();
        let sound = self.sound();
        let toast = self.config.replay.toast_on_save.then(|| self.toast.clone());
        let length = cut.duration_secs();
        let worker = std::thread::Builder::new()
            .name("clipr-save".into())
            .spawn(move || {
                let path = reserved.path().to_path_buf();
                let started = Instant::now();
                match save::write(&path, &video_format, &audio_formats, &cut, &markers) {
                    Ok(()) => {
                        tracing::info!(
                            "saved {length:.1} s to {} in {} ms",
                            path.display(),
                            started.elapsed().as_millis()
                        );
                        cue(sound, true);
                        if let Some(t) = &toast {
                            t.show(&format!("Clip saved · {length:.0} s"), true);
                        }
                        bus.emit(Event::ClipSaved { path });
                    }
                    Err(e) => {
                        tracing::error!("saving the clip failed: {e:#}");
                        cue(sound, false);
                        if let Some(t) = &toast {
                            t.show("Couldn't save the clip", false);
                        }
                        bus.emit(Event::ClipFailed {
                            message: format!("Saving the clip failed: {e:#}"),
                        });
                    }
                }
                drop(reserved);
            })
            .expect("can't start a thread");
        self.saves.push(worker);
    }

    /// Asks for a full recording. `auto` is the game process that auto-started it.
    fn start_recording(&mut self, auto: Option<u32>) {
        if self.recording.is_some() {
            return;
        }
        if let Some(message) = self.low_disk() {
            self.fail(&message, "Disk almost full");
            self.decline_auto(); // don't retry an auto recording every poll
            return;
        }
        tracing::info!(
            "recording requested{}",
            if auto.is_some() { " (auto)" } else { "" }
        );
        self.recording = Some(Recording {
            auto,
            file: None,
            next_disk_check: Instant::now(),
        });
    }

    /// Stops the full recording. `decline`: don't auto-start another one for the game running now (the
    /// user stopped it, or a limit did).
    fn stop_recording(&mut self, decline: bool) {
        let Some(mut r) = self.recording.take() else {
            return;
        };
        if decline {
            self.decline_auto();
        }
        let file = r.file.take();
        self.finish_recording_file(file);
    }

    fn decline_auto(&mut self) {
        if let Some(g) = self.game.as_ref().filter(|g| g.auto_record) {
            self.declined_auto = Some(g.pid);
        }
    }

    /// Starts recording a game listed in `games.auto_record`, once per run of that game.
    fn auto_record(&mut self) {
        let Some(game) = &self.game else {
            self.declined_auto = None;
            return;
        };
        if game.auto_record && self.recording.is_none() && self.declined_auto != Some(game.pid) {
            let pid = game.pid;
            self.start_recording(Some(pid));
        }
    }

    /// Opens the file once capture runs, and stops at the time limit, on low disk, on a write failure, or
    /// when the auto-recorded game closes.
    fn drive_recording(&mut self, now: Instant) {
        let Some(rec) = self.recording.as_ref() else {
            return;
        };
        if let Some(pid) = rec.auto
            && self.game.as_ref().is_none_or(|g| g.pid != pid)
        {
            tracing::info!("the auto-recorded game closed");
            self.stop_recording(false);
            return;
        }
        if let Some((r, _)) = &rec.file {
            if let Some(why) = r.failure() {
                self.stop_recording(true);
                self.fail(&format!("The recording was stopped: {why}."), "Recording stopped");
                return;
            }
            let max = u64::from(self.config.recording.max_minutes);
            if max > 0 && r.started.elapsed() >= Duration::from_secs(max * 60) {
                tracing::info!("the recording reached its {max} minute limit");
                self.stop_recording(true);
                return;
            }
            if now >= rec.next_disk_check {
                if let Some(r) = self.recording.as_mut() {
                    r.next_disk_check = now + Duration::from_secs(10);
                }
                if let Some(message) = self.low_disk() {
                    self.stop_recording(true);
                    self.fail(
                        &format!("The recording was stopped. {message}"),
                        "Recording stopped: disk almost full",
                    );
                }
            }
            return;
        }
        if self.pipeline.is_none() {
            return;
        }
        let (video, audio) = {
            let s = self.shared.lock().unwrap();
            (s.video_format.clone(), s.audio_formats.clone())
        };
        let Some(video) = video else {
            return; // the encoder hasn't produced its first frame yet
        };
        let game = self
            .game
            .as_ref()
            .map_or("Desktop", |g| g.name.as_str())
            .to_string();
        let reserved = save::clip_path(&self.config.clips.dir(), &game, "_recording");
        let recorder = record::Recorder::start(reserved.path().to_path_buf(), video, audio);
        self.shared.lock().unwrap().record = Some(recorder.sink());
        if let Some(r) = self.recording.as_mut() {
            r.file = Some((recorder, reserved));
        }
    }

    /// Finishes the current recording file but keeps the recording wanted (capture is restarting).
    fn close_recording_file(&mut self) {
        let file = self.recording.as_mut().and_then(|r| r.file.take());
        self.finish_recording_file(file);
    }

    fn finish_recording_file(&mut self, file: Option<(record::Recorder, save::Reserved)>) {
        self.shared.lock().unwrap().record = None;
        let Some((recorder, reserved)) = file else {
            return;
        };
        let secs = recorder.started.elapsed().as_secs();
        match recorder.stop() {
            Ok(None) => tracing::info!("the recording stopped before its first frame; no file"),
            Ok(Some(path)) => {
                tracing::info!("recording saved: {} ({secs} s)", path.display());
                cue(self.sound(), true);
                if self.config.replay.toast_on_save {
                    self.toast
                        .show(&format!("Recording saved · {}:{:02}", secs / 60, secs % 60), true);
                }
                self.bus.emit(Event::ClipSaved { path });
            }
            Err(e) => {
                tracing::error!("the recording failed: {e:#}");
                self.fail(&format!("The recording failed: {e:#}"), "Recording failed");
            }
        }
        drop(reserved);
    }

    /// A message when the clips drive has less free space than `clips.min_free_gb` (0 = never).
    fn low_disk(&self) -> Option<String> {
        let min_gb = u64::from(self.config.clips.min_free_gb);
        if min_gb == 0 {
            return None;
        }
        let dir = self.config.clips.dir();
        let _ = std::fs::create_dir_all(&dir);
        let free = crate::win::free_bytes(&dir)?;
        const GB: u64 = 1 << 30;
        (free < min_gb * GB).then(|| {
            format!(
                "Not enough disk space: {:.1} GB free, at least {min_gb} GB needed (Settings → Library)",
                free as f64 / GB as f64
            )
        })
    }

    fn fail(&self, message: &str, toast: &str) {
        tracing::warn!("{message}");
        cue(self.sound(), false);
        if self.config.replay.toast_on_save {
            self.toast.show(toast, false);
        }
        self.bus.emit(Event::ClipFailed {
            message: message.into(),
        });
    }
}

/// The save sound (`None`: switched off): clipr's chime for a saved clip, a low tone for a failure.
fn cue(volume: Option<u32>, ok: bool) {
    if let Some(volume) = volume {
        sound::play(if ok { Cue::Saved } else { Cue::Failed }, volume);
    }
}
