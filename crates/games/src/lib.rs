//! Game detection: Discord's detectable-games list, foreground tracking, custom and block lists.
//!
//! [`GameWatcher`] answers one question, "which game should be captured right now?". The known-games
//! list comes from Discord's public `applications/detectable` endpoint, cached on disk and refreshed in
//! the background, with a small built-in list until the first download. The matching logic is plain
//! Rust; only [`win`] touches Win32.

#![cfg(windows)]

mod catalog;
mod win;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use clipr_core::config::GamesConfig;

use catalog::GameMap;

/// A game that is running and should be captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Game {
    pub name: String,
    /// Lower-case exe file name, e.g. `cs2.exe`.
    pub exe: String,
    pub pid: u32,
    pub hwnd: isize,
    /// HMONITOR of the game's window.
    pub monitor: isize,
    /// The user asked for a full recording whenever this game runs.
    pub auto_record: bool,
}

#[derive(Default)]
struct Lists {
    custom: HashSet<String>,
    blocked: HashSet<String>,
    auto_record: HashSet<String>,
}

impl Lists {
    fn from_config(config: &GamesConfig) -> Self {
        let set = |v: &[String]| v.iter().map(|s| s.trim().to_ascii_lowercase()).collect();
        Self {
            custom: set(&config.custom),
            blocked: set(&config.blocked),
            auto_record: set(&config.auto_record),
        }
    }
}

struct State {
    known: GameMap,
    lists: Lists,
    current: Option<Game>,
    /// Fullscreen unknown exes already logged.
    logged: HashSet<String>,
}

/// Watches the foreground window for known games. Cheap to share: all methods take `&self`.
pub struct GameWatcher {
    state: Arc<Mutex<State>>,
}

/// How a window's exe relates to the lists. Precedence: blocked, custom, built-in deny list, known games.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    No,
    /// A known game; carries Discord's name.
    Known(String),
    /// Listed in `custom` and not known by name.
    Custom,
}

fn classify(exe: &str, known: &GameMap, lists: &Lists) -> Verdict {
    if lists.blocked.contains(exe) {
        return Verdict::No;
    }
    if lists.custom.contains(exe) {
        return known
            .get(exe)
            .map_or(Verdict::Custom, |n| Verdict::Known(n.clone()));
    }
    if catalog::is_denied(exe) {
        return Verdict::No;
    }
    known.get(exe).map_or(Verdict::No, |n| Verdict::Known(n.clone()))
}

impl GameWatcher {
    /// Loads the cached list and, if it is missing or older than 24 h, refreshes it on a background
    /// thread. Never blocks on the network.
    pub fn new(cache_file: PathBuf) -> Self {
        let cache = catalog::load_cache(&cache_file);
        let now = catalog::now_secs();
        // How long until the cached list is due for a refresh (zero: now).
        let due_in = cache
            .as_ref()
            .filter(|c| catalog::is_fresh(c.fetched_at, now))
            .map_or(0, |c| catalog::MAX_AGE_SECS - (now - c.fetched_at));
        let known = match cache {
            Some(c) if !c.games.is_empty() => c.games,
            _ => catalog::builtin(),
        };
        tracing::info!("{} known game executables loaded", known.len());
        let state = Arc::new(Mutex::new(State {
            known,
            lists: Lists::default(),
            current: None,
            logged: HashSet::new(),
        }));
        let refresher = Arc::clone(&state);
        let spawned = std::thread::Builder::new()
            .name("games-refresh".into())
            .spawn(move || refresh_forever(&refresher, &cache_file, due_in));
        if let Err(e) = spawned {
            tracing::warn!("could not start the game list refresh: {e}");
        }
        Self { state }
    }

    pub fn set_config(&self, config: &GamesConfig) {
        lock(&self.state).lists = Lists::from_config(config);
    }

    /// The game to capture right now, if any. Call every ~2 s.
    pub fn poll(&self) -> Option<Game> {
        self.poll_with(win::foreground(), win::process_exe, win::monitor_of)
    }

