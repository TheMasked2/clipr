//! The settings file, `config.toml`.
//!
//! Every field has a default, so a missing file, a missing section or an unknown old key never stops clipr
//! from starting. The web UI edits the same struct; hand edits keep working too.

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
    /// Give clipr's capture work priority on the GPU and CPU, so a game running at uncapped FPS can't
    /// starve it (stutter in clips). Never slows the game noticeably: clipr's GPU work is ~0.1 ms a frame.
    pub high_priority: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            codec: VideoCodec::H264,
            fps: 60,
            height: 1080,
            bitrate_kbps: 15_000,
            monitor: 0,
            high_priority: true,
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
    /// Apps recorded on their own tracks (exe names, e.g. `discord.exe`). Needs Windows 10 2004 or newer.
    pub apps: Vec<String>,
    /// Also write every source on its own track (track 1 is always the full mix; up to 6 tracks), so you
    /// can turn voice chat down later in an editor.
    pub separate_tracks: bool,
    /// Remove background noise (fans, keyboard) from the microphone (RNNoise).
    pub noise_suppression: bool,
    /// Mute the microphone while you're not talking.
    pub noise_gate: bool,
    /// The gate opens above this level, in dBFS (-60 quiet room … -30 loud room).
    pub gate_threshold_db: i32,
    /// Even out loud and quiet speech on the microphone.
    pub compressor: bool,
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
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            system: true,
            system_volume: 100,
            mic: false,
            mic_volume: 100,
            apps: Vec::new(),
            separate_tracks: false,
            noise_suppression: true,
            noise_gate: false,
            gate_threshold_db: -45,
            compressor: false,
            push_to_talk: String::new(),
            ptt_release_ms: 200,
            mic_offset_ms: 0,
            system_offset_ms: 0,
            bitrate_kbps: 160,
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
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            save_clip: "Alt+F10".into(),
            save_long_clip: String::new(),
            toggle_buffer: String::new(),
            toggle_recording: "Alt+F9".into(),
            mark: String::new(),
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GamesConfig {
    pub mode: GameMode,
    /// Extra games to detect, by exe name (`mygame.exe`).
    pub custom: Vec<String>,
    /// Exe names never treated as games, even if Discord lists them.
    pub blocked: Vec<String>,
    /// Start a full recording by itself when one of these games starts (exe names).
    pub auto_record: Vec<String>,
    /// Video settings that replace `video.*` while a given game runs.
    pub profiles: Vec<GameProfile>,
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
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            lan: false,
            lan_pin: String::new(),
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
    /// Check GitHub releases and update by itself. Opt-in.
    pub auto_update: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            open_ui_on_start: true,
            auto_update: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    /// Send anonymous crash reports. Opt-in.
    pub crash_reports: bool,
}

impl Config {
    /// Reads the file, or returns the defaults if it doesn't exist yet.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("can't read {}", path.display())),
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
}
