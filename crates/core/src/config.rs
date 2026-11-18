//! The settings file, `config.toml`.
//!
//! Every field has a default, so a missing file, a missing section or an unknown old key never stops clipr
//! from starting. The web UI edits the same struct; hand edits keep working too.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The web UI's preferred port. If it's taken, clipr uses the next free one (see `clipr-web`).
pub const DEFAULT_PORT: u16 = 7845;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub web: WebConfig,
    pub ui: UiConfig,
    pub general: GeneralConfig,
    pub privacy: PrivacyConfig,
    pub replay: ReplayConfig,
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub hotkeys: HotkeyConfig,
    pub clips: ClipsConfig,
    pub games: GamesConfig,
    pub recording: RecordingConfig,
    pub discord: DiscordConfig,
    pub cloud: CloudConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayConfig {
    /// Keep the replay buffer running, so the save-clip hotkey has something to save.
    pub enabled: bool,
    /// How many seconds a saved clip goes back.
    pub clip_secs: u32,
    /// How much the buffer keeps in memory; the "save long clip" hotkey saves all of it. At 15 Mbit/s,
    /// 120 s is about 225 MB of RAM.
    pub buffer_secs: u32,
    /// Play a short sound when a clip is saved.
    pub sound_on_save: bool,
    /// How loud that sound (and the mic mute/unmute blip) plays, 0–100 %.
    pub sound_volume: u32,
    /// Show a small "Clip saved" note in the corner of the screen. It never shows up in clips, and games in
    /// exclusive fullscreen hide it.
    pub toast_on_save: bool,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            clip_secs: 30,
            buffer_secs: 120,
            sound_on_save: true,
            sound_volume: 60,
            toast_on_save: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    /// Plays everywhere (Discord, phones, browsers). The default.
    #[default]
    H264,
    /// Smaller files at the same quality; not every browser or phone plays it.
    Hevc,
    /// Smallest files; needs a recent GPU (RTX 40, RX 7000, Arc) and a recent player.
    Av1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    pub codec: VideoCodec,
    /// Frames per second. WGC delivers up to 60 for now.
    pub fps: u32,
    /// Output height in pixels (width follows the monitor's shape). 0 = the monitor's own resolution.
    pub height: u32,
    /// Video bitrate in kbit/s. Higher looks better and makes bigger files (15000 ≈ 56 MB per 30 s).
    pub bitrate_kbps: u32,
    /// Which monitor to capture: 0 = the primary monitor, 1, 2, … = that monitor in Windows' order.
    pub monitor: u32,
    /// Give clipr's capture work a raised GPU priority, so a game running at uncapped FPS can't starve it
    /// (stutter in clips). Off by default: on some PCs (seen on an RTX 4070 Ti) clips skipped more frames with it.
    /// Replaces beta.3's `high_priority`, which was on by default and used a higher GPU class.
    pub gpu_priority: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            codec: VideoCodec::H264,
            fps: 60,
            height: 1080,
            bitrate_kbps: 15_000,
            monitor: 0,
            gpu_priority: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Record what you hear (game, voice chat, music): the default output device.
    pub system: bool,
    /// System audio volume in the mix, 0–200 %.
    pub system_volume: u32,
    /// Record your default microphone, mixed into the same track.
    pub mic: bool,
    /// Microphone volume, 0–200 %.
    pub mic_volume: u32,
    /// Record the detected game's own sound on a track of its own (only the game, no voice chat or music).
    /// Follows whichever game clipr detects. Needs Windows 10 2004 or newer.
    pub game: bool,
    /// Apps recorded on their own tracks (exe names, e.g. `discord.exe`). Needs Windows 10 2004 or newer.
    pub apps: Vec<String>,
    /// Volume of the game and each app, 0–200 %, by source key (`game`, or an app's exe name in lower case).
    /// A source not listed plays at 100 %. System audio and the mic have their own fields.
    pub volumes: BTreeMap<String, u32>,
    /// Also give system audio and the mic a track of their own (apps always have one; track 1 is always the
    /// full mix; up to 6 tracks), so you can turn them down later in an editor.
    pub separate_tracks: bool,
    /// Filters per source, applied in order, like OBS: the key is `system`, `mic`, or an app's exe name
    /// (`discord.exe`). Every source starts with none. An entry clipr can't read (an unknown kind from a newer
    /// version, a missing value) is skipped with a warning instead of failing the whole file.
    #[serde(deserialize_with = "lenient_filters")]
    pub filters: BTreeMap<String, Vec<AudioFilter>>,
    /// Record the microphone only while this key is held (your voice-chat push-to-talk key, like `V` or
    /// `Mouse4`). Empty = always on.
    pub push_to_talk: String,
    /// Keep the microphone open this long after the push-to-talk key is released, in ms.
    pub ptt_release_ms: u32,
    /// Shift the microphone in time, in ms (positive = later), if it's out of sync.
    pub mic_offset_ms: i32,
    /// Shift system audio in time, in ms (positive = later).
    pub system_offset_ms: i32,
    /// AAC bitrate per track in kbit/s: 96, 128, 160 or 192.
    pub bitrate_kbps: u32,
    /// Beta 1's fixed mic filters, read once so [`AudioConfig::migrate`] can turn them into filters.
    #[serde(rename = "noise_gate", skip_serializing)]
    pub legacy_noise_gate: Option<bool>,
    #[serde(rename = "gate_threshold_db", skip_serializing)]
    pub legacy_gate_threshold_db: Option<i32>,
    #[serde(rename = "compressor", skip_serializing)]
    pub legacy_compressor: Option<bool>,
    /// Beta 1 had noise suppression on by default; it isn't carried over (filters start off now).
    #[serde(rename = "noise_suppression", skip_serializing)]
    pub legacy_noise_suppression: Option<bool>,
}

