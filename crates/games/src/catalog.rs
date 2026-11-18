//! The known-games list: Discord's detectable applications, cached on disk, plus a built-in fallback.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const DETECTABLE_URL: &str = "https://discord.com/api/v9/applications/detectable";
/// The cache is refreshed when older than this.
pub const MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// Lower-case exe file name -> game name.
pub type GameMap = HashMap<String, String>;

/// Lower-case exe file name -> the game's icon on Discord's CDN (for Rich Presence).
pub type IconMap = HashMap<String, String>;

#[derive(Deserialize)]
struct App {
    name: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    icon_hash: Option<String>,
    #[serde(default)]
    executables: Vec<Exe>,
}

#[derive(Deserialize)]
struct Exe {
    #[serde(default)]
    os: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    is_launcher: bool,
}

/// What is stored in `games.json`.
#[derive(Serialize, Deserialize)]
pub struct Cache {
    /// Unix seconds of the download.
    pub fetched_at: u64,
    pub games: GameMap,
    /// Missing in caches from before icons were kept: those are refreshed right away.
    #[serde(default)]
    pub icons: IconMap,
}

/// Last path component of a Discord executable name, lower-cased (`steamapps/x/Foo.exe` -> `foo.exe`).
fn exe_file_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .to_ascii_lowercase()
}

/// Parses Discord's JSON, keeping win32 non-launcher executables.
#[cfg(test)]
pub fn parse_detectable(json: &[u8]) -> Result<(GameMap, IconMap)> {
    let apps: Vec<App> = serde_json::from_slice(json).context("the game list has an unexpected format")?;
    Ok(collect(apps))
}

/// The icon of a Discord application, if both parts look right.
fn icon_url(id: &str, hash: Option<&str>) -> Option<String> {
    let hash = hash?;
    let ok = !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && !hash.is_empty()
        && hash.bytes().all(|b| b.is_ascii_hexdigit() || b == b'_');
    ok.then(|| format!("https://cdn.discordapp.com/app-icons/{id}/{hash}.png?size=256"))
}

fn collect(apps: Vec<App>) -> (GameMap, IconMap) {
    let mut map = GameMap::new();
    let mut icons = IconMap::new();
    for app in apps {
        let icon = icon_url(&app.id, app.icon_hash.as_deref());
        for exe in app.executables {
            if exe.os != "win32" || exe.is_launcher {
                continue;
            }
            // A leading `>` marks an exact-path match in Discord's list; we only match on the file name.
            let file = exe_file_name(exe.name.trim_start_matches('>'));
            if file.is_empty() {
                continue;
            }
            if let Some(icon) = &icon {
                icons.entry(file.clone()).or_insert_with(|| icon.clone());
            }
            map.entry(file).or_insert_with(|| app.name.clone());
        }
    }
    (map, icons)
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// True if a download made at `fetched_at` is still fresh at `now`. A timestamp in the future is stale.
pub fn is_fresh(fetched_at: u64, now: u64) -> bool {
    now >= fetched_at && now - fetched_at < MAX_AGE_SECS
}

pub fn load_cache(path: &Path) -> Option<Cache> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("ignoring unreadable game cache {}: {e}", path.display());
            None
        }
    }
}

pub fn save_cache(path: &Path, cache: &Cache) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(cache)?).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Downloads the list. Sends only a User-Agent of `clipr/<version>`.
pub fn download() -> Result<(GameMap, IconMap)> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(format!("clipr/{}", clipr_core::VERSION))
        .build()
        .into();
    let mut response = agent
        .get(DETECTABLE_URL)
        .call()
        .context("could not download the list of known games")?;
    let apps: Vec<App> = response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_json()
        .context("could not read the list of known games")?;
    Ok(collect(apps))
}

/// Games that need no download. Only executables that are specific to the game.
pub fn builtin() -> GameMap {
    const GAMES: &[(&str, &str)] = &[
        ("valorant-win64-shipping.exe", "VALORANT"),
        ("cs2.exe", "Counter-Strike 2"),
        ("fortniteclient-win64-shipping.exe", "Fortnite"),
        ("r5apex.exe", "Apex Legends"),
        ("league of legends.exe", "League of Legends"),
        ("overwatch.exe", "Overwatch 2"),
        ("rocketleague.exe", "Rocket League"),
        ("gta5.exe", "Grand Theft Auto V"),
        ("eldenring.exe", "ELDEN RING"),
        ("rainbowsix.exe", "Tom Clancy's Rainbow Six Siege"),
        ("rainbowsix_vulkan.exe", "Tom Clancy's Rainbow Six Siege"),
        ("escapefromtarkov.exe", "Escape from Tarkov"),
        ("dota2.exe", "Dota 2"),
        ("tslgame.exe", "PUBG: BATTLEGROUNDS"),
        ("cod.exe", "Call of Duty"),
        ("modernwarfare.exe", "Call of Duty: Modern Warfare"),
        ("marvel-win64-shipping.exe", "Marvel Rivals"),
        ("project8.exe", "Deadlock"),
        ("destiny2.exe", "Destiny 2"),
        ("cyberpunk2077.exe", "Cyberpunk 2077"),
        ("witcher3.exe", "The Witcher 3: Wild Hunt"),
        ("bg3.exe", "Baldur's Gate 3"),
        ("bg3_dx11.exe", "Baldur's Gate 3"),
        ("helldivers2.exe", "HELLDIVERS 2"),
        ("palworld-win64-shipping.exe", "Palworld"),
        ("hogwartslegacy.exe", "Hogwarts Legacy"),
        ("terraria.exe", "Terraria"),
        ("stardewvalley.exe", "Stardew Valley"),
        ("thefinals.exe", "THE FINALS"),
        ("hollowknight.exe", "Hollow Knight"),
        ("sekiro.exe", "Sekiro: Shadows Die Twice"),
        ("rdr2.exe", "Red Dead Redemption 2"),
        ("starfield.exe", "Starfield"),
    ];
    GAMES
        .iter()
        .map(|(e, n)| (e.to_string(), n.to_string()))
        .collect()
}

/// Common programs that Discord (or users) list but that are not games.
const DENY: &[&str] = &[
    "explorer.exe",
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
    "discord.exe",
    "discordptb.exe",
    "discordcanary.exe",
    "obs64.exe",
    "obs32.exe",
    "obs.exe",
    "steam.exe",
    "steamwebhelper.exe",
    "epicgameslauncher.exe",
    "galaxyclient.exe",
    "battle.net.exe",
    "origin.exe",
    "eadesktop.exe",
    "ubisoftconnect.exe",
    "upc.exe",
    "code.exe",
    "devenv.exe",
    "spotify.exe",
    "clipr.exe",
    "teams.exe",
    "slack.exe",
    "zoom.exe",
    "vlc.exe",
    "notepad.exe",
    "cmd.exe",
    "powershell.exe",
    "windowsterminal.exe",
    "applicationframehost.exe",
    "searchhost.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "taskmgr.exe",
    "javaw.exe",
    "java.exe",
];

pub fn is_denied(exe: &str) -> bool {
    DENY.contains(&exe)
}
