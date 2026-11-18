//! Game detection: Discord's detectable-games list, foreground tracking, custom and block lists.
//!
//! [`GameWatcher`] answers one question, "which game should be captured right now?". The known-games
//! list comes from Discord's public `applications/detectable` endpoint, cached on disk and refreshed in
//! the background, with a small built-in list until the first download. The matching logic is plain
//! Rust; only [`win`] touches Win32.

#![cfg(windows)]

mod catalog;
mod win;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use clipr_core::config::GamesConfig;

use catalog::{GameMap, IconMap};

/// A game that is running and should be captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Game {
    pub name: String,
    /// Lower-case exe file name, e.g. `cs2.exe`.
    pub exe: String,
    pub pid: u32,
    /// The game's icon on Discord's CDN, when Discord knows the game.
    pub icon: Option<String>,
    /// From the user's custom list, not Discord's: `name` is then the window title, which can be private.
    pub custom: bool,
    pub hwnd: isize,
    /// HMONITOR of the game's window.
    pub monitor: isize,
    /// The user asked for a full recording whenever this game runs.
    pub auto_record: bool,
}

#[derive(Default)]
struct Lists {
    custom: HashSet<String>,
    /// Custom entries that are a game's name, not an exe (`trust your gut`), lower case.
    custom_names: Vec<String>,
    blocked: HashSet<String>,
    auto_record: HashSet<String>,
    /// Fullscreen programs from game library folders count as games (`games.detect_unlisted`).
    detect_unlisted: bool,
}

impl Lists {
    fn from_config(config: &GamesConfig) -> Self {
        let set = |v: &[String]| v.iter().map(|s| s.trim().to_ascii_lowercase()).collect();
        Self {
            custom: set(&config.custom),
            // Three letters at least, so a stray "a" doesn't make every window a game.
            custom_names: config
                .custom
                .iter()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.ends_with(".exe") && s.chars().count() >= 3)
                .collect(),
            blocked: set(&config.blocked),
            auto_record: set(&config.auto_record),
            detect_unlisted: config.detect_unlisted,
        }
    }

    /// Never a game: blocked by the user, or on the built-in list of programs that aren't games.
    fn excluded(&self, exe: &str) -> bool {
        self.blocked.contains(exe) || catalog::is_denied(exe)
    }

    /// Whether a game found by `rule` still counts under these settings (the user may have blocked it, turned
    /// "find new games" off or removed the name).
    fn allows(&self, exe: &str, rule: &Rule) -> bool {
        !self.excluded(exe)
            && match rule {
                Rule::Library => self.detect_unlisted,
                Rule::Name(n) => self.custom_names.contains(n),
            }
    }
}

/// Why a program that isn't on a list is a game.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Rule {
    /// It ran fullscreen from a game library folder (`games.detect_unlisted`).
    Library,
    /// A custom entry is its name (lower case).
    Name(String),
}

/// A game found without being on a list, remembered by its full path so it stays a game while it runs in the
/// background or windowed.
#[derive(Clone, Debug)]
struct Found {
    name: String,
    /// `name` is a window title (which can be private), not a folder.
    custom: bool,
    rule: Rule,
}

/// A fullscreen program clipr saw but didn't record (not on the list), for "add it as a game".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unlisted {
    /// Lower-case exe file name.
    pub exe: String,
    pub title: String,
}

/// How many recently seen unlisted programs are kept.
const UNLISTED_KEEP: usize = 8;

/// Folders games are installed in by their stores, lower case with backslashes. The game's name is the folder
/// right after one of these (`steamapps\common\Hades II\...` → `Hades II`).
const LIBRARIES: [&str; 10] = [
    "\\steamapps\\common\\",
    "\\epic games\\",
    "\\xboxgames\\",
    "\\riot games\\",
    "\\gog galaxy\\games\\",
    "\\gog games\\",
    "\\ea games\\",
    "\\ubisoft game launcher\\games\\",
    "\\amazon games\\library\\",
    "\\itch\\apps\\",
];

/// Folders in game libraries that hold tools, not games.
const NOT_GAMES: [&str; 10] = [
    "riot client",
    "steamworks shared",
    "steam controller configs",
    "steamvr",
    "wallpaper_engine",
    "wallpaper engine",
    "epic online services",
    "directx",
    "launcher",
    "launcher.ea",
];

