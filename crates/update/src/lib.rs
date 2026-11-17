//! Opt-in self-updater from GitHub releases. Only applies an update when no game runs and nothing is recording.
//!
//! This crate finds a newer release ([`check`]) and downloads its installer with a verified sha256
//! ([`download`]). When to check and when to apply is up to `clipr.exe` (see its `updater` module): it quits
//! and runs the installer with [`INSTALLER_ARGS`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Where releases come from.
pub const REPO: &str = "TheMasked2/clipr";

/// What the installer gets when clipr updates itself. `/UPDATE=1` makes it wait for clipr to exit, skip its
/// questions and start clipr again in the tray (see `installer/clipr.iss`). Inno reads `{param:UPDATE}` only
/// in the `/NAME=value` form.
pub const INSTALLER_ARGS: &[&str] = &["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/UPDATE=1"];

/// A bigger download than this is not ours.
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// Release notes past this many characters are cut (the UI links the release page for the rest).
const MAX_NOTES: usize = 4000;

/// Hosts a download may come from (GitHub redirects release assets to its object storage).
const DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// `sha256:<hex>`, set by GitHub for assets uploaded since mid-2025.
    #[serde(default)]
    digest: Option<String>,
}

/// A release newer than the running clipr.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Release {
    /// The version without the tag's `v`, e.g. `0.2.0` or `0.2.0-beta.1`.
    pub version: String,
    /// The release page on GitHub.
    pub page: String,
    /// Release notes (Markdown), cut at a few thousand characters.
    pub notes: String,
    /// When it was published (RFC 3339), if GitHub says.
    pub published: Option<String>,
    pub prerelease: bool,
    /// The installer (`clipr-<version>-setup.exe`).
    pub setup: Option<Asset>,
    /// The portable zip (`clipr-<version>-windows-x64.zip`).
    pub portable: Option<Asset>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// The expected sha256 (lowercase hex) when GitHub gave one.
    #[serde(skip)]
    pub sha256: Option<String>,
    /// A `<name>.sha256` asset to read the checksum from when GitHub gave none.
    #[serde(skip)]
    pub checksum_url: Option<String>,
}

/// Is this copy of clipr installed (rather than unzipped)? Inno Setup puts its uninstaller next to the exe;
/// only an installed copy can update itself.
pub fn is_installed(exe: &Path) -> bool {
    exe.parent().is_some_and(|dir| dir.join("unins000.exe").is_file())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(15)))
        .https_only(true)
        .user_agent(format!("clipr/{}", clipr_core::VERSION))
        .build()
        .into()
}

/// Asks GitHub for the newest release after `current`. Prereleases count only when `prereleases` is on.
/// `Ok(None)` means clipr is up to date. Blocking.
pub fn check(current: &str, prereleases: bool) -> Result<Option<Release>> {
    let current = parse_version(current).context("the running version isn't a version")?;
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=30");
    let mut response = agent(Duration::from_secs(30))
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| anyhow::anyhow!("can't reach GitHub: {e}"))?;
    let releases: Vec<GhRelease> = response
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_json()
        .context("GitHub sent a release list clipr can't read")?;
    Ok(newest(&releases, &current, prereleases))
}

/// `v1.2.3` or `1.2.3-beta.1`.
fn parse_version(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches(['v', 'V'])).ok()
}

/// The newest release above `current`, skipping drafts and (unless allowed) prereleases.
fn newest(releases: &[GhRelease], current: &Version, prereleases: bool) -> Option<Release> {
    let (version, release) = releases
        .iter()
        .filter(|r| !r.draft)
        .filter_map(|r| Some((parse_version(&r.tag_name)?, r)))
        .filter(|(v, r)| prereleases || (!r.prerelease && v.pre.is_empty()))
        .filter(|(v, _)| v > current)
        .max_by(|a, b| a.0.cmp(&b.0))?;
    let find = |suffix: &str| {
        let asset = release
            .assets
            .iter()
            .find(|a| a.name.to_ascii_lowercase().ends_with(suffix) && safe_name(&a.name))?;
        let checksum_name = format!("{}.sha256", asset.name);
        let checksum_url = release
            .assets
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&checksum_name))
            .map(|c| c.browser_download_url.clone());
        Some(Asset {
            name: asset.name.clone(),
            url: asset.browser_download_url.clone(),
            size: asset.size,
            sha256: asset.digest.as_deref().and_then(parse_digest),
            checksum_url,
        })
    };
    let mut notes = release.body.clone().unwrap_or_default();
    if let Some((cut, _)) = notes.char_indices().nth(MAX_NOTES) {
        notes.truncate(cut);
        notes.push_str("\n\n…");
    }
    Some(Release {
        version: version.to_string(),
        page: release.html_url.clone(),
        notes,
        published: release.published_at.clone(),
        prerelease: release.prerelease || !version.pre.is_empty(),
        setup: find("-setup.exe"),
        portable: find("-windows-x64.zip"),
    })
}