/// The [`AudioConfig::filters`] key for system audio.
pub const SYSTEM_SOURCE: &str = "system";
/// The [`AudioConfig::filters`] key for the microphone.
pub const MIC_SOURCE: &str = "mic";
/// The [`AudioConfig::filters`] key for the game's own sound ([`AudioConfig::game`]).
pub const GAME_SOURCE: &str = "game";

/// One audio filter. Levels are in dBFS, times in ms.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFilter {
    /// Off keeps the filter and its settings but skips it.
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(flatten)]
    pub kind: FilterKind,
}

fn yes() -> bool {
    true
}

fn lenient_filters<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, Vec<AudioFilter>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Good(AudioFilter),
        Bad(serde::de::IgnoredAny),
    }
    let raw = BTreeMap::<String, Vec<Entry>>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|(source, entries)| {
            let good = entries
                .into_iter()
                .filter_map(|e| match e {
                    Entry::Good(f) => Some(f),
                    Entry::Bad(_) => {
                        tracing::warn!("skipping an audio filter of {source} that clipr can't read");
                        None
                    }
                })
                .collect();
            (source, good)
        })
        .collect())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FilterKind {
    /// RNNoise: removes steady background noise (fans, hum, keyboard). `strength` 0-100 % blends the
    /// cleaned sound with the original.
    NoiseSuppression { strength: u32 },
    /// Mutes the source while it's quiet: opens above `open_db`, closes below `close_db` after `hold_ms`.
    NoiseGate {
        open_db: i32,
        close_db: i32,
        attack_ms: u32,
        hold_ms: u32,
        release_ms: u32,
    },
    /// Evens out loud and quiet parts: above `threshold_db`, every `ratio` dB in gives 1 dB out.
    Compressor {
        threshold_db: i32,
        ratio: u32,
        attack_ms: u32,
        release_ms: u32,
        makeup_db: i32,
    },
    /// Never lets a peak past `ceiling_db`.
    Limiter { ceiling_db: i32, release_ms: u32 },
    /// Turns the source up or down.
    Gain { db: i32 },
}

impl FilterKind {
    /// What a newly added filter starts with, by its `kind` name.
    pub fn defaults(name: &str) -> Option<Self> {
        Some(match name {
            "noise_suppression" => Self::NoiseSuppression { strength: 100 },
            "noise_gate" => Self::NoiseGate {
                open_db: -32,
                close_db: -40,
                attack_ms: 10,
                hold_ms: 150,
                release_ms: 100,
            },
            "compressor" => Self::Compressor {
                threshold_db: -20,
                ratio: 3,
                attack_ms: 5,
                release_ms: 80,
                makeup_db: 4,
            },
            "limiter" => Self::Limiter {
                ceiling_db: -1,
                release_ms: 100,
            },
            "gain" => Self::Gain { db: 0 },
            _ => return None,
        })
    }
}

