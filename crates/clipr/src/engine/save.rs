//! Turning the replay buffer into a clip file: names, folders, markers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use anyhow::{Result, bail};
use clipr_buffer::{Cut, SECOND};
use clipr_mux::MediaFormat;

/// Names handed out to saves that are still being written (their `.part` file may not exist yet).
static IN_FLIGHT: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// A clip name reserved for one save; released when dropped.
pub struct Reserved(PathBuf);

impl Reserved {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Reserved {
    fn drop(&mut self) {
        IN_FLIGHT.lock().unwrap().remove(&self.0);
    }
}

/// Reserves `<clips>\<Game>\<Game>_2026-11-15_14-03-22<suffix>.mp4`, with ` (2)` added if that name is
/// taken: by a file, by its unfinished `.part` file, or by a save still in progress.
pub fn clip_path(clips_dir: &Path, game: &str, suffix: &str) -> Reserved {
    let game = clipr_games::safe_folder_name(game);
    let dir = clips_dir.join(&game);
    let stamp = local_stamp();
    let mut in_flight = IN_FLIGHT.lock().unwrap();
    let taken = |p: &Path| {
        let mut part = p.as_os_str().to_owned();
        part.push(".part");
        p.exists() || Path::new(&part).exists() || in_flight.contains(p)
    };
    let mut path = dir.join(format!("{game}_{stamp}{suffix}.mp4"));
    let mut n = 2;
    while taken(&path) {
        path = dir.join(format!("{game}_{stamp}{suffix} ({n}).mp4"));
        n += 1;
    }
    in_flight.insert(path.clone());
    Reserved(path)
}

/// Local date and time as `2026-11-15_14-03-22`.
fn local_stamp() -> String {
    // SAFETY: no preconditions.
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

/// Writes the clip and its markers sidecar (`<file>.markers.json`, seconds from the clip's start).
pub fn write(
    path: &Path,
    video: &MediaFormat,
    audio: Option<&MediaFormat>,
    cut: &Cut,
    markers: &[i64],
) -> Result<()> {
    let Some(start) = cut.start() else {
        bail!("nothing has been captured yet");
    };
    clipr_mux::write_clip(path, video, audio, cut)?;
    let end = cut.video.last().map_or(start, |p| p.end());
    write_markers(path, start, end, markers);
    Ok(())
}

/// Writes `<file>.markers.json` for the markers between `start` and `end` (clock times). Nothing when
/// there are none.
pub fn write_markers(path: &Path, start: i64, end: i64, markers: &[i64]) {
    let secs: Vec<String> = markers
        .iter()
        .filter(|&&m| (start..=end).contains(&m))
        .map(|&m| format!("{:.2}", (m - start) as f64 / SECOND as f64))
        .collect();
    if secs.is_empty() {
        return;
    }
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".markers.json");
    let json = format!("{{\"markers\":[{}]}}", secs.join(","));
    if let Err(e) = std::fs::write(&sidecar, json) {
        tracing::warn!("couldn't write the markers file: {e}");
    }
}
