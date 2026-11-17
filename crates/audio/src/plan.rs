//! Which sources exist and which tracks mix what, from the user's settings.

use clipr_core::config::{GAME_SOURCE, MIC_SOURCE, SYSTEM_SOURCE};

/// A capture source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// The default output device (loopback).
    System,
    /// The default capture device.
    Mic,
    /// The detected game's process, via process loopback (see [`crate::set_game_pid`]).
    Game,
    /// One app (exe name), via process loopback.
    App(String),
}

impl SourceKind {
    /// The source's key in `audio.filters`: `system`, `mic`, or the app's exe name in lower case.
    pub fn key(&self) -> String {
        match self {
            Self::System => SYSTEM_SOURCE.into(),
            Self::Mic => MIC_SOURCE.into(),
            Self::Game => GAME_SOURCE.into(),
            Self::App(exe) => exe.to_ascii_lowercase(),
        }
    }

    /// The name shown in the UI and used as a track name.
    pub fn label(&self) -> String {
        match self {
            Self::System => "System".into(),
            Self::Mic => "Mic".into(),
            Self::Game => "Game".into(),
            // `.exe` in any case, like the UI's label.
            Self::App(exe) => match exe.len().checked_sub(4) {
                Some(i) if exe.is_char_boundary(i) && exe[i..].eq_ignore_ascii_case(".exe") => {
                    exe[..i].to_owned()
                }
                _ => exe.clone(),
            },
        }
    }

    /// A single process's sound (the game or an app): it gets its own track, and is in the mix only
    /// without system audio (which already holds it).
    pub fn is_process(&self) -> bool {
        matches!(self, Self::Game | Self::App(_))
    }
}

/// One output track: a name and the sources (indexes into [`Plan::sources`]) mixed into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackPlan {
    pub name: String,
    pub sources: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub sources: Vec<SourceKind>,
    /// Track 0 is always the mix.
    pub tracks: Vec<TrackPlan>,
    /// Things worth telling the user (dropped tracks).
    pub warnings: Vec<String>,
}

/// Most tracks an MP4 here carries, the mix included.
pub const MAX_TRACKS: usize = 6;

/// Builds the plan. Sources are system, mic, the game, then each distinct app.
///
/// The mix holds system and mic, plus the game and the apps only when system audio is off: with system
/// audio on, their sound is already in it, and adding them again would double it. The game and every app
/// always get their own track; `separate` also gives system audio and the mic one. Returns no sources when
/// nothing is enabled.
pub fn plan(system: bool, mic: bool, game: bool, apps: &[String], separate: bool) -> Plan {
    let mut sources = Vec::new();
    if system {
        sources.push(SourceKind::System);
    }
    if mic {
        sources.push(SourceKind::Mic);
    }
    if game {
        sources.push(SourceKind::Game);
    }
    for app in apps {
        let app = app.trim();
        let known = |s: &SourceKind| matches!(s, SourceKind::App(a) if a.eq_ignore_ascii_case(app));
        if !app.is_empty() && !sources.iter().any(known) {
            sources.push(SourceKind::App(app.to_owned()));
        }
    }
    let mut warnings = Vec::new();
    if sources.is_empty() {
        return Plan {
            sources,
            tracks: Vec::new(),
            warnings,
        };
    }

    let mix = (0..sources.len())
        .filter(|&i| !(system && sources[i].is_process()))
        .collect();
    let mut tracks = vec![TrackPlan {
        name: "Mix".into(),
        sources: mix,
    }];
    for (i, source) in sources.iter().enumerate() {
        if separate || source.is_process() {
            if tracks.len() == MAX_TRACKS {
                warnings.push(format!(
                    "only {MAX_TRACKS} audio tracks fit: {} is in the mix but has no track of its own",
                    source.label()
                ));
                continue;
            }
            tracks.push(TrackPlan {
                name: source.label(),
                sources: vec![i],
            });
        }
    }
    Plan {
        sources,
        tracks,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nothing_enabled_is_empty() {
        assert!(plan(false, false, false, &[], true).sources.is_empty());
        assert!(
            plan(false, false, false, &apps(&["", "  "]), false)
                .tracks
                .is_empty()
        );
    }

    #[test]
    fn system_and_mic_share_one_mix() {
        let p = plan(true, true, false, &[], false);
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].name, "Mix");
        assert_eq!(p.tracks[0].sources, [0, 1]);
    }

    #[test]
    fn apps_are_not_added_on_top_of_system_audio() {
        let p = plan(true, true, false, &apps(&["discord.exe"]), true);
        assert_eq!(p.sources.len(), 3);
        // The mix has system and mic only: discord is already inside the system audio.
        assert_eq!(p.tracks[0].sources, [0, 1]);
        let names: Vec<_> = p.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Mix", "System", "Mic", "discord"]);
        assert_eq!(p.tracks[3].sources, [2]);
    }

    #[test]
    fn apps_get_their_own_track_without_separate_tracks() {
        let p = plan(true, false, false, &apps(&["discord.exe"]), false);
        assert_eq!(
            p.sources,
            [SourceKind::System, SourceKind::App("discord.exe".into())]
        );
        let names: Vec<_> = p.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Mix", "discord"]);
        // Not in the mix: system audio already has it.
        assert_eq!(p.tracks[0].sources, [0]);
    }

    #[test]
    fn without_system_audio_the_mix_is_mic_plus_apps() {
        let p = plan(false, true, false, &apps(&["game.exe", "discord.exe"]), false);
        assert_eq!(p.tracks[0].sources, [0, 1, 2]);
        // Each app still gets its own track; the mic doesn't without separate tracks.
        let names: Vec<_> = p.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Mix", "game", "discord"]);
    }

    #[test]
    fn app_labels_drop_exe_in_any_case() {
        for exe in ["Discord.exe", "Discord.EXE", "Discord.Exe", "Discord"] {
            assert_eq!(SourceKind::App(exe.into()).label(), "Discord");
        }
        assert_eq!(SourceKind::App("é.exe".into()).label(), "é");
        assert_eq!(SourceKind::App("exe".into()).label(), "exe");
    }

    #[test]
    fn the_game_is_a_track_of_its_own() {
        let p = plan(true, true, true, &apps(&["discord.exe"]), false);
        assert_eq!(
            p.sources,
            [
                SourceKind::System,
                SourceKind::Mic,
                SourceKind::Game,
                SourceKind::App("discord.exe".into())
            ]
        );
        let names: Vec<_> = p.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Mix", "Game", "discord"]);
        // System audio already holds the game.
        assert_eq!(p.tracks[0].sources, [0, 1]);
        assert_eq!(p.tracks[1].sources, [2]);
        // Without system audio the mix needs it.
        assert_eq!(plan(false, true, true, &[], false).tracks[0].sources, [0, 1]);
        assert_eq!(SourceKind::Game.key(), "game");
    }

    #[test]
    fn duplicate_apps_collapse() {
        let p = plan(false, false, false, &apps(&["Discord.exe", "discord.EXE"]), false);
        assert_eq!(p.sources.len(), 1);
    }

    #[test]
    fn at_most_six_tracks_with_a_warning() {
        let p = plan(
            true,
            true,
            false,
            &apps(&["a.exe", "b.exe", "c.exe", "d.exe", "e.exe"]),
            true,
        );
        assert_eq!(p.tracks.len(), MAX_TRACKS);
        assert_eq!(p.warnings.len(), 2); // d and e
        assert!(p.warnings[0].contains('d'));
        // Still captured, and (system is on, so) not in the mix either way.
        assert_eq!(p.sources.len(), 7);
    }
}