/// Parts of exe names that are never the game itself: crash reporters, installers, launchers, helpers,
/// overlays and anti-cheat. Specific enough not to hit games (`crashbandicoot.exe` is a game).
const NOT_GAME_EXES: [&str; 17] = [
    "crashhandler",
    "crashreport",
    "crashpad",
    "crash_report",
    "bugreport",
    "unins0",
    "redist",
    "installer",
    "anticheat",
    "battleye",
    "launcher",
    "helper",
    "overlay",
    "service",
    "updater",
    "dxsetup",
    "uploader",
];

/// An exe that is never the game itself (see [`NOT_GAME_EXES`]), or an installer (`setup.exe`, `xsetup.exe`).
fn not_game_exe(exe: &str) -> bool {
    NOT_GAME_EXES.iter().any(|n| exe.contains(n)) || exe.ends_with("setup.exe")
}

/// The game's name from its install folder, when `path` is in a game library folder.
fn library_game(path: &str) -> Option<String> {
    let lower = path.replace('/', "\\").to_ascii_lowercase();
    let exe = lower.rsplit('\\').next().unwrap_or("");
    if not_game_exe(exe) {
        return None;
    }
    let start = LIBRARIES
        .iter()
        .find_map(|lib| lower.find(lib).map(|i| i + lib.len()))?;
    let rest = &path[start..];
    let folder = rest.split(['\\', '/']).next()?.trim();
    // The exe has to be inside the game's folder, not the library folder itself.
    if folder.is_empty()
        || rest.len() <= folder.len()
        || NOT_GAMES.contains(&folder.to_ascii_lowercase().as_str())
    {
        return None;
    }
    Some(folder.to_string())
}

/// Whether a window title names the game `name` (lower case): the title is the name, or starts with it
/// followed by something that isn't part of a word (`Trust Your Gut v1.2`, `Trust Your Gut - DX12`), so
/// `ark` doesn't match "Dark Souls".
fn title_names(title: &str, name: &str) -> bool {
    let title = title.trim().to_lowercase();
    title
        .strip_prefix(name)
        .is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_alphanumeric()))
}

/// When a custom entry is the game's name: its install folder in a game library is that name, or it runs
/// fullscreen with a window title naming it (any window could have a matching title otherwise: a browser
/// tab, a document). The name, the entry and whether the name is the window title.
fn named_game(names: &[String], fg: &win::Foreground) -> Option<(String, String, bool)> {
    if let Some(folder) = library_game(&fg.path) {
        let folder_l = folder.to_lowercase();
        if let Some(n) = names.iter().find(|n| **n == folder_l) {
            return Some((folder, n.clone(), false));
        }
    }
    if !fg.fullscreen {
        return None;
    }
    let n = names.iter().find(|n| title_names(&fg.title, n))?;
    Some((fg.title.trim().to_string(), n.clone(), true))
}