impl AudioConfig {
    /// A source's volume in %, by its key (`system`, `mic`, `game` or an app's exe name, in any case).
    pub fn volume(&self, source: &str) -> u32 {
        if source.eq_ignore_ascii_case(SYSTEM_SOURCE) {
            self.system_volume
        } else if source.eq_ignore_ascii_case(MIC_SOURCE) {
            self.mic_volume
        } else {
            self.volumes
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(source))
                .map_or(100, |(_, &v)| v)
        }
    }

    /// Sets a source's volume in %, by its key.
    pub fn set_volume(&mut self, source: &str, percent: u32) {
        let source = source.to_ascii_lowercase();
        match source.as_str() {
            SYSTEM_SOURCE => self.system_volume = percent,
            MIC_SOURCE => self.mic_volume = percent,
            _ => {
                // A hand-edited file may spell the key in another case: replace every spelling.
                self.volumes.retain(|k, _| !k.eq_ignore_ascii_case(&source));
                if percent != 100 {
                    self.volumes.insert(source, percent);
                }
            }
        }
    }

    /// These settings without the volumes, to tell a volume change (applied live) from one that needs the
    /// audio capture restarted.
    pub fn without_volumes(&self) -> Self {
        Self {
            system_volume: 100,
            mic_volume: 100,
            volumes: BTreeMap::new(),
            ..self.clone()
        }
    }

    /// Turns beta 1's fixed mic gate and compressor, if they were switched on, into mic filters.
    pub fn migrate(&mut self) {
        let gate = self.legacy_noise_gate.take() == Some(true);
        let threshold = self.legacy_gate_threshold_db.take().unwrap_or(-45);
        let compressor = self.legacy_compressor.take() == Some(true);
        self.legacy_noise_suppression = None;
        if self.filters.contains_key(MIC_SOURCE) || !(gate || compressor) {
            return;
        }
        let mut mic = Vec::new();
        if gate {
            mic.push(AudioFilter {
                enabled: true,
                kind: FilterKind::NoiseGate {
                    open_db: threshold,
                    close_db: threshold,
                    attack_ms: 10,
                    hold_ms: 150,
                    release_ms: 100,
                },
            });
        }
        if compressor {
            mic.extend(FilterKind::defaults("compressor").map(|kind| AudioFilter { enabled: true, kind }));
        }
        self.filters.insert(MIC_SOURCE.into(), mic);
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            system: true,
            system_volume: 100,
            mic: false,
            mic_volume: 100,
            game: false,
            apps: Vec::new(),
            volumes: BTreeMap::new(),
            separate_tracks: false,
            filters: BTreeMap::new(),
            push_to_talk: String::new(),
            ptt_release_ms: 200,
            mic_offset_ms: 0,
            system_offset_ms: 0,
            bitrate_kbps: 160,
            legacy_noise_gate: None,
            legacy_gate_threshold_db: None,
            legacy_compressor: None,
            legacy_noise_suppression: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    /// Saves the last `replay.clip_secs` seconds. Like `Alt+F10` or `Ctrl+Shift+S`; empty turns it off.
    pub save_clip: String,
    /// Saves the whole buffer (`replay.buffer_secs`).
    pub save_long_clip: String,
    /// Pauses or resumes the replay buffer. Empty (the default) turns it off.
    pub toggle_buffer: String,
    /// Starts or stops a full recording.
    pub toggle_recording: String,
    /// Marks this moment; markers show on the clip's timeline in the gallery.
    pub mark: String,
    /// Mutes or unmutes the microphone in clips (not in your voice chat). Empty (the default) turns it off.
    pub toggle_mic: String,
    /// Opens or closes the in-game overlay (the Gallery and Settings over the game). Not `Alt+Z`, which is
    /// NVIDIA's overlay.
    pub overlay: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            save_clip: "Alt+F10".into(),
            save_long_clip: String::new(),
            toggle_buffer: String::new(),
            toggle_recording: "Alt+F9".into(),
            mark: String::new(),
            toggle_mic: String::new(),
            overlay: "Alt+F11".into(),
        }
    }
}

