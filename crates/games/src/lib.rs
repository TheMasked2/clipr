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
const LIBRARIES: [&str; 11] = [
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
    "\\battle.net\\games\\",
];

/// Folders in game libraries that hold tools, not games.
const NOT_GAMES: [&str; 5] = [
    "riot client",
    "steamworks shared",
    "steam controller configs",
    "directx",
    "launcher",
];

/// Parts of exe names that are never the game itself (crash reporters, installers, anti-cheat).
const NOT_GAME_EXES: [&str; 7] = [
    "crash",
    "unins",
    "setup",
    "redist",
    "install",
    "anticheat",
    "bugreport",
];

/// The game's name from its install folder, when `path` is in a game library folder.
fn library_game(path: &str) -> Option<String> {
    let lower = path.replace('/', "\\").to_ascii_lowercase();
    let exe = lower.rsplit('\\').next().unwrap_or("");
    if NOT_GAME_EXES.iter().any(|n| exe.contains(n)) {
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

/// The game's name when a custom entry names it: the window title or the install folder contains the entry.
fn named_game(names: &[String], title: &str, path: &str) -> Option<String> {
    let title_l = title.to_lowercase();
    let folder = library_game(path);
    let folder_l = folder.as_deref().map(str::to_lowercase).unwrap_or_default();
    names
        .iter()
        .find(|n| title_l.contains(n.as_str()) || folder_l.contains(n.as_str()))?;
    Some(folder.unwrap_or_else(|| title.trim().to_string()))
}

struct State {
    known: GameMap,
    icons: IconMap,
    lists: Lists,
    current: Option<Game>,
    /// Fullscreen unknown exes already logged.
    logged: HashSet<String>,
    /// Games found this run without being on a list (a library folder, a custom name): exe → name, so they
    /// stay games while they run in the background. Cleared when the settings change.
    found: HashMap<String, String>,
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
        // Found again on the next poll under the new rules (a game the user just blocked isn't).
        st.found.clear();
        // An unlisted program that is a game now (added or found) leaves the list on its next poll.
        let lists = &st.lists;
        let keep: VecDeque<Unlisted> = st
            .unlisted
            .iter()
            .filter(|u| !lists.custom.contains(&u.exe))
            .cloned()
            .collect();
        st.unlisted = keep;
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
                    if let Some(name) = st.found.get(&fg.exe) {
                        Some(name.clone())
                    } else if let Some(name) = named_game(&st.lists.custom_names, &fg.title, &fg.path) {
                        // Named after the window title (which can be private) unless the folder named it.
                        custom = library_game(&fg.path).is_none();
                        tracing::info!("{} is a game by its name in the custom games", fg.exe);
                        st.found.insert(fg.exe.clone(), name.clone());
                        Some(name)
                    } else if st.lists.detect_unlisted
                        && fg.fullscreen
                        && let Some(name) = library_game(&fg.path)
                    {
                        tracing::info!(
                            "{} isn't on the game list, but runs fullscreen from a game library",
                            fg.exe
                        );
                        st.found.insert(fg.exe.clone(), name.clone());
                        Some(name)
                    } else {
                        None
                    }
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
                st.current = Some(game.clone());
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
            let still_game = process_exe(prev.pid).is_some_and(|exe| exe == prev.exe)
                && (classify(&prev.exe, &st.known, &st.lists) != Verdict::No
                    || st.found.contains_key(&prev.exe));
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
        let w = watcher(lists(&["trust your gut", "a"], &[], &[]));
        // Anywhere, windowed too: the user named it.
        let g = w
            .poll_with(
                fg_at("C:\\Games\\TYG\\tyg.exe", 9, "Trust Your Gut v0.3", false),
                |_| None,
                |_| 1,
            )
            .unwrap();
        assert_eq!((g.name.as_str(), g.custom), ("Trust Your Gut v0.3", true));
        // A one-letter entry doesn't make everything a game.
        assert_eq!(
            w.poll_with(fg("notepad.exe", 3, "a file", false), |_| None, |_| 1),
            None
        );
    }
}