struct State {
    known: GameMap,
    icons: IconMap,
    lists: Lists,
    current: Option<Game>,
    /// Fullscreen unknown exes already logged.
    logged: HashSet<String>,
    /// Games found this run without being on a list (a library folder, a custom name), by lower-case full
    /// path: they stay games while they run in the background or windowed. Settings changes re-check them.
    found: HashMap<String, Found>,
    /// Why `current` is a game when it isn't on a list, to re-check it while it runs in the background.
    current_rule: Option<Rule>,
    /// Fullscreen programs seen lately that aren't games to clipr, newest first.
    unlisted: VecDeque<Unlisted>,
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
            .filter(|c| catalog::is_fresh(c.fetched_at, now) && !c.icons.is_empty())
            .map_or(0, |c| catalog::MAX_AGE_SECS - (now - c.fetched_at));
        let (known, icons) = match cache {
            Some(c) if !c.games.is_empty() => (c.games, c.icons),
            _ => (catalog::builtin(), IconMap::new()),
        };
        tracing::info!("{} known game executables loaded", known.len());
        let state = Arc::new(Mutex::new(State {
            known,
            icons,
            lists: Lists::default(),
            current: None,
            logged: HashSet::new(),
            found: HashMap::new(),
            current_rule: None,
            unlisted: VecDeque::new(),
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
        let mut st = lock(&self.state);
        st.lists = Lists::from_config(config);
        // Found games stay games unless the new settings say otherwise (blocked, "find new games" off, name
        // removed): saving the settings mid-game doesn't end the game.
        let State {
            lists,
            found,
            unlisted,
            ..
        } = &mut *st;
        found.retain(|path, f| lists.allows(file_name(path), &f.rule));
        // An unlisted program the user added leaves the list now; one found otherwise on its next poll.
        unlisted.retain(|u| !lists.custom.contains(&u.exe));
    }

    /// Fullscreen programs seen lately that clipr didn't record, newest first (to add one as a game).
    pub fn unlisted(&self) -> Vec<Unlisted> {
        lock(&self.state).unlisted.iter().cloned().collect()
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
            let mut custom = verdict == Verdict::Custom;
            let mut rule = None;
            let path = fg.path.replace('/', "\\").to_lowercase();
            let name = match verdict {
                Verdict::Known(name) => Some(name),
                Verdict::Custom => Some(if fg.title.trim().is_empty() {
                    fg.exe.strip_suffix(".exe").unwrap_or(&fg.exe).to_string()
                } else {
                    fg.title.trim().to_string()
                }),
                Verdict::No if st.lists.excluded(&fg.exe) => None,
                // Not on any list: a custom entry naming it, or a fullscreen program from a game library.
                Verdict::No => {
                    let found = if let Some(f) = st.found.get(&path) {
                        Some(f.clone())
                    } else if let Some((name, entry, by_title)) = named_game(&st.lists.custom_names, &fg) {
                        tracing::info!("{} is a game by its name in the custom games", fg.exe);
                        Some(Found {
                            name,
                            custom: by_title,
                            rule: Rule::Name(entry),
                        })
                    } else if st.lists.detect_unlisted
                        && fg.fullscreen
                        && let Some(name) = library_game(&fg.path)
                    {
                        tracing::info!(
                            "{} isn't on the game list, but runs fullscreen from a game library",
                            fg.exe
                        );
                        Some(Found {
                            name,
                            custom: false,
                            rule: Rule::Library,
                        })
                    } else {
                        None
                    };
                    found.map(|f| {
                        custom = f.custom;
                        rule = Some(f.rule.clone());
                        let name = f.name.clone();
                        // Only by path, and not a title match: a window title can match by chance (a browser
                        // tab), which shouldn't make that program a game for good.
                        if !f.custom {
                            st.found.insert(path.clone(), f);
                        }
                        name
                    })
                }
            };
            if let Some(name) = name {
                let game = Game {
                    name,
                    icon: st.icons.get(&fg.exe).cloned(),
                    custom,
                    auto_record: st.lists.auto_record.contains(&fg.exe),
                    monitor: monitor_of(fg.hwnd),
                    exe: fg.exe,
                    pid: fg.pid,
                    hwnd: fg.hwnd,
                };
                if st.current.as_ref().map(|g| (&g.exe, g.pid)) != Some((&game.exe, game.pid)) {
                    tracing::info!("game detected: {} ({}, pid {})", game.name, game.exe, game.pid);
                }
                // A game now: not "seen lately, not recorded" any more.
                st.unlisted.retain(|u| u.exe != game.exe);
                st.current = Some(game.clone());
                st.current_rule = rule;
                return Some(game);
            }
            if fg.fullscreen && !st.lists.excluded(&fg.exe) {
                // Remember it for Settings → Games ("seen lately"), newest first.
                st.unlisted.retain(|u| u.exe != fg.exe);
                st.unlisted.push_front(Unlisted {
                    exe: fg.exe.clone(),
                    title: fg.title.trim().to_string(),
                });
                st.unlisted.truncate(UNLISTED_KEEP);
            }
            if fg.fullscreen && !st.lists.excluded(&fg.exe) && st.logged.insert(fg.exe.clone()) {
                tracing::debug!(
                    "fullscreen window of unknown program {} ({:?}); add it to the custom games to record it",
                    fg.exe,
                    fg.title
                );
            }
        }
        // Not in the foreground: keep the last game while its process lives.
        if let Some(prev) = st.current.take() {
            let rule = st.current_rule.take();
            let listed = classify(&prev.exe, &st.known, &st.lists) != Verdict::No;
            let found = rule.as_ref().is_some_and(|r| st.lists.allows(&prev.exe, r));
            let still_game = process_exe(prev.pid).is_some_and(|exe| exe == prev.exe) && (listed || found);
            if still_game {
                st.current = Some(prev.clone());
                st.current_rule = rule.filter(|_| !listed);
                return Some(prev);
            }
            tracing::info!("game ended: {}", prev.name);
        }
        None
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
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
        Ok((games, icons)) if !games.is_empty() => {
            tracing::info!("downloaded {} known game executables", games.len());
            let cache = catalog::Cache {
                fetched_at: catalog::now_secs(),
                games,
                icons,
            };
            if let Err(e) = catalog::save_cache(cache_file, &cache) {
                tracing::warn!("could not save the game list: {e:#}");
            }
            let mut st = lock(state);
            st.known = cache.games;
            st.icons = cache.icons;
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
/// The window to capture for a game: see [`win::main_window`]. `None` when it has no visible window.
pub fn main_window(pid: u32) -> Option<(isize, i64)> {
    win::main_window(pid)
}

/// Whether `hwnd` is still a visible window of `pid`, and its area (`i64::MAX` while minimised).
pub fn window_of(hwnd: isize, pid: u32) -> Option<i64> {
    win::window_of(hwnd, pid)
}

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
      {"name":"Foo Game","id":"123","icon_hash":"abc","executables":[
        {"os":"win32","name":"steamapps/common/Foo/Foo.exe","is_launcher":false},
        {"os":"win32","name":"FooLauncher.exe","is_launcher":true},
        {"os":"linux","name":"foo","is_launcher":false},
        {"os":"darwin","name":"Foo.app","is_launcher":false}]},
      {"name":"Bar","id":"9","icon_hash":"../x","executables":[{"os":"win32","name":">bar-win64.exe","is_launcher":false}]},
      {"name":"No Exes"},
      {"name":"Browser","executables":[{"os":"win32","name":"chrome.exe","is_launcher":false}]}
    ]"#;

    fn known() -> GameMap {
        catalog::parse_detectable(FIXTURE.as_bytes()).unwrap().0
    }

    #[test]
    fn icons_come_with_the_list() {
        let (_, icons) = catalog::parse_detectable(FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            icons.get("foo.exe").map(String::as_str),
            Some("https://cdn.discordapp.com/app-icons/123/abc.png?size=256")
        );
        // A hash that isn't one is left out.
        assert!(!icons.contains_key("bar-win64.exe"));
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
                icons: IconMap::from([(
                    "valorant.exe".into(),
                    "https://cdn.discordapp.com/app-icons/1/a.png".into(),
                )]),
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
                icons: catalog::parse_detectable(FIXTURE.as_bytes()).unwrap().1,
                lists,
                current: None,
                logged: HashSet::new(),
                found: HashMap::new(),
                current_rule: None,
                unlisted: VecDeque::new(),
            })),
        }
    }

    fn fg(exe: &str, pid: u32, title: &str, fullscreen: bool) -> Option<win::Foreground> {
        fg_at(&format!("C:\\Program Files\\Some\\{exe}"), pid, title, fullscreen)
    }

    fn fg_at(path: &str, pid: u32, title: &str, fullscreen: bool) -> Option<win::Foreground> {
        let exe = path.rsplit('\\').next().unwrap().to_ascii_lowercase();
        Some(win::Foreground {
            hwnd: 7,
            pid,
            path: path.into(),
            exe,
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

    #[test]
    fn library_folders_name_the_game() {
        let steam = "D:\\SteamLibrary\\steamapps\\common\\Hades II\\Ship\\Hades2.exe";
        assert_eq!(library_game(steam).as_deref(), Some("Hades II"));
        assert_eq!(
            library_game("C:\\XboxGames\\Starfield\\Content\\Starfield.exe").as_deref(),
            Some("Starfield")
        );
        assert_eq!(
            library_game("C:\\Program Files\\Epic Games\\Fortnite\\FortniteClient.exe").as_deref(),
            Some("Fortnite")
        );
        // Not games: other folders, tools in a library, crash reporters, an exe right in the library folder.
        assert_eq!(
            library_game("C:\\Program Files\\Mozilla Firefox\\firefox.exe"),
            None
        );
        assert_eq!(
            library_game("D:\\Steam\\steamapps\\common\\Steamworks Shared\\x.exe"),
            None
        );
        assert_eq!(
            library_game("D:\\Steam\\steamapps\\common\\Hades II\\CrashReporter.exe"),
            None
        );
        assert_eq!(library_game("C:\\XboxGames\\gamingservices.exe"), None);
        // "crash" in a game's own name is fine; a helper next to a game isn't the game.
        assert_eq!(
            library_game("D:\\Steam\\steamapps\\common\\Crash Bandicoot\\CrashBandicootNSaneTrilogy.exe")
                .as_deref(),
            Some("Crash Bandicoot")
        );
        assert_eq!(
            library_game("C:\\Program Files\\Epic Games\\Fortnite\\EpicWebHelper.exe"),
            None
        );
    }

    #[test]
    fn an_unlisted_fullscreen_game_from_a_library_is_recorded() {
        let w = watcher(lists(&[], &[], &[]));
        let path = "D:\\SteamLibrary\\steamapps\\common\\Trust Your Gut\\TrustYourGut.exe";
        // Windowed (a launcher, a menu in a window): not yet, but remembered as seen.
        assert_eq!(
            w.poll_with(fg_at(path, 5, "Trust Your Gut", false), |_| None, |_| 1),
            None
        );
        let g = w
            .poll_with(fg_at(path, 5, "Trust Your Gut", true), |_| None, |_| 1)
            .unwrap();
        assert_eq!(
            (g.name.as_str(), g.exe.as_str(), g.custom),
            ("Trust Your Gut", "trustyourgut.exe", false)
        );
        // Alt-tabbed away, it stays the game while it runs.
        let g = w
            .poll_with(
                fg("explorer.exe", 1, "", false),
                |_| Some("trustyourgut.exe".into()),
                |_| 1,
            )
            .unwrap();
        assert_eq!(g.name, "Trust Your Gut");
        // Saving the settings while it runs in the background doesn't end it.
        w.set_config(&GamesConfig::default());
        let alive = |_: u32| Some("trustyourgut.exe".to_string());
        assert!(
            w.poll_with(fg("explorer.exe", 1, "", false), alive, |_| 1)
                .is_some()
        );
        // Another program with the same exe name elsewhere isn't that game.
        assert_eq!(
            w.poll_with(
                fg_at("C:\\Tools\\TrustYourGut.exe", 6, "x", false),
                |_| None,
                |_| 1
            ),
            None
        );
        // Turning "find new games" off ends it.
        let off = GamesConfig {
            detect_unlisted: false,
            ..GamesConfig::default()
        };
        w.set_config(&off);
        assert_eq!(w.poll_with(fg("explorer.exe", 1, "", false), alive, |_| 1), None);
        // The switch off: not a game.
        let mut off = lists(&[], &[], &[]);
        off.detect_unlisted = false;
        let w = watcher(off);
        assert_eq!(
            w.poll_with(fg_at(path, 5, "Trust Your Gut", true), |_| None, |_| 1),
            None
        );
        assert_eq!(w.unlisted()[0].exe, "trustyourgut.exe");
        // Blocked wins.
        let w = watcher(lists(&[], &["trustyourgut.exe"], &[]));
        assert_eq!(
            w.poll_with(fg_at(path, 5, "Trust Your Gut", true), |_| None, |_| 1),
            None
        );
    }

    #[test]
    fn a_custom_entry_can_be_the_games_name() {
        let w = watcher(lists(&["trust your gut", "ark", "a"], &[], &[]));
        let tyg = "C:\\Games\\TYG\\tyg.exe";
        // A window title only names a game when it runs fullscreen: a browser tab or a document could match.
        assert_eq!(
            w.poll_with(fg_at(tyg, 9, "Trust Your Gut v0.3", false), |_| None, |_| 1),
            None
        );
        let g = w
            .poll_with(fg_at(tyg, 9, "Trust Your Gut v0.3", true), |_| None, |_| 1)
            .unwrap();
        assert_eq!((g.name.as_str(), g.custom), ("Trust Your Gut v0.3", true));
        // In the background it stays the game while it runs, still private (the title isn't shown).
        let alive = |_: u32| Some("tyg.exe".to_string());
        let g = w
            .poll_with(fg("explorer.exe", 1, "", false), alive, |_| 1)
            .unwrap();
        assert!(g.custom);
        // The name starts the title as whole words; a one-letter entry is ignored.
        let w = watcher(lists(&["trust your gut", "ark", "a"], &[], &[]));
        for title in ["Dark Souls", "Trust Your Gutsy recipes", "a file"] {
            assert_eq!(
                w.poll_with(fg("x.exe", 3, title, true), |_| None, |_| 1),
                None,
                "{title}"
            );
        }
        // The install folder in a game library names it too, windowed and all, and the name isn't private.
        let lib = "D:\\SteamLibrary\\steamapps\\common\\Trust Your Gut\\Game.exe";
        let mut l = lists(&["trust your gut"], &[], &[]);
        l.detect_unlisted = false;
        let g = watcher(l)
            .poll_with(fg_at(lib, 4, "UnityWindow", false), |_| None, |_| 1)
            .unwrap();
        assert_eq!((g.name.as_str(), g.custom), ("Trust Your Gut", false));
    }
}