impl HotkeyConfig {
    /// The overlay hotkey came later, with a default: a config that already uses that key for something else
    /// leaves the overlay without one, instead of every Save being refused over a key the user never set.
    pub fn migrate(&mut self) {
        let norm = |k: &str| k.replace(' ', "").to_ascii_lowercase();
        let overlay = norm(&self.overlay);
        let others = [
            &self.save_clip,
            &self.save_long_clip,
            &self.toggle_buffer,
            &self.toggle_recording,
            &self.mark,
            &self.toggle_mic,
        ];
        if !overlay.is_empty() && others.iter().any(|k| norm(k) == overlay) {
            self.overlay.clear();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipsConfig {
    /// Where clips go. Empty = `Videos\clipr`. Each game gets its own subfolder.
    pub folder: String,
    /// Delete clips older than this many days (favorites never). 0 = keep forever.
    pub max_age_days: u32,
    /// Keep the clips folder under this many GB by deleting the oldest (favorites never). 0 = no limit.
    pub max_total_gb: u32,
    /// Stop recording and warn when the clips drive has less than this many GB free.
    pub min_free_gb: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GameMode {
    /// Buffer only while a known game runs; idle otherwise. The default.
    #[default]
    Auto,
    /// Always buffer the monitor, game or not.
    Always,
}

/// What "Game only" captures while a game runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GameCapture {
    /// Only the game's window: alt-tabbing, notifications and other monitors stay out of clips. The default.
    #[default]
    Window,
    /// The whole monitor the game is on (for games whose window can't be captured).
    Monitor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GamesConfig {
    pub mode: GameMode,
    /// What is captured while a game runs.
    pub capture: GameCapture,
    /// Also treat a fullscreen program as a game when it runs from a game library folder (Steam, Epic, Xbox,
    /// GOG, EA, Ubisoft, Riot, Amazon, itch), so brand-new games that aren't on the list yet are recorded too.
    pub detect_unlisted: bool,
    /// Extra games to detect: an exe name (`mygame.exe`), or a game's name as in its window title or folder
    /// (`Trust Your Gut`).
    pub custom: Vec<String>,
    /// Exe names never treated as games, even if Discord lists them.
    pub blocked: Vec<String>,
    /// Start a full recording by itself when one of these games starts (exe names).
    pub auto_record: Vec<String>,
    /// Video settings that replace `video.*` while a given game runs.
    pub profiles: Vec<GameProfile>,
}

impl Default for GamesConfig {
    fn default() -> Self {
        Self {
            mode: GameMode::default(),
            capture: GameCapture::default(),
            detect_unlisted: true,
            custom: Vec::new(),
            blocked: Vec::new(),
            auto_record: Vec::new(),
            profiles: Vec::new(),
        }
    }
}

/// Video settings for one game. Empty fields keep the `video.*` value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameProfile {
    /// The game's exe (`cs2.exe`) or display name; case doesn't matter.
    pub game: String,
    pub fps: Option<u32>,
    pub height: Option<u32>,
    pub bitrate_kbps: Option<u32>,
}

impl GameProfile {
    fn matches(&self, name: &str, exe: &str) -> bool {
        let want = self.game.trim();
        !want.is_empty() && (want.eq_ignore_ascii_case(name.trim()) || want.eq_ignore_ascii_case(exe.trim()))
    }
}

impl VideoConfig {
    /// These settings with the first matching profile's values on top (the game's display name or exe).
    pub fn for_game(&self, profiles: &[GameProfile], name: &str, exe: &str) -> VideoConfig {
        let mut v = self.clone();
        if let Some(p) = profiles.iter().find(|p| p.matches(name, exe)) {
            v.fps = p.fps.unwrap_or(v.fps);
            v.height = p.height.unwrap_or(v.height);
            v.bitrate_kbps = p.bitrate_kbps.unwrap_or(v.bitrate_kbps);
        }
        v
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingConfig {
    /// Stop a recording after this many minutes. 0 = no limit.
    pub max_minutes: u32,
    /// Where full recordings go, in game folders like clips. Empty = the clips folder.
    pub folder: String,
}

impl RecordingConfig {
    /// The folder recordings go to: their own, or the clips folder.
    pub fn dir(&self, clips: &ClipsConfig) -> std::path::PathBuf {
        match self.folder.trim() {
            "" => clips.dir(),
            f => f.into(),
        }
    }

    /// The separate recordings folder, if one is set (and it isn't the clips folder, however it's written).
    pub fn own_dir(&self, clips: &ClipsConfig) -> Option<std::path::PathBuf> {
        Some(self.dir(clips)).filter(|d| !same_folder(d, &clips.dir()))
    }
}

/// A Windows path compared the way Windows does: any case, `/` or `\`, no trailing separator.
pub fn folder_key(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('/', "\\").to_lowercase();
    match s.trim_end_matches('\\') {
        "" => s,
        t if t.ends_with(':') => format!("{t}\\"),
        t => t.to_string(),
    }
}

/// Whether two paths name the same folder (see [`folder_key`]).
pub fn same_folder(a: &std::path::Path, b: &std::path::Path) -> bool {
    folder_key(a) == folder_key(b)
}

impl Default for ClipsConfig {
    fn default() -> Self {
        Self {
            folder: String::new(),
            max_age_days: 0,
            max_total_gb: 0,
            min_free_gb: 5,
        }
    }
}

impl ClipsConfig {
    /// The clips folder, with the default filled in.
    pub fn dir(&self) -> std::path::PathBuf {
        if !self.folder.trim().is_empty() {
            return self.folder.trim().into();
        }
        dirs::video_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
            .unwrap_or_else(|| ".".into())
            .join("clipr")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// First port to try for the web UI.
    pub port: u16,
    /// Open the web UI to the local network (phone at home). Off by default; needs `lan_pin` once built.
    pub lan: bool,
    /// The 6-digit PIN other devices on the LAN must enter. Generated when LAN access is first turned on.
    pub lan_pin: String,
    /// Other devices may see the live capture preview (the whole screen, not just games). Off by default.
    pub lan_preview: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            lan: false,
            lan_pin: String::new(),
            lan_preview: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// `dark-sleek` (default), later also `minimal`, `playful`, `system`.
    pub theme: String,
    /// Accent colour as `#rrggbb`.
    pub accent: String,
    /// The first-run wizard was finished or skipped.
    pub setup_done: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark-sleek".into(),
            accent: "#8B5CF6".into(),
            setup_done: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Start clipr when Windows starts.
    pub start_with_windows: bool,
    /// Open the web UI when you start clipr yourself, so you can see it's running. Never happens when
    /// Windows starts it (`--background`).
    pub open_ui_on_start: bool,
    /// Check GitHub releases and update by itself. Opt-in. "Check now" in the UI works either way.
    pub auto_update: bool,
    /// Offer beta (prerelease) versions too. On by default while clipr itself is a beta.
    pub update_beta: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            open_ui_on_start: true,
            auto_update: false,
            update_beta: crate::VERSION.contains('-'),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    /// Send anonymous crash reports. Opt-in.
    pub crash_reports: bool,
}

/// Where clips go on Discord, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordConfig {
    /// Channels to post to, each through a webhook (Server Settings → Integrations → Webhooks).
    pub webhooks: Vec<Webhook>,
    /// Post while a game runs. Off: posts wait until the game closes, so an upload never costs frames.
    pub post_during_game: bool,
    /// Your Discord upload limit in MB: 10 (free), 50 (Nitro Basic) or 500 (Nitro). A bigger clip is sent
    /// as a smaller copy that fits (the clip itself is never changed).
    pub upload_limit_mb: u32,
    /// Rich Presence: show the game clipr clips (and its icon) in your Discord profile. Needs the Discord app
    /// running on this PC; nothing goes online.
    pub presence: bool,
    /// The Discord account connected through "Connect Discord" (shown in Settings; no token is kept).
    pub account: Option<DiscordAccount>,
}

/// Who connected their Discord account: only what clipr shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordAccount {
    pub id: String,
    pub username: String,
    /// Display name, when set.
    #[serde(default)]
    pub name: Option<String>,
    /// Avatar URL on Discord's CDN, when set.
    #[serde(default)]
    pub avatar: Option<String>,
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            webhooks: Vec::new(),
            post_during_game: false,
            upload_limit_mb: 10,
            presence: true,
            account: None,
        }
    }
}

/// One Discord channel clips can be posted to.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Webhook {
    /// What the UI calls it, like `#clips`. Unique.
    pub name: String,
    /// The webhook URL Discord gives you (`https://discord.com/api/webhooks/...`). Anyone with it can post
    /// to the channel, so it's never logged.
    pub url: String,
    /// What happens to a new clip: ask first, post it, or nothing (sharing by hand still works).
    pub mode: PostMode,
    /// Only clips of these games (names as clipr shows them). Empty = every game.
    pub games: Vec<String>,
}

impl Default for Webhook {
    fn default() -> Self {
        Self {
            name: String::new(),
            url: String::new(),
            mode: PostMode::Ask,
            games: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PostMode {
    /// Every new clip waits for a yes in the UI.
    #[default]
    Ask,
    /// Every new clip is posted (after the game, unless posting during games is on).
    Auto,
    /// New clips aren't posted; sharing a clip by hand still works.
    Off,
}

// Not derived: the URL is a secret and must never reach a log through `{:?}`.
impl std::fmt::Debug for Webhook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Webhook")
            .field("name", &self.name)
            .field("url", &if self.url.is_empty() { "" } else { "<hidden>" })
            .field("mode", &self.mode)
            .field("games", &self.games)
            .finish()
    }
}

/// Your own bucket on Cloudflare R2, Amazon S3, Backblaze B2 or another S3-compatible service. Clips are
/// copied there; the files on this PC stay.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudConfig {
    /// Copy clips to the bucket.
    pub enabled: bool,
    /// Which service (only picks the setup steps and fills in the endpoint).
    pub provider: CloudProvider,
    /// The S3 endpoint, `https://<account id>.r2.cloudflarestorage.com` on R2.
    pub endpoint: String,
    /// The bucket's region; `auto` on R2.
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    /// A secret: never logged, and other devices never see or set it.
    pub secret_access_key: String,
    /// The bucket's public address (an `r2.dev` link or your own domain). Discord gets links under it;
    /// without one, Discord gets a smaller copy of the file instead.
    pub public_url: String,
    /// Folder in the bucket clips go to.
    pub prefix: String,
    /// When new clips go up.
    pub upload: UploadTiming,
    /// Upload speed limit in Mbit/s (0 = no limit).
    pub max_mbps: u32,
    /// Copy full recordings too (they can be large).
    pub recordings: bool,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: CloudProvider::R2,
            endpoint: String::new(),
            region: "auto".into(),
            bucket: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            public_url: String::new(),
            prefix: "clipr/".into(),
            upload: UploadTiming::AfterGame,
            max_mbps: 0,
            recordings: false,
        }
    }
}