/// A file name the server may choose: no folders, no surprises.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `sha256:<64 hex>` → the lowercase hex.
fn parse_digest(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    is_sha256(hex).then(|| hex.to_ascii_lowercase())
}

/// A `.sha256` file: `<hex>` or `<hex>  <file name>` (sha256sum style).
fn parse_checksum_file(text: &str) -> Option<String> {
    let hex = text.split_whitespace().next()?;
    is_sha256(hex).then(|| hex.to_ascii_lowercase())
}

fn is_sha256(hex: &str) -> bool {
    hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The sha256 of a file, as lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

fn check_download_url(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .context("a download must use https")?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if !DOWNLOAD_HOSTS.contains(&host.to_ascii_lowercase().as_str()) {
        bail!("clipr only downloads updates from GitHub, not from {host}");
    }
    Ok(())
}

/// Downloads `asset` into `dir` and checks its sha256 (from GitHub, else from its `.sha256` asset; with
/// neither it refuses). Returns the verified file. `progress(done, total)` follows the download. A file
/// already there with the right checksum is reused. Blocking.
pub fn download(asset: &Asset, dir: &Path, progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    if !safe_name(&asset.name) {
        bail!("the release has a file with a strange name: {}", asset.name);
    }
    if asset.size > MAX_DOWNLOAD {
        bail!("the update is too big ({} MB)", asset.size / (1024 * 1024));
    }
    check_download_url(&asset.url)?;
    let expected = match (&asset.sha256, &asset.checksum_url) {
        (Some(sha), _) => sha.clone(),
        (None, Some(url)) => {
            check_download_url(url)?;
            let mut response = agent(Duration::from_secs(30))
                .get(url)
                .call()
                .map_err(|e| anyhow::anyhow!("can't download the checksum: {e}"))?;
            let text = response
                .body_mut()
                .with_config()
                .limit(4096)
                .read_to_string()
                .context("can't read the checksum")?;
            parse_checksum_file(&text).context("the checksum file isn't a sha256")?
        }
        (None, None) => bail!(
            "the release has no checksum for {}, so clipr won't install it; download it yourself",
            asset.name
        ),
    };

    std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    let path = dir.join(&asset.name);
    if path.is_file() && sha256_file(&path).is_ok_and(|sha| sha == expected) {
        return Ok(path);
    }
    let part = dir.join(format!("{}.part", asset.name));
    let response = agent(Duration::from_secs(30 * 60))
        .get(&asset.url)
        .call()
        .map_err(|e| anyhow::anyhow!("can't download the update: {e}"))?;
    let reader = response
        .into_body()
        .into_with_config()
        .limit(MAX_DOWNLOAD)
        .reader();
    if let Err(e) = save_verified(reader, &part, asset.size, &expected, progress) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&part, &path).with_context(|| format!("can't move {}", part.display()))?;
    Ok(path)
}

