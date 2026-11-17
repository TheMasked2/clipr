//! Which sources exist and which tracks mix what, from the user's settings.

use clipr_core::config::{MIC_SOURCE, SYSTEM_SOURCE};

/// A capture source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// The default output device (loopback).
    System,
    /// The default capture device.
    Mic,
    /// One app (exe name), via process loopback.
    App(String),
}

impl SourceKind {
    /// The source's key in `audio.filters`: `system`, `mic`, or the app's exe name in lower case.
    pub fn key(&self) -> String {
        match self {
            Self::System => SYSTEM_SOURCE.into(),
            Self::Mic => MIC_SOURCE.into(),
            Self::App(exe) => exe.to_ascii_lowercase(),
        }
    }

    /// The name shown in the UI and used as a track name.
    pub fn label(&self) -> String {
        match self {
            Self::System => "System".into(),
            Self::Mic => "Mic".into(),
            Self::App(exe) => exe
                .strip_suffix(".exe")
                .or_else(|| exe.strip_suffix(".EXE"))
                .unwrap_or(exe)
                .to_owned(),
        }
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

/// Builds the plan. Sources are system, mic, then each distinct app.
///
/// The mix holds system and mic, plus the apps only when system audio is off: with system audio on, an
/// app's sound is already in it, and adding the app again would double it. Apps always get their own
/// track when `separate` is on. Returns no sources when nothing is enabled.
pub fn plan(system: bool, mic: bool, apps: &[String], separate: bool) -> Plan {
    let mut sources = Vec::new();
    if system {
        sources.push(SourceKind::System);
    }
    if mic {
        sources.push(SourceKind::Mic);
    }
    // With system audio on and no separate tracks an app's sound has nowhere to go: don't capture it.
    let want_apps = !system || separate;
    for app in apps.iter().filter(|_| want_apps) {
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
        .filter(|&i| !(system && matches!(sources[i], SourceKind::App(_))))
        .collect();
    let mut tracks = vec![TrackPlan {
        name: "Mix".into(),
        sources: mix,
    }];
    if separate {
        for (i, source) in sources.iter().enumerate() {
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
        assert!(plan(false, false, &[], true).sources.is_empty());
        assert!(plan(false, false, &apps(&["", "  "]), false).tracks.is_empty());
    }

    #[test]
    fn system_and_mic_share_one_mix() {
        let p = plan(true, true, &[], false);
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].name, "Mix");
        assert_eq!(p.tracks[0].sources, [0, 1]);
    }

    #[test]
    fn apps_are_not_added_on_top_of_system_audio() {
        let p = plan(true, true, &apps(&["discord.exe"]), true);
        assert_eq!(p.sources.len(), 3);
        // The mix has system and mic only: discord is already inside the system audio.
        assert_eq!(p.tracks[0].sources, [0, 1]);
        let names: Vec<_> = p.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Mix", "System", "Mic", "discord"]);
        assert_eq!(p.tracks[3].sources, [2]);
    }

    #[test]
    fn apps_are_skipped_when_they_would_go_nowhere() {
        let p = plan(true, false, &apps(&["discord.exe"]), false);
        assert_eq!(p.sources, [SourceKind::System]);
    }

    #[test]
    fn without_system_audio_the_mix_is_mic_plus_apps() {
        let p = plan(false, true, &apps(&["game.exe", "discord.exe"]), false);
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].sources, [0, 1, 2]);
    }

    #[test]
    fn duplicate_apps_collapse() {
        let p = plan(false, false, &apps(&["Discord.exe", "discord.EXE"]), false);
        assert_eq!(p.sources.len(), 1);
    }

    #[test]
    fn at_most_six_tracks_with_a_warning() {
        let p = plan(
            true,
            true,
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