// Not derived: the secret must never reach a log through `{:?}`.
impl std::fmt::Debug for CloudConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudConfig")
            .field("enabled", &self.enabled)
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field(
                "secret_access_key",
                &if self.secret_access_key.is_empty() {
                    ""
                } else {
                    "<hidden>"
                },
            )
            .field("public_url", &self.public_url)
            .field("prefix", &self.prefix)
            .field("upload", &self.upload)
            .field("max_mbps", &self.max_mbps)
            .field("recordings", &self.recordings)
            .finish()
    }
}

impl CloudConfig {
    /// Whether everything needed to upload is filled in.
    pub fn ready(&self) -> bool {
        self.enabled
            && !self.endpoint.trim().is_empty()
            && !self.bucket.trim().is_empty()
            && !self.access_key_id.trim().is_empty()
            && !self.secret_access_key.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloudProvider {
    #[default]
    R2,
    S3,
    B2,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadTiming {
    /// Once no game runs, so uploading never costs frames.
    #[default]
    AfterGame,
    /// Right away (the speed limit keeps games playable).
    Now,
    /// Only clips you upload by hand.
    Manual,
}

impl Webhook {
    /// Whether new clips of `game` go to this webhook at all (`Off` never does).
    pub fn wants(&self, game: &str) -> bool {
        self.mode != PostMode::Off
            && (self.games.is_empty()
                || self
                    .games
                    .iter()
                    .any(|g| g.trim().eq_ignore_ascii_case(game.trim())))
    }
}

impl Config {
    /// Reads the file, or returns the defaults if it doesn't exist yet.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let mut config: Self =
                    toml::from_str(&text).with_context(|| format!("can't read {}", path.display()))?;
                config.audio.migrate();
                config.hotkeys.migrate();
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("can't open {}", path.display())),
        }
    }