    /// How many executables are known (Discord list or built-in fallback).
    pub fn list_len(&self) -> usize {
        lock(&self.state).known.len()
    }

    fn poll_with(
        &self,
        foreground: Option<win::Foreground>,
        process_exe: impl Fn(u32) -> Option<String>,
        monitor_of: impl Fn(isize) -> isize,
    ) -> Option<Game> {
        let mut st = lock(&self.state);
        if let Some(fg) = foreground {
            let verdict = classify(&fg.exe, &st.known, &st.lists);
            let name = match verdict {
                Verdict::Known(name) => Some(name),
                Verdict::Custom => Some(if fg.title.trim().is_empty() {
                    fg.exe.strip_suffix(".exe").unwrap_or(&fg.exe).to_string()
                } else {
                    fg.title.trim().to_string()
                }),
                Verdict::No => None,
            };
            if let Some(name) = name {
                let game = Game {
                    name,
                    auto_record: st.lists.auto_record.contains(&fg.exe),
                    monitor: monitor_of(fg.hwnd),
                    exe: fg.exe,
                    pid: fg.pid,
                    hwnd: fg.hwnd,
                };
                if st.current.as_ref().map(|g| (&g.exe, g.pid)) != Some((&game.exe, game.pid)) {
                    tracing::info!("game detected: {} ({}, pid {})", game.name, game.exe, game.pid);
                }
                st.current = Some(game.clone());
                return Some(game);
            }
            if fg.fullscreen
                && !st.lists.blocked.contains(&fg.exe)
                && !catalog::is_denied(&fg.exe)
                && st.logged.insert(fg.exe.clone())
            {
                tracing::debug!(
                    "fullscreen window of unknown program {} ({:?}); add it to the custom games to record it",
                    fg.exe,
                    fg.title
                );
            }
        }
        // Not in the foreground: keep the last game while its process lives.
        if let Some(prev) = st.current.take() {
            let still_game = process_exe(prev.pid).is_some_and(|exe| exe == prev.exe)
                && classify(&prev.exe, &st.known, &st.lists) != Verdict::No;
            if still_game {
                st.current = Some(prev.clone());
                return Some(prev);
            }
            tracing::info!("game ended: {}", prev.name);
        }
        None
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keeps the list fresh while clipr runs: daily, and an hour after a failed download.
fn refresh_forever(state: &Mutex<State>, cache_file: &std::path::Path, mut due_in: u64) {
    const RETRY_SECS: u64 = 60 * 60;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(due_in));
        due_in = if refresh(state, cache_file) {
            catalog::MAX_AGE_SECS
        } else {
            RETRY_SECS
        };
    }
}

/// Downloads the list once. True when a new list was loaded.
fn refresh(state: &Mutex<State>, cache_file: &std::path::Path) -> bool {
    match catalog::download() {
        Ok(games) if !games.is_empty() => {
            tracing::info!("downloaded {} known game executables", games.len());
            let cache = catalog::Cache {
                fetched_at: catalog::now_secs(),
                games,
            };
            if let Err(e) = catalog::save_cache(cache_file, &cache) {
                tracing::warn!("could not save the game list: {e:#}");
            }
            lock(state).known = cache.games;
            true
        }
        Ok(_) => {
            tracing::warn!("the downloaded game list was empty; keeping the old one");
            false
        }
        Err(e) => {
            tracing::warn!("{e:#}; using the cached game list");
            false
        }
    }
}