/// Writes `reader` to `path`, hashing as it goes; fails unless exactly `size` bytes arrive with sha256
/// `expected`.
fn save_verified(
    mut reader: impl Read,
    path: &Path,
    size: u64,
    expected: &str,
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    let mut file = std::fs::File::create(path).with_context(|| format!("can't write {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).context("the download broke off")?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > size {
            bail!("the download is bigger than GitHub said");
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .with_context(|| format!("can't write {}", path.display()))?;
        progress(done, size);
    }
    file.sync_all()?;
    if done != size {
        bail!("the download stopped early ({done} of {size} bytes)");
    }
    if to_hex(&hasher.finalize()) != expected {
        bail!("the download's checksum is wrong, so clipr won't install it");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "2619c18a2934c2edcc6b0ee2f1450f0fe4f517ac16a93249ebb4395d55c2646d";

    fn release(tag: &str, prerelease: bool, assets: &[&str]) -> GhRelease {
        GhRelease {
            tag_name: tag.into(),
            html_url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
            draft: false,
            prerelease,
            body: Some("notes".into()),
            published_at: None,
            assets: assets
                .iter()
                .map(|name| GhAsset {
                    name: (*name).into(),
                    browser_download_url: format!("https://github.com/{REPO}/releases/download/{tag}/{name}"),
                    size: 10,
                    digest: name
                        .ends_with(".exe")
                        .then(|| format!("sha256:{}", SHA.to_uppercase())),
                })
                .collect(),
        }
    }

    fn v(s: &str) -> Version {
        parse_version(s).unwrap()
    }

    #[test]
    fn picks_the_newest_stable_release() {
        let list = [
            release("v0.1.0", false, &[]),
            release("v0.3.0-beta.1", true, &[]),
            release(
                "v0.2.0",
                false,
                &["clipr-0.2.0-setup.exe", "clipr-0.2.0-windows-x64.zip"],
            ),
        ];
        let r = newest(&list, &v("0.1.0"), false).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert!(!r.prerelease);
        let setup = r.setup.unwrap();
        assert_eq!(setup.name, "clipr-0.2.0-setup.exe");
        assert_eq!(setup.sha256.as_deref(), Some(SHA), "the digest is lowercased");
        assert_eq!(r.portable.unwrap().name, "clipr-0.2.0-windows-x64.zip");
    }

    #[test]
    fn prereleases_only_when_asked() {
        let list = [release("v0.2.0-beta.2", true, &[]), release("v0.1.0", false, &[])];
        let beta = v("0.1.0-beta.1");
        assert_eq!(newest(&list, &beta, true).unwrap().version, "0.2.0-beta.2");
        assert_eq!(newest(&list, &beta, false).unwrap().version, "0.1.0");
        // A tag with a pre part counts as a prerelease even when GitHub isn't told.
        let unmarked = [release("v0.2.0-rc.1", false, &[])];
        assert!(newest(&unmarked, &v("0.1.0"), false).is_none());
    }

    #[test]
    fn nothing_when_up_to_date_and_drafts_are_skipped() {
        let mut draft = release("v9.0.0", false, &[]);
        draft.draft = true;
        let list = [release("v0.1.0", false, &[]), release("junk", false, &[]), draft];
        assert!(newest(&list, &v("0.1.0"), true).is_none());
        // 0.1.0 is newer than its own beta.
        assert_eq!(newest(&list, &v("0.1.0-beta.1"), false).unwrap().version, "0.1.0");
    }

    #[test]
    fn finds_a_checksum_asset_and_skips_strange_names() {
        let mut r = release(
            "v0.2.0",
            false,
            &[
                "clipr-0.2.0-windows-x64.zip",
                "clipr-0.2.0-windows-x64.zip.sha256",
            ],
        );
        r.assets.push(GhAsset {
            name: "..\\evil-setup.exe".into(),
            browser_download_url: "https://github.com/x".into(),
            size: 1,
            digest: None,
        });
        let r = newest(&[r], &v("0.1.0"), false).unwrap();
        assert!(r.setup.is_none(), "a name with a path is never used");
        let zip = r.portable.unwrap();
        assert!(zip.sha256.is_none());
        assert!(zip.checksum_url.unwrap().ends_with(".zip.sha256"));
    }

    #[test]
    fn long_notes_are_cut() {
        let mut r = release("v0.2.0", false, &[]);
        r.body = Some("é".repeat(MAX_NOTES + 50));
        let notes = newest(&[r], &v("0.1.0"), false).unwrap().notes;
        assert!(notes.chars().count() < MAX_NOTES + 10);
    }

    #[test]
    fn parses_checksums() {
        assert_eq!(parse_digest(&format!("sha256:{SHA}")).as_deref(), Some(SHA));
        assert!(parse_digest("sha1:abc").is_none());
        assert!(parse_digest("sha256:xyz").is_none());
        let line = format!("{SHA}  clipr-setup.exe\n");
        assert_eq!(parse_checksum_file(&line).as_deref(), Some(SHA));
        let upper = format!("{}\r\n", SHA.to_uppercase());
        assert_eq!(parse_checksum_file(&upper).as_deref(), Some(SHA));
        assert!(parse_checksum_file("").is_none());
    }

    #[test]
    fn only_github_urls() {
        assert!(check_download_url("https://github.com/a/b/releases/download/v1/x.exe").is_ok());
        assert!(check_download_url("https://objects.githubusercontent.com/x").is_ok());
        assert!(check_download_url("http://github.com/x").is_err());
        assert!(check_download_url("https://github.com.evil.com/x").is_err());
        assert!(check_download_url("https://evil.com/github.com/x").is_err());
    }

    #[test]
    fn verifies_size_and_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.part");
        let data = b"hello clipr";
        let sha = to_hex(&Sha256::digest(data));
        let len = data.len() as u64;
        let mut seen = 0;
        save_verified(&data[..], &path, len, &sha, |done, _| seen = done).unwrap();
        assert_eq!(seen, len);
        assert_eq!(sha256_file(&path).unwrap(), sha);

        let wrong = "0".repeat(64);
        assert!(save_verified(&data[..], &path, len, &wrong, |_, _| {}).is_err());
        assert!(
            save_verified(&data[..], &path, 5, &sha, |_, _| {}).is_err(),
            "too big"
        );
        assert!(
            save_verified(&data[..], &path, 50, &sha, |_, _| {}).is_err(),
            "too short"
        );
    }

    #[test]
    fn download_refuses_without_a_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let asset = Asset {
            name: "clipr-0.2.0-setup.exe".into(),
            url: format!("https://github.com/{REPO}/releases/download/v0.2.0/clipr-0.2.0-setup.exe"),
            size: 10,
            sha256: None,
            checksum_url: None,
        };
        let err = download(&asset, dir.path(), |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("no checksum"), "{err}");
    }

    #[test]
    fn installed_means_an_uninstaller_beside_the_exe() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("clipr.exe");
        assert!(!is_installed(&exe));
        std::fs::write(dir.path().join("unins000.exe"), b"").unwrap();
        assert!(is_installed(&exe));
    }
}
