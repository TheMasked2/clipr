//! Turning the replay buffer into a clip file: names, folders, markers.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clipr_buffer::{Cut, SECOND};
use clipr_mux::MediaFormat;

/// `<clips>\<Game>\<Game>_2026-11-15_14-03-22<suffix>.mp4`, with ` (2)` added if that exists already.
pub fn clip_path(clips_dir: &Path, game: &str, suffix: &str) -> PathBuf {
    let game = clipr_games::safe_folder_name(game);
    let dir = clips_dir.join(&game);
    let stamp = local_stamp();
    let mut path = dir.join(format!("{game}_{stamp}{suffix}.mp4"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{game}_{stamp}{suffix} ({n}).mp4"));
        n += 1;
    }
    path
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
    write_markers(path, start, markers);
    Ok(())
}

/// Writes `<file>.markers.json` for the markers after `start` (clock times). Nothing when there are none.
pub fn write_markers(path: &Path, start: i64, markers: &[i64]) {
    let secs: Vec<String> = markers
        .iter()
        .filter(|&&m| m >= start)
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