/// Makes a game name safe as a Windows folder or file name. Never empty.
pub fn safe_folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = collapsed.chars().take(60).collect();
    let trimmed = truncated.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        return "Desktop".to_string();
    }
    // Reserved device names cannot be folder names on Windows.
    let stem = trimmed.split('.').next().unwrap_or(trimmed).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit()));
    if reserved {
        // The underscore goes before the extension: "CON.txt" -> "CON_.txt".
        match trimmed.find('.') {
            Some(dot) => format!("{}_{}", &trimmed[..dot], &trimmed[dot..]),
            None => format!("{trimmed}_"),
        }
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[
      {"name":"Foo Game","executables":[
        {"os":"win32","name":"steamapps/common/Foo/Foo.exe","is_launcher":false},
        {"os":"win32","name":"FooLauncher.exe","is_launcher":true},
        {"os":"linux","name":"foo","is_launcher":false},
        {"os":"darwin","name":"Foo.app","is_launcher":false}]},
      {"name":"Bar","executables":[{"os":"win32","name":">bar-win64.exe","is_launcher":false}]},
      {"name":"No Exes"},
      {"name":"Browser","executables":[{"os":"win32","name":"chrome.exe","is_launcher":false}]}
    ]"#;

    fn known() -> GameMap {
        catalog::parse_detectable(FIXTURE.as_bytes()).unwrap()
    }

    fn lists(custom: &[&str], blocked: &[&str], auto: &[&str]) -> Lists {
        let v = |s: &[&str]| s.iter().map(|s| s.to_string()).collect();
        Lists::from_config(&GamesConfig {
            custom: v(custom),
            blocked: v(blocked),
            auto_record: v(auto),
            ..Default::default()
        })
    }

    #[test]
    fn parses_discord_json() {
        let m = known();
        assert_eq!(m.get("foo.exe").map(String::as_str), Some("Foo Game"));
        assert_eq!(m.get("bar-win64.exe").map(String::as_str), Some("Bar"));
        assert!(!m.contains_key("foolauncher.exe"), "launchers are skipped");
        assert!(
            !m.contains_key("foo") && !m.contains_key("foo.app"),
            "non-win32 skipped"
        );
        assert!(catalog::parse_detectable(b"{}").is_err());
    }

    #[test]
    fn cache_freshness() {
        let day = catalog::MAX_AGE_SECS;
        assert!(catalog::is_fresh(1000, 1000 + day - 1));
        assert!(!catalog::is_fresh(1000, 1000 + day));
        assert!(!catalog::is_fresh(2000, 1000), "future timestamps are stale");
    }

    #[test]
    fn cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("games.json");
        assert!(catalog::load_cache(&path).is_none());
        catalog::save_cache(
            &path,
            &catalog::Cache {
                fetched_at: 42,
                games: known(),
            },
        )
        .unwrap();
        let c = catalog::load_cache(&path).unwrap();
        assert_eq!(c.fetched_at, 42);
        assert_eq!(c.games, known());
        std::fs::write(&path, "garbage").unwrap();
        assert!(catalog::load_cache(&path).is_none());
    }

    #[test]
    fn precedence() {
        let k = known();
        let none = lists(&[], &[], &[]);
        assert_eq!(classify("foo.exe", &k, &none), Verdict::Known("Foo Game".into()));
        assert_eq!(
            classify("chrome.exe", &k, &none),
            Verdict::No,
            "deny list beats Discord"
        );
        assert_eq!(classify("unknown.exe", &k, &none), Verdict::No);
        let custom = lists(&["unknown.exe", "chrome.exe", "foo.exe"], &[], &[]);
        assert_eq!(classify("unknown.exe", &k, &custom), Verdict::Custom);
        assert_eq!(
            classify("chrome.exe", &k, &custom),
            Verdict::Known("Browser".into())
        );
        assert_eq!(
            classify("foo.exe", &k, &custom),
            Verdict::Known("Foo Game".into())
        );
        let both = lists(&["unknown.exe"], &["unknown.exe", "foo.exe"], &[]);
        assert_eq!(
            classify("unknown.exe", &k, &both),
            Verdict::No,
            "blocked beats custom"
        );
        assert_eq!(
            classify("foo.exe", &k, &both),
            Verdict::No,
            "blocked beats Discord"
        );
    }

    fn watcher(lists: Lists) -> GameWatcher {
        GameWatcher {
            state: Arc::new(Mutex::new(State {
                known: known(),
                lists,
                current: None,
                logged: HashSet::new(),
            })),
        }
    }

    fn fg(exe: &str, pid: u32, title: &str, fullscreen: bool) -> Option<win::Foreground> {
        Some(win::Foreground {
            hwnd: 7,
            pid,
            exe: exe.into(),
            title: title.into(),
            fullscreen,
        })
    }

    #[test]
    fn poll_keeps_game_after_alt_tab() {
        let w = watcher(lists(&["mine.exe"], &[], &["foo.exe"]));
        let g = w
            .poll_with(fg("foo.exe", 10, "t", true), |_| None, |_| 99)
            .unwrap();
        assert_eq!(
            (g.name.as_str(), g.pid, g.monitor, g.auto_record),
            ("Foo Game", 10, 99, true)
        );
        // Alt-tab to a non-game while the process lives.
        let alive = |pid: u32| (pid == 10).then(|| "foo.exe".to_string());
        assert_eq!(
            w.poll_with(fg("notepad.exe", 5, "", false), alive, |_| 1),
            Some(g.clone())
        );
        assert_eq!(w.poll_with(None, alive, |_| 1), Some(g.clone()));
        // Pid reused by another exe, or gone: the game ended.
        let reused = |_: u32| Some("other.exe".to_string());
        assert_eq!(w.poll_with(fg("notepad.exe", 5, "", false), reused, |_| 1), None);
        assert_eq!(w.poll_with(None, alive, |_| 1), None);
    }

    #[test]
    fn poll_custom_name_and_unknown_fullscreen() {
        let w = watcher(lists(&["mine.exe"], &[], &[]));
        let g = w
            .poll_with(fg("mine.exe", 3, "My Game - Level 1", false), |_| None, |_| 1)
            .unwrap();
        assert_eq!(g.name, "My Game - Level 1");
        assert!(!g.auto_record);
        let w = watcher(lists(&["mine.exe"], &[], &[]));
        let g = w
            .poll_with(fg("mine.exe", 3, " ", false), |_| None, |_| 1)
            .unwrap();
        assert_eq!(g.name, "mine");
        // Unknown fullscreen exes are not detected, only logged once.
        assert_eq!(
            w.poll_with(fg("mystery.exe", 4, "x", true), |_| None, |_| 1),
            None
        );
        assert!(w.state.lock().unwrap().logged.contains("mystery.exe"));
    }

    #[test]
    fn config_applies() {
        let w = watcher(Lists::default());
        w.set_config(&GamesConfig {
            blocked: vec!["FOO.EXE".into()],
            ..Default::default()
        });
        assert_eq!(w.poll_with(fg("foo.exe", 1, "", false), |_| None, |_| 1), None);
        assert_eq!(w.list_len(), known().len());
    }

    #[test]
    fn folder_names() {
        assert_eq!(safe_folder_name("Counter-Strike 2"), "Counter-Strike 2");
        assert_eq!(
            safe_folder_name("Tom Clancy's: R6 / Siege?"),
            "Tom Clancy's R6 Siege"
        );
        assert_eq!(safe_folder_name("  ..a   b..  "), "a b");
        assert_eq!(safe_folder_name("a\u{7}b\tc"), "a b c");
        assert_eq!(safe_folder_name(""), "Desktop");
        assert_eq!(safe_folder_name("<>:\"/\\|?*"), "Desktop");
        assert_eq!(safe_folder_name(" . "), "Desktop");
        assert_eq!(safe_folder_name(&"x".repeat(100)).chars().count(), 60);
        assert_eq!(safe_folder_name("con"), "con_");
        assert_eq!(safe_folder_name("COM1"), "COM1_");
    }

    #[test]
    fn builtin_is_sane() {
        let b = catalog::builtin();
        assert!(b.len() >= 30);
        assert!(
            b.keys()
                .all(|k| k == &k.to_ascii_lowercase() && !catalog::is_denied(k))
        );
    }
}