    /// Like [`Config::load`], but a broken file is kept aside as `config.toml.broken` and the defaults are
    /// used, so a typo never stops clipr from starting.
    pub fn load_or_recover(path: &Path) -> Self {
        match Self::load(path) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!("{err:#}; starting with default settings");
                let aside = path.with_extension("toml.broken");
                if let Err(e) = fs::rename(path, &aside) {
                    tracing::warn!("couldn't keep the broken settings file aside: {e}");
                }
                Self::default()
            }
        }
    }

    /// Writes the file atomically: a temporary file first, then a rename, so a crash mid-save never leaves a
    /// half-written config.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self).context("can't serialise settings")?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).with_context(|| format!("can't write {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("can't replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recordings_folder_written_like_the_clips_folder_is_the_same_folder() {
        let clips = ClipsConfig {
            folder: r"D:\Clips".into(),
            ..ClipsConfig::default()
        };
        for same in [r"d:\clips", "D:/Clips/", r"D:\Clips\"] {
            let rec = RecordingConfig {
                folder: same.into(),
                ..RecordingConfig::default()
            };
            assert_eq!(rec.own_dir(&clips), None, "{same}");
        }
        let rec = RecordingConfig {
            folder: r"E:\Recordings".into(),
            ..RecordingConfig::default()
        };
        assert_eq!(rec.own_dir(&clips), Some(r"E:\Recordings".into()));
        assert_eq!(folder_key(std::path::Path::new("C:/")), r"c:\");
    }

    #[test]
    fn volumes_by_source_and_compared_without_them() {
        let mut a = AudioConfig::default();
        a.set_volume("MIC", 50);
        a.set_volume("Discord.exe", 120);
        assert_eq!(
            (a.mic_volume, a.volume("discord.EXE"), a.volume("game")),
            (50, 120, 100)
        );
        assert_eq!(a.without_volumes(), AudioConfig::default());
        a.set_volume("discord.exe", 100);
        assert!(a.volumes.is_empty());
        a.volumes.insert("Game".into(), 50);
        a.set_volume("game", 70);
        assert_eq!((a.volumes.len(), a.volume("GAME")), (1, 70));
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.web.port, DEFAULT_PORT);
        assert!(!config.general.auto_update, "auto-update must be opt-in");
        assert!(!config.privacy.crash_reports, "crash reports must be opt-in");
        assert!(!config.web.lan, "LAN access must be opt-in");
        assert!(
            config.general.open_ui_on_start,
            "a manual start should show where clipr went"
        );
        assert_eq!(config.video.codec, VideoCodec::H264);
        assert!(config.clips.dir().ends_with("clipr"));
    }

    #[test]
    fn audio_filters_round_trip_and_start_empty() {
        assert!(
            AudioConfig::default().filters.is_empty(),
            "no filter is on by default"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config::default();
        let gate = FilterKind::defaults("noise_gate").unwrap();
        config.audio.filters.insert(
            MIC_SOURCE.into(),
            vec![
                AudioFilter {
                    enabled: true,
                    kind: FilterKind::NoiseSuppression { strength: 70 },
                },
                AudioFilter {
                    enabled: false,
                    kind: gate,
                },
            ],
        );
        config.audio.filters.insert(
            "discord.exe".into(),
            vec![AudioFilter {
                enabled: true,
                kind: FilterKind::Gain { db: -6 },
            }],
        );
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        // The JSON the web UI gets has the same shape.
        let json = serde_json::to_value(&config.audio.filters).unwrap();
        assert_eq!(json["mic"][0]["kind"], "noise_suppression");
        assert_eq!(json["mic"][1]["enabled"], false);
    }

    #[test]
    fn a_bad_filter_is_skipped_not_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "[web]\nport = 9000\n\n[[audio.filters.mic]]\nkind = \"gain\"\ndb = -3\n\n\
                    [[audio.filters.mic]]\nkind = \"reverb\"\nroom = 5\n\n[[audio.filters.mic]]\nkind = \"limiter\"\n";
        fs::write(&path, text).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.web.port, 9000, "the rest of the file is kept");
        let mic = &config.audio.filters[MIC_SOURCE];
        assert_eq!(
            mic.len(),
            1,
            "the unknown kind and the limiter with missing values are skipped"
        );
        assert_eq!(mic[0].kind, FilterKind::Gain { db: -3 });
    }

    #[test]
    fn beta_mic_filters_migrate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let old = "[audio]\nnoise_suppression = true\nnoise_gate = true\ngate_threshold_db = -50\ncompressor = false\n";
        fs::write(&path, old).unwrap();
        let config = Config::load(&path).unwrap();
        let mic = &config.audio.filters[MIC_SOURCE];
        assert_eq!(
            mic.len(),
            1,
            "only the gate was switched on; suppression was just the old default"
        );
        assert!(matches!(mic[0].kind, FilterKind::NoiseGate { open_db: -50, .. }));
        assert_eq!(config.audio.legacy_noise_gate, None);
        // Saving drops the old keys.
        config.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("noise_suppression =") && !text.contains("gate_threshold_db"),
            "{text}"
        );

        fs::write(&path, "[audio]\nnoise_suppression = true\n").unwrap();
        assert!(Config::load(&path).unwrap().audio.filters.is_empty());
    }

    #[test]
    fn webhooks_match_games_and_modes() {
        let mut w = Webhook {
            name: "#clips".into(),
            url: "https://discord.com/api/webhooks/1/x".into(),
            mode: PostMode::Auto,
            games: vec![],
        };
        assert!(w.wants("VALORANT"));
        w.games = vec!["valorant ".into()];
        assert!(w.wants("VALORANT") && !w.wants("Minecraft"));
        w.mode = PostMode::Off;
        assert!(!w.wants("VALORANT"));
        let text = toml::to_string(&DiscordConfig {
            webhooks: vec![w],
            ..Default::default()
        })
        .unwrap();
        assert!(text.contains("mode = \"off\""), "{text}");
        assert_eq!(Config::default().discord.upload_limit_mb, 10);
        let shown = format!(
            "{:?}",
            Webhook {
                url: "https://discord.com/api/webhooks/1/secret".into(),
                ..Default::default()
            }
        );
        assert!(!shown.contains("secret"), "{shown}");
    }

    #[test]
    fn codec_is_lowercase_in_the_file() {
        let text = toml::to_string(&Config::default()).unwrap();
        assert!(text.contains("codec = \"h264\""), "{text}");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let mut config = Config::default();
        config.web.port = 9000;
        config.ui.accent = "#22D3EE".into();
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[web]\nport = 8000\n\n[unknown_old_section]\nx = 1\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.web.port, 8000);
        assert!(!config.web.lan);
        assert_eq!(config.ui, UiConfig::default());
    }

    #[test]
    fn profile_overrides_only_what_it_sets() {
        let video = VideoConfig::default();
        let profiles = vec![
            GameProfile {
                game: "  Counter-Strike 2 ".into(),
                fps: Some(120),
                ..Default::default()
            },
            GameProfile {
                game: "CS2.EXE".into(),
                bitrate_kbps: Some(40_000),
                ..Default::default()
            },
            GameProfile::default(),
        ];
        // Matches by display name, ignoring case and spaces; the first match wins.
        let v = video.for_game(&profiles, "counter-strike 2", "cs2.exe");
        assert_eq!((v.fps, v.height, v.bitrate_kbps), (120, 1080, 15_000));
        // Matches by exe.
        let v = video.for_game(&profiles, "Other name", "cs2.exe");
        assert_eq!((v.fps, v.bitrate_kbps), (60, 40_000));
        // No match, or an empty profile name: untouched.
        assert_eq!(video.for_game(&profiles, "Doom", "doom.exe"), video);
        assert_eq!(video.for_game(&profiles, "", ""), video);
    }

    #[test]
    fn profiles_round_trip_through_toml() {
        let mut config = Config::default();
        config.games.profiles.push(GameProfile {
            game: "doom.exe".into(),
            fps: Some(30),
            height: None,
            bitrate_kbps: None,
        });
        let text = toml::to_string_pretty(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn broken_file_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "this is = = not toml").unwrap();
        assert!(Config::load(&path).is_err());
        assert_eq!(Config::load_or_recover(&path), Config::default());
        assert!(path.with_extension("toml.broken").exists());
        assert!(!path.exists());
    }

    #[test]
    fn the_overlay_hotkey_gives_way_to_a_key_already_in_use() {
        let mut h = HotkeyConfig {
            mark: "alt+f11".into(),
            ..HotkeyConfig::default()
        };
        h.migrate();
        assert_eq!(h.overlay, "");
        let mut h = HotkeyConfig::default();
        h.migrate();
        assert_eq!(h.overlay, "Alt+F11");
    }
}
