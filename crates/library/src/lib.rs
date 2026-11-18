//! The clip library: SQLite index, thumbnails and hover-scrub sprites, shelves, markers, auto-cleanup.
//!
//! The files on disk are the truth (`<clips>/<Game>/<name>.mp4|mkv`); the database only holds what files can't:
//! favorites, shelves, and cached duration/size/thumbnail state. [`Library::scan`] syncs the two.
//!
//! - Ids are paths relative to the clips folder with `/` separators; [`Library::resolve`] refuses anything that
//!   leaves the folder.
//! - Deleting from the UI ([`Library::trash`]) goes to the Recycle Bin. [`Library::cleanup`] deletes
//!   **permanently**, since freeing disk space is its whole job (each file is logged).

mod mp4;
mod thumbs;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, Weak, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clipr_core::config::ClipsConfig;
use rusqlite::{Connection, params};
use serde::Serialize;
use tokio::sync::broadcast;

pub use mp4::{Mp4Info, probe_file};
pub use thumbs::{jpeg, thumb_file_name};

/// Frames in a hover-scrub sprite.
pub const SPRITE_FRAMES: u32 = 10;

/// Files modified more recently than this are probably still being written: no thumbnail yet, and cleanup
/// leaves them alone.
const RECENT: Duration = Duration::from_secs(15);

/// Errors the web layer maps to HTTP statuses (found with `anyhow::Error::downcast_ref`).
#[derive(Debug)]
pub enum LibraryError {
    /// 404: no such clip.
    NotFound(String),
    /// 400: the request itself is wrong (bad id, bad title, name taken).
    Invalid(String),
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(m) | Self::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for LibraryError {}

/// How the ids of files in the separate recordings folder start (see [`Library::set_recordings_dir`]). A
/// colon can't be part of a Windows folder name, so it never clashes with a game folder in the clips folder.
pub const RECORDINGS_PREFIX: &str = ":recordings/";

fn invalid(message: impl Into<String>) -> anyhow::Error {
    LibraryError::Invalid(message.into()).into()
}

fn not_found(id: &str) -> anyhow::Error {
    LibraryError::NotFound(format!("there is no clip \"{id}\"")).into()
}

/// A clip as the API returns it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Clip {
    pub id: String,
    pub title: String,
    pub game: String,
    pub kind: String,
    pub size_bytes: u64,
    pub created: i64,
    pub duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub favorite: bool,
    pub shelves: Vec<String>,
    pub markers: Vec<f64>,
    pub url: String,
    pub thumb: Option<String>,
    pub sprite: Option<String>,
    pub sprite_frames: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GameInfo {
    pub name: String,
    pub clips: u64,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ClipFilter {
    /// Case-insensitive text in the title, game or a shelf.
    pub q: Option<String>,
    pub game: Option<String>,
    pub shelf: Option<String>,
    pub favorite: bool,
    /// `clip` or `recording`.
    pub kind: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub added: usize,
    pub removed: usize,
    pub updated: usize,
}

impl ScanReport {
    pub fn changed(&self) -> bool {
        self.added + self.removed + self.updated > 0
    }
}

/// thumb_state: nothing yet / thumbnail and sprite / failed for good / thumbnail only.
const THUMB_NONE: i64 = 0;
const THUMB_BOTH: i64 = 1;
const THUMB_FAILED: i64 = 2;
const THUMB_ONLY: i64 = 3;

#[derive(Clone, Debug, PartialEq)]
struct Row {
    id: String,
    game: String,
    kind: String,
    size: i64,
    created: i64,
    /// Modification time in unix milliseconds.
    mtime: i64,
    duration: Option<f64>,
    width: Option<i64>,
    height: Option<i64>,
    favorite: bool,
    shelves: Vec<String>,
    markers: Vec<f64>,
    markers_mtime: i64,
    thumb_state: i64,
}

const COLS: &str = "id, game, kind, size, created, mtime, duration, width, height, favorite, shelves, markers, markers_mtime, thumb_state";

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let shelves: String = r.get(10)?;
    let markers: String = r.get(11)?;
    Ok(Row {
        id: r.get(0)?,
        game: r.get(1)?,
        kind: r.get(2)?,
        size: r.get(3)?,
        created: r.get(4)?,
        mtime: r.get(5)?,
        duration: r.get(6)?,
        width: r.get(7)?,
        height: r.get(8)?,
        favorite: r.get::<_, i64>(9)? != 0,
        shelves: serde_json::from_str(&shelves).unwrap_or_default(),
        markers: serde_json::from_str(&markers).unwrap_or_default(),
        markers_mtime: r.get(12)?,
        thumb_state: r.get(13)?,
    })
}

fn upsert(conn: &Connection, row: &Row) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO clips ({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"
        ),
        params![
            row.id,
            row.game,
            row.kind,
            row.size,
            row.created,
            row.mtime,
            row.duration,
            row.width,
            row.height,
            i64::from(row.favorite),
            serde_json::to_string(&row.shelves)?,
            serde_json::to_string(&row.markers)?,
            row.markers_mtime,
            row.thumb_state
        ],
    )
    .context("can't write to the clip database")?;
    Ok(())
}

struct Job {
    id: String,
    path: PathBuf,
    duration: Option<f64>,
}

struct Inner {
    clips_dir: RwLock<PathBuf>,
    /// Full recordings, when they have a folder of their own.
    recordings_dir: RwLock<Option<PathBuf>>,
    thumbs_dir: PathBuf,
    db: Mutex<Connection>,
    last_scan: Mutex<Option<Instant>>,
    scan_lock: Mutex<()>,
    queued: Mutex<HashSet<String>>,
    jobs: Sender<Job>,
    changes: broadcast::Sender<()>,
    recycle_bin: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The clip library. `Send + Sync`; share it as `Arc<Library>`.
pub struct Library {
    inner: Arc<Inner>,
}

impl Library {
    /// Opens (creating if needed) the database at `db_file`. Thumbnails live in `thumbs\` next to it.
    pub fn open(clips_dir: PathBuf, db_file: PathBuf) -> Result<Library> {
        let data_dir = db_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        fs::create_dir_all(&data_dir).with_context(|| format!("can't create {}", data_dir.display()))?;
        let thumbs_dir = data_dir.join("thumbs");
        fs::create_dir_all(&thumbs_dir).with_context(|| format!("can't create {}", thumbs_dir.display()))?;
        let conn = Connection::open(&db_file)
            .with_context(|| format!("can't open the clip database {}", db_file.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS clips (
                 id TEXT PRIMARY KEY,
                 game TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 created INTEGER NOT NULL,
                 mtime INTEGER NOT NULL,
                 duration REAL,
                 width INTEGER,
                 height INTEGER,
                 favorite INTEGER NOT NULL DEFAULT 0,
                 shelves TEXT NOT NULL DEFAULT '[]',
                 markers TEXT NOT NULL DEFAULT '[]',
                 markers_mtime INTEGER NOT NULL DEFAULT 0,
                 thumb_state INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS clips_created ON clips (created DESC);",
        )
        .context("can't set up the clip database")?;
        let (jobs, rx) = mpsc::channel();
        let inner = Arc::new(Inner {
            clips_dir: RwLock::new(clips_dir),
            recordings_dir: RwLock::new(None),
            thumbs_dir,
            db: Mutex::new(conn),
            last_scan: Mutex::new(None),
            scan_lock: Mutex::new(()),
            queued: Mutex::new(HashSet::new()),
            jobs,
            changes: broadcast::Sender::new(16),
            recycle_bin: AtomicBool::new(true),
        });
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("clipr-thumbs".into())
            .spawn(move || worker(&weak, &rx))
            .context("can't start the thumbnail thread")?;
        Ok(Library { inner })
    }

    /// Tests set this to `false` so deleting doesn't touch the real Recycle Bin.
    pub fn set_recycle_bin(&self, on: bool) {
        self.inner.recycle_bin.store(on, Ordering::Relaxed);
    }

    pub fn clips_dir(&self) -> PathBuf {
        self.inner
            .clips_dir
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Where thumbnails and sprites are kept (see [`thumb_file_name`]).
    pub fn thumbs_dir(&self) -> &Path {
        &self.inner.thumbs_dir
    }

    /// Call when the clips-folder setting changes. The index is rebuilt by the next [`scan`](Self::scan).
    pub fn set_clips_dir(&self, dir: PathBuf) {
        let mut current = self
            .inner
            .clips_dir
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if *current == dir {
            return;
        }
        *current = dir;
        drop(current);
        let _ = lock(&self.inner.db).execute("DELETE FROM clips", []);
        lock(&self.inner.queued).clear();
        *lock(&self.inner.last_scan) = None;
    }

    /// The separate recordings folder, if there is one.
    pub fn recordings_dir(&self) -> Option<PathBuf> {
        self.inner
            .recordings_dir
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Call when the recordings-folder setting changes (`None`: recordings go to the clips folder). Files there
    /// get ids starting with [`RECORDINGS_PREFIX`]; the next [`scan`](Self::scan) indexes them.
    pub fn set_recordings_dir(&self, dir: Option<PathBuf>) {
        let dir = dir.filter(|d| *d != self.clips_dir());
        let mut current = self
            .inner
            .recordings_dir
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if *current == dir {
            return;
        }
        *current = dir;
        drop(current);
        let _ = lock(&self.inner.db).execute(
            "DELETE FROM clips WHERE substr(id, 1, ?1) = ?2",
            params![RECORDINGS_PREFIX.len() as i64, RECORDINGS_PREFIX],
        );
        *lock(&self.inner.last_scan) = None;
    }

    /// The folders indexed, with the id prefix of each: the clips folder, and the recordings folder if set.
    fn roots(&self) -> Vec<(&'static str, PathBuf)> {
        let mut roots = vec![("", self.clips_dir())];
        if let Some(rec) = self.recordings_dir() {
            roots.push((RECORDINGS_PREFIX, rec));
        }
        roots
    }

    /// The folder an id lives in and the id's path inside it.
    fn split_id<'a>(&self, id: &'a str) -> Option<(PathBuf, &'a str)> {
        match id.strip_prefix(RECORDINGS_PREFIX) {
            Some(rest) => self.recordings_dir().map(|d| (d, rest)),
            None => Some((self.clips_dir(), id)),
        }
    }

    /// Fires when the library changed behind the caller's back (a thumbnail finished, or
    /// [`notify_changed`](Self::notify_changed) was called).
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.inner.changes.subscribe()
    }

    pub fn notify_changed(&self) {
        let _ = self.inner.changes.send(());
    }

    /// Checks that `id` stays inside the clips folder and names a video file; returns its path.
    pub fn resolve(&self, id: &str) -> Result<PathBuf> {
        let (dir, rest) = self
            .split_id(id)
            .ok_or_else(|| invalid(format!("\"{id}\" isn't a clip in the clips folder")))?;
        resolve_in(&dir, rest)
    }

    fn path_of(&self, id: &str) -> PathBuf {
        let (dir, rest) = self.split_id(id).unwrap_or_else(|| (self.clips_dir(), id));
        rest.split('/').fold(dir, |p, part| p.join(part))
    }

    /// Whether the folder an id lives in is there (an unplugged drive isn't).
    fn root_present(&self, id: &str) -> bool {
        self.split_id(id).is_some_and(|(dir, _)| dir.is_dir())
    }

    // ---- scanning --------------------------------------------------------------------------------------

    /// Syncs the index with the folder: new files are added (and get thumbnails), vanished ones dropped.
    pub fn scan(&self) -> Result<ScanReport> {
        let _guard = lock(&self.inner.scan_lock);
        self.scan_locked()
    }

    fn scan_locked(&self) -> Result<ScanReport> {
        let result = self.scan_inner();
        *lock(&self.inner.last_scan) = Some(Instant::now());
        result
    }

    fn scan_inner(&self) -> Result<ScanReport> {
        let mut report = ScanReport::default();
        // Rows first: one added while walking must not look like a vanished file.
        let existing: HashMap<String, Row> =
            self.all_rows()?.into_iter().map(|r| (r.id.clone(), r)).collect();
        let mut entries = Vec::new();
        // Folders that were read; rows of the others stay (an unplugged drive must not wipe the favorites).
        let mut walked: Vec<&'static str> = Vec::new();
        for (prefix, dir) in self.roots() {
            if !dir.is_dir() {
                continue;
            }
            for mut e in walk(&dir)? {
                e.id = format!("{prefix}{}", e.id);
                entries.push(e);
            }
            walked.push(prefix);
        }
        if walked.is_empty() {
            return Ok(report);
        }
        let in_walked = |id: &str| {
            let prefix = if id.starts_with(RECORDINGS_PREFIX) {
                RECORDINGS_PREFIX
            } else {
                ""
            };
            walked.contains(&prefix)
        };
        let mut writes = Vec::new();
        let mut seen = HashSet::new();
        for entry in &entries {
            seen.insert(entry.id.as_str());
            let old = existing.get(&entry.id);
            let row = build_row(entry, old, false);
            if old != Some(&row) {
                if old.is_some() {
                    report.updated += 1;
                } else {
                    report.added += 1;
                }
                writes.push(row);
            }
        }
        let gone: Vec<&Row> = existing
            .values()
            .filter(|r| !seen.contains(r.id.as_str()) && in_walked(&r.id))
            .collect();
        report.removed = gone.len();
        {
            let mut db = lock(&self.inner.db);
            let tx = db.transaction()?;
            for row in &writes {
                upsert(&tx, row)?;
            }
            for row in &gone {
                tx.execute("DELETE FROM clips WHERE id = ?1", [&row.id])?;
            }
            tx.commit()?;
        }
        for row in &gone {
            remove_thumbs(&self.inner.thumbs_dir, &row.id);
        }
        // Thumbnails for everything that has none yet, except files that are still being written.
        let now = now_millis();
        let pending: Vec<Row> = self
            .all_rows()?
            .into_iter()
            .filter(|r| r.thumb_state == THUMB_NONE && now - r.mtime > RECENT.as_millis() as i64)
            .collect();
        self.enqueue(&pending);
        Ok(report)
    }

    /// Scans unless a scan finished less than `max_age` ago.
    pub fn scan_if_stale(&self, max_age: Duration) -> Result<Option<ScanReport>> {
        // Wait for a scan that is running right now: its result is what the caller wants to see.
        let _guard = lock(&self.inner.scan_lock);
        let fresh = lock(&self.inner.last_scan).is_some_and(|t| t.elapsed() < max_age);
        if fresh {
            return Ok(None);
        }
        self.scan_locked().map(Some)
    }

    /// Adds or refreshes one finished file (for `ClipSaved`), and queues its thumbnails.
    pub fn add_file(&self, path: &Path) -> Result<Clip> {
        let under = |dir: &Path| {
            path.strip_prefix(dir).ok().map(Path::to_path_buf).or_else(|| {
                let (p, d) = (path.canonicalize().ok()?, dir.canonicalize().ok()?);
                p.strip_prefix(d).ok().map(Path::to_path_buf)
            })
        };
        // The recordings folder first, in case it sits inside the clips folder.
        let (prefix, dir, rel) = self
            .roots()
            .into_iter()
            .rev()
            .find_map(|(prefix, dir)| under(&dir).map(|rel| (prefix, dir, rel)))
            .with_context(|| format!("{} is outside the clips folder", path.display()))?;
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let full = resolve_in(&dir, &rel)?;
        let mut entry = entry_for(&full, &rel)?;
        let id = format!("{prefix}{rel}");
        entry.id = id.clone();
        let old = self.row(&id)?;
        let row = build_row(&entry, old.as_ref(), true);
        if old.as_ref() != Some(&row) {
            upsert(&lock(&self.inner.db), &row)?;
        }
        if row.thumb_state == THUMB_NONE {
            self.enqueue(std::slice::from_ref(&row));
        }
        Ok(self.to_clip(&row))
    }

    fn enqueue(&self, rows: &[Row]) {
        let mut queued = lock(&self.inner.queued);
        for row in rows {
            if queued.insert(row.id.clone()) {
                let job = Job {
                    id: row.id.clone(),
                    path: self.path_of(&row.id),
                    duration: row.duration,
                };
                let _ = self.inner.jobs.send(job);
            }
        }
    }

    // ---- queries ---------------------------------------------------------------------------------------

    fn all_rows(&self) -> Result<Vec<Row>> {
        let db = lock(&self.inner.db);
        let mut stmt = db.prepare(&format!("SELECT {COLS} FROM clips ORDER BY created DESC, id"))?;
        let rows = stmt
            .query_map([], row_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn row(&self, id: &str) -> Result<Option<Row>> {
        let db = lock(&self.inner.db);
        let mut stmt = db.prepare(&format!("SELECT {COLS} FROM clips WHERE id = ?1"))?;
        let mut rows = stmt.query_map([id], row_from)?;
        Ok(rows.next().transpose()?)
    }

    fn to_clip(&self, row: &Row) -> Clip {
        let name = row.id.rsplit('/').next().unwrap_or(&row.id);
        let title = name.rsplit_once('.').map_or(name, |(stem, _)| stem).to_string();
        let encoded = encode_path(&row.id);
        let has_thumb = matches!(row.thumb_state, THUMB_BOTH | THUMB_ONLY);
        Clip {
            id: row.id.clone(),
            title,
            game: row.game.clone(),
            kind: row.kind.clone(),
            size_bytes: row.size.max(0) as u64,
            created: row.created,
            duration_secs: row.duration,
            width: row.width.and_then(|v| u32::try_from(v).ok()),
            height: row.height.and_then(|v| u32::try_from(v).ok()),
            favorite: row.favorite,
            shelves: row.shelves.clone(),
            markers: row.markers.clone(),
            // `?v=` changes when the file does (a trim that replaced it), so browsers never show a cached copy
            // of the old video or picture under the same address.
            url: format!("/media/{encoded}?v={}", row.mtime),
            thumb: has_thumb.then(|| format!("/thumbs/{encoded}.jpg?v={}", row.mtime)),
            sprite: (row.thumb_state == THUMB_BOTH)
                .then(|| format!("/thumbs/{encoded}.sprite.jpg?v={}", row.mtime)),
            sprite_frames: SPRITE_FRAMES,
        }
    }

    /// Newest first.
    pub fn list(&self, filter: &ClipFilter) -> Result<Vec<Clip>> {
        let q = filter
            .q
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(str::to_lowercase);
        let clips = self
            .all_rows()?
            .iter()
            .filter(|r| filter.game.as_ref().is_none_or(|g| &r.game == g))
            .filter(|r| filter.shelf.as_ref().is_none_or(|s| r.shelves.contains(s)))
            .filter(|r| !filter.favorite || r.favorite)
            .filter(|r| filter.kind.as_ref().is_none_or(|k| &r.kind == k))
            .filter(|r| {
                q.as_ref().is_none_or(|q| {
                    let name = r.id.rsplit('/').next().unwrap_or(&r.id).to_lowercase();
                    name.contains(q.as_str())
                        || r.game.to_lowercase().contains(q.as_str())
                        || r.shelves.iter().any(|s| s.to_lowercase().contains(q.as_str()))
                })
            })
            .map(|r| self.to_clip(r))
            .collect();
        Ok(clips)
    }

    pub fn get(&self, id: &str) -> Result<Clip> {
        let row = self.row(id)?.ok_or_else(|| not_found(id))?;
        Ok(self.to_clip(&row))
    }

    pub fn games(&self) -> Result<Vec<GameInfo>> {
        let db = lock(&self.inner.db);
        let mut stmt = db.prepare(
            "SELECT game, COUNT(*), COALESCE(SUM(size), 0) FROM clips GROUP BY game ORDER BY game COLLATE NOCASE",
        )?;
        let games = stmt
            .query_map([], |r| {
                Ok(GameInfo {
                    name: r.get(0)?,
                    clips: r.get::<_, i64>(1)?.max(0) as u64,
                    size_bytes: r.get::<_, i64>(2)?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(games)
    }

    pub fn shelves(&self) -> Result<Vec<String>> {
        let mut all: Vec<String> = self.all_rows()?.into_iter().flat_map(|r| r.shelves).collect();
        all.sort_by_key(|s| s.to_lowercase());
        all.dedup();
        Ok(all)
    }

    // ---- changes ---------------------------------------------------------------------------------------

    /// Renames (a new `title` renames the file and its sidecars; the id changes), and sets favorite / shelves.
    pub fn update(
        &self,
        id: &str,
        title: Option<&str>,
        favorite: Option<bool>,
        shelves: Option<&[String]>,
    ) -> Result<Clip> {
        let _guard = lock(&self.inner.scan_lock);
        let mut row = self.row(id)?.ok_or_else(|| not_found(id))?;
        let old_id = row.id.clone();
        if let Some(title) = title {
            let path = self.resolve(id)?;
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mp4")
                .to_string();
            let title = clean_title(title, &ext)?;
            let old_name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let new_name = format!("{title}.{ext}");
            if new_name != old_name {
                let target = path.with_file_name(&new_name);
                if target.exists() && !new_name.eq_ignore_ascii_case(&old_name) {
                    return Err(invalid(format!(
                        "there is already a clip called \"{title}\" in this folder"
                    )));
                }
                fs::rename(&path, &target).with_context(|| {
                    format!("couldn't rename \"{old_name}\" (is it open in another program?)")
                })?;
                let (old_side, new_side) = (markers_path(&path), markers_path(&target));
                if old_side.exists() {
                    if let Err(e) = fs::rename(&old_side, &new_side) {
                        tracing::warn!("can't move the markers file of {old_name}: {e}");
                    }
                }
                row.id = match old_id.rsplit_once('/') {
                    Some((dir, _)) => format!("{dir}/{new_name}"),
                    None => new_name.clone(),
                };
                row.kind = kind_of(&new_name).to_string();
                rename_thumbs(&self.inner.thumbs_dir, &old_id, &row.id);
                row.mtime = fs::metadata(&target).map_or(row.mtime, |m| millis(m.modified()));
            }
        }
        if let Some(fav) = favorite {
            row.favorite = fav;
        }
        if let Some(shelves) = shelves {
            row.shelves = clean_shelves(shelves)?;
        }
        {
            let db = lock(&self.inner.db);
            if row.id != old_id {
                db.execute("DELETE FROM clips WHERE id = ?1", [&old_id])?;
            }
            upsert(&db, &row)?;
        }
        if row.id != old_id {
            let mut queued = lock(&self.inner.queued);
            if queued.remove(&old_id) {
                queued.insert(row.id.clone());
            }
        }
        Ok(self.to_clip(&row))
    }

    /// Moves the clip and its sidecars to the Recycle Bin.
    pub fn trash(&self, id: &str) -> Result<()> {
        let _guard = lock(&self.inner.scan_lock);
        let path = self.resolve(id)?;
        if self.row(id)?.is_none() && !path.exists() {
            return Err(not_found(id));
        }
        let mut files: Vec<PathBuf> = [path.clone(), markers_path(&path)]
            .into_iter()
            .filter(|p| p.exists())
            .collect();
        if self.inner.recycle_bin.load(Ordering::Relaxed) {
            if !files.is_empty() {
                trash::delete_all(&files).with_context(|| {
                    format!("couldn't move \"{id}\" to the Recycle Bin (is it open in another program?)")
                })?;
            }
        } else {
            for f in files.drain(..) {
                fs::remove_file(&f).with_context(|| format!("couldn't delete {}", f.display()))?;
            }
        }
        remove_thumbs(&self.inner.thumbs_dir, id);
        lock(&self.inner.db).execute("DELETE FROM clips WHERE id = ?1", [id])?;
        lock(&self.inner.queued).remove(id);
        Ok(())
    }

    /// Writes the markers sidecar for the clip at `path` (seconds, sorted).
    pub fn write_markers(&self, path: &Path, markers: &[f64]) -> Result<()> {
        let mut sorted: Vec<f64> = markers
            .iter()
            .copied()
            .filter(|m| m.is_finite() && *m >= 0.0)
            .collect();
        sorted.sort_by(f64::total_cmp);
        let side = markers_path(path);
        if sorted.is_empty() {
            let _ = fs::remove_file(&side);
        } else {
            fs::write(&side, serde_json::json!({ "markers": sorted }).to_string())
                .with_context(|| format!("can't write {}", side.display()))?;
        }
        Ok(())
    }

    /// Deletes clips by the age and size limits, oldest first, never favorites. Files are deleted
    /// **permanently** (not to the Recycle Bin), since the point is to free space. Returns how many were
    /// removed and how many bytes that freed.
    pub fn cleanup(&self, cfg: &ClipsConfig) -> Result<(usize, u64)> {
        if cfg.max_age_days == 0 && cfg.max_total_gb == 0 {
            return Ok((0, 0));
        }
        // An unplugged drive must not look like an empty library.
        if !self.clips_dir().is_dir() {
            return Ok((0, 0));
        }
        // Hold the lock until the deletes are done, so a rename or favorite can't slip in between.
        let _guard = lock(&self.inner.scan_lock);
        self.scan_locked()?;
        let rows = self.all_rows()?; // newest first
        let doomed = self.doomed(&rows, cfg, now_millis());
        let (mut removed, mut freed) = (0usize, 0u64);
        for r in doomed {
            // Re-check the favorite flag at the moment of deleting.
            let dropped =
                lock(&self.inner.db).execute("DELETE FROM clips WHERE id = ?1 AND favorite = 0", [&r.id])?;
            if dropped == 0 {
                continue;
            }
            let path = self.path_of(&r.id);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!("cleanup: can't delete {}: {e}", path.display());
                    continue;
                }
            }
            let _ = fs::remove_file(markers_path(&path));
            remove_thumbs(&self.inner.thumbs_dir, &r.id);
            tracing::info!("cleanup: deleted {} ({} bytes)", r.id, r.size);
            removed += 1;
            freed += r.size.max(0) as u64;
        }
        Ok((removed, freed))
    }

    /// What [`cleanup`](Self::cleanup) would delete with the limits in `cfg` if it ran `ahead` from now (the
    /// size limit counts today's total), oldest first. Deletes nothing: it's for warning before it happens.
    pub fn cleanup_preview(&self, cfg: &ClipsConfig, ahead: std::time::Duration) -> Result<Vec<Clip>> {
        if (cfg.max_age_days == 0 && cfg.max_total_gb == 0) || !self.clips_dir().is_dir() {
            return Ok(Vec::new());
        }
        let rows = {
            let _guard = lock(&self.inner.scan_lock);
            self.scan_locked()?;
            self.all_rows()?
        };
        let at = now_millis() + ahead.as_millis() as i64;
        let mut doomed = self.doomed(&rows, cfg, at);
        doomed.sort_by_key(|r| r.created);
        Ok(doomed.into_iter().map(|r| self.to_clip(r)).collect())
    }

    /// The rows the limits in `cfg` pick at the time `now_ms`: too old first, then the oldest until the rest
    /// fits the size limit. Never favorites, clips saved in the last few minutes, or files on a missing drive.
    fn doomed<'a>(&self, rows: &'a [Row], cfg: &ClipsConfig, now_ms: i64) -> Vec<&'a Row> {
        // Never a file whose folder is gone (an unplugged drive): its row would vanish with its favorite.
        let eligible =
            |r: &Row| !r.favorite && now_ms - r.mtime > RECENT.as_millis() as i64 && self.root_present(&r.id);
        let mut doomed: Vec<&Row> = Vec::new();
        let mut chosen: HashSet<&str> = HashSet::new();
        if cfg.max_age_days > 0 {
            let cutoff = now_ms / 1000 - i64::from(cfg.max_age_days) * 86_400;
            for r in rows.iter().rev().filter(|r| eligible(r) && r.created < cutoff) {
                chosen.insert(&r.id);
                doomed.push(r);
            }
        }
        if cfg.max_total_gb > 0 {
            let limit = i64::from(cfg.max_total_gb) << 30;
            let mut total: i64 =
                rows.iter().map(|r| r.size).sum::<i64>() - doomed.iter().map(|r| r.size).sum::<i64>();
            for r in rows
                .iter()
                .rev()
                .filter(|r| eligible(r) && !chosen.contains(r.id.as_str()))
            {
                if total <= limit {
                    break;
                }
                total -= r.size;
                doomed.push(r);
            }
        }
        doomed
    }

    /// `(free bytes, total bytes, bytes the clips use)`. Free and total are for the clips folder's drive.
    pub fn disk(&self) -> Result<(u64, u64, u64)> {
        use std::os::windows::ffi::OsStrExt;
        let mut probe = self.clips_dir();
        while !probe.exists() {
            if !probe.pop() {
                break;
            }
        }
        let wide: Vec<u16> = probe.as_os_str().encode_wide().chain([0]).collect();
        let (mut free, mut total) = (0u64, 0u64);
        // SAFETY: `wide` is NUL-terminated and outlives the call; the out pointers are valid u64s.
        unsafe {
            windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                windows::core::PCWSTR(wide.as_ptr()),
                Some(&mut free),
                Some(&mut total),
                None,
            )
        }
        .with_context(|| format!("can't read the disk space of {}", probe.display()))?;
        let used: i64 =
            lock(&self.inner.db).query_row("SELECT COALESCE(SUM(size), 0) FROM clips", [], |r| r.get(0))?;
        Ok((free, total, used.max(0) as u64))
    }
}

// ---- the thumbnail thread -------------------------------------------------------------------------------

fn worker(weak: &Weak<Inner>, rx: &Receiver<Job>) {
    thumbs::lower_priority();
    let mut session: Option<Option<thumbs::MfSession>> = None;
    while let Ok(job) = rx.recv() {
        let Some(inner) = weak.upgrade() else { break };
        let session = session.get_or_insert_with(|| match thumbs::MfSession::start() {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!("thumbnails are off: {e:#}");
                None
            }
        });
        let still_wanted = lock(&inner.db)
            .query_row("SELECT thumb_state FROM clips WHERE id = ?1", [&job.id], |r| {
                r.get::<_, i64>(0)
            })
            .is_ok_and(|s| s == THUMB_NONE);
        let state = if !still_wanted || session.is_none() || !job.path.exists() {
            None
        } else {
            let thumb = inner.thumbs_dir.join(thumb_file_name(&job.id, false));
            let sprite = inner.thumbs_dir.join(thumb_file_name(&job.id, true));
            match thumbs::generate(&job.path, &thumb, &sprite, job.duration, SPRITE_FRAMES) {
                Ok(thumbs::Made::Both) => Some(THUMB_BOTH),
                Ok(thumbs::Made::ThumbOnly) => Some(THUMB_ONLY),
                Err(e) => {
                    tracing::warn!("no thumbnail for {}: {e:#}", job.id);
                    Some(THUMB_FAILED)
                }
            }
        };
        if let Some(state) = state {
            let _ = lock(&inner.db).execute(
                "UPDATE clips SET thumb_state = ?1 WHERE id = ?2",
                params![state, job.id],
            );
            let _ = inner.changes.send(());
        }
        lock(&inner.queued).remove(&job.id);
    }
}

// ---- files ----------------------------------------------------------------------------------------------

struct FileEntry {
    id: String,
    game: String,
    path: PathBuf,
    size: i64,
    created: i64,
    mtime: i64,
    markers_mtime: i64,
}

fn millis(t: std::io::Result<SystemTime>) -> i64 {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

fn now_millis() -> i64 {
    millis(Ok(SystemTime::now()))
}

fn is_video(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".mp4") || lower.ends_with(".mkv")
}

fn kind_of(file_name: &str) -> &'static str {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
    if stem.ends_with("_recording") {
        "recording"
    } else {
        "clip"
    }
}

fn markers_path(video: &Path) -> PathBuf {
    let mut s = video.as_os_str().to_owned();
    s.push(".markers.json");
    PathBuf::from(s)
}

fn read_markers(video: &Path) -> Vec<f64> {
    #[derive(serde::Deserialize)]
    struct Sidecar {
        #[serde(default)]
        markers: Vec<f64>,
    }
    let Ok(text) = fs::read_to_string(markers_path(video)) else {
        return Vec::new();
    };
    let mut markers = serde_json::from_str::<Sidecar>(&text)
        .map(|s| s.markers)
        .unwrap_or_default();
    markers.retain(|m| m.is_finite() && *m >= 0.0);
    markers.sort_by(f64::total_cmp);
    markers
}

fn entry_for(path: &Path, id: &str) -> Result<FileEntry> {
    let meta = fs::metadata(path).with_context(|| format!("can't read {}", path.display()))?;
    let game = match id.split_once('/') {
        Some((game, _)) => game.to_string(),
        None => "Other".to_string(),
    };
    let mtime = millis(meta.modified());
    let created = meta.created().or_else(|_| meta.modified());
    Ok(FileEntry {
        id: id.to_string(),
        game,
        path: path.to_path_buf(),
        size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
        created: millis(created) / 1000,
        mtime,
        markers_mtime: fs::metadata(markers_path(path)).map_or(0, |m| millis(m.modified())),
    })
}

/// The video files in `dir` and one level of game folders below it.
fn walk(dir: &Path) -> Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("can't read {}", dir.display()))?
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else { continue };
        if name.starts_with('.') || kind.is_symlink() {
            continue;
        }
        if kind.is_file() && is_video(&name) {
            out.extend(entry_for(&entry.path(), &name).ok());
        } else if kind.is_dir() {
            let Ok(sub) = fs::read_dir(entry.path()) else {
                continue;
            };
            for file in sub.flatten() {
                let fname = file.file_name().to_string_lossy().into_owned();
                let Ok(fkind) = file.file_type() else { continue };
                if fkind.is_file() && !fname.starts_with('.') && is_video(&fname) {
                    out.extend(entry_for(&file.path(), &format!("{name}/{fname}")).ok());
                }
            }
        }
    }
    Ok(out)
}

/// The row for a file: reuses what was learned before when size and time are unchanged. `reset_thumbs` throws
/// the thumbnails away when the file changed.
fn build_row(e: &FileEntry, old: Option<&Row>, reset_thumbs: bool) -> Row {
    let same_file = old.filter(|o| o.size == e.size && o.mtime == e.mtime);
    let (duration, width, height) = match same_file {
        Some(o) => (o.duration, o.width, o.height),
        None if e.id.to_ascii_lowercase().ends_with(".mp4") => match mp4::probe_file(&e.path) {
            Ok(info) => (
                info.duration_secs,
                info.width.map(i64::from),
                info.height.map(i64::from),
            ),
            Err(err) => {
                tracing::debug!("can't read the length of {}: {err:#}", e.id);
                (None, None, None)
            }
        },
        None => (None, None, None),
    };
    let markers = match old {
        Some(o) if o.markers_mtime == e.markers_mtime => o.markers.clone(),
        _ => read_markers(&e.path),
    };
    let thumb_state = match old {
        None => THUMB_NONE,
        Some(o) if same_file.is_none() && reset_thumbs => {
            if o.thumb_state != THUMB_NONE {
                // The picture is stale; the caller queues a fresh one (the files are overwritten).
            }
            THUMB_NONE
        }
        Some(o) => o.thumb_state,
    };
    Row {
        id: e.id.clone(),
        game: e.game.clone(),
        kind: kind_of(&e.id).to_string(),
        size: e.size,
        created: same_file.map_or(e.created, |o| o.created),
        mtime: e.mtime,
        duration,
        width,
        height,
        favorite: old.is_some_and(|o| o.favorite),
        shelves: old.map(|o| o.shelves.clone()).unwrap_or_default(),
        markers,
        markers_mtime: e.markers_mtime,
        thumb_state,
    }
}

fn resolve_in(dir: &Path, id: &str) -> Result<PathBuf> {
    let bad = || invalid(format!("\"{id}\" isn't a clip in the clips folder"));
    if id.is_empty() || id.starts_with('/') || id.contains(['\\', ':', '\0']) {
        return Err(bad());
    }
    let mut path = dir.to_path_buf();
    for part in id.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part.chars().any(|c| c.is_control() || "<>\"|?*".contains(c))
        {
            return Err(bad());
        }
        path.push(part);
    }
    if !is_video(id) {
        return Err(bad());
    }
    // A symlink (or junction) inside the folder must not lead out of it.
    if let Ok(root) = dir.canonicalize() {
        let real = if path.exists() {
            path.canonicalize().ok()
        } else {
            path.parent().and_then(|p| p.canonicalize().ok())
        };
        if real.is_some_and(|p| !p.starts_with(&root)) {
            return Err(bad());
        }
    }
    Ok(path)
}

/// Percent-encodes each path segment for use in a URL.
fn encode_path(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 8);
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn clean_title(title: &str, ext: &str) -> Result<String> {
    let mut t = title.trim();
    if let Some(stem) = t
        .to_ascii_lowercase()
        .strip_suffix(&format!(".{ext}"))
        .map(str::len)
    {
        t = t[..stem].trim_end();
    }
    if t.is_empty() {
        return Err(invalid("the title can't be empty"));
    }
    if t.chars().count() > 150 {
        return Err(invalid("the title is too long (150 characters at most)"));
    }
    if let Some(c) = t.chars().find(|c| c.is_control() || "<>:\"/\\|?*".contains(*c)) {
        return Err(invalid(format!("a title can't contain {c:?}")));
    }
    if t.ends_with('.') {
        return Err(invalid("a title can't end with a dot"));
    }
    // Files starting with a dot are skipped by the folder walk.
    if t.starts_with('.') {
        return Err(invalid("a title can't start with a dot"));
    }
    let upper = t.split('.').next().unwrap_or(t).to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(invalid(format!("\"{t}\" is a reserved name in Windows")));
    }
    Ok(t.to_string())
}

fn clean_shelves(shelves: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for s in shelves {
        let s = s.trim();
        if s.is_empty() {
            continue;
        }
        if s.chars().count() > 40 {
            return Err(invalid("a shelf name can be 40 characters at most"));
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(s)) {
            out.push(s.to_string());
        }
    }
    if out.len() > 30 {
        return Err(invalid("a clip can be on 30 shelves at most"));
    }
    Ok(out)
}

fn remove_thumbs(dir: &Path, id: &str) {
    for sprite in [false, true] {
        let _ = fs::remove_file(dir.join(thumb_file_name(id, sprite)));
    }
}

fn rename_thumbs(dir: &Path, old: &str, new: &str) {
    for sprite in [false, true] {
        let _ = fs::rename(
            dir.join(thumb_file_name(old, sprite)),
            dir.join(thumb_file_name(new, sprite)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        lib: Library,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("clips");
        fs::create_dir_all(&root).unwrap();
        let lib = Library::open(root.clone(), tmp.path().join("data").join("library.db")).unwrap();
        lib.set_recycle_bin(false);
        Fixture { _tmp: tmp, root, lib }
    }

    fn backdate(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    fn put(f: &Fixture, rel: &str, bytes: usize) -> PathBuf {
        let path = f.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![7u8; bytes]).unwrap();
        backdate(&path);
        path
    }

    fn set_created(f: &Fixture, id: &str, created: i64) {
        lock(&f.lib.inner.db)
            .execute(
                "UPDATE clips SET created = ?1 WHERE id = ?2",
                params![created, id],
            )
            .unwrap();
    }

    #[test]
    fn resolve_rejects_escapes() {
        let f = fixture();
        put(&f, "Game/a.mp4", 10);
        assert!(f.lib.resolve("Game/a.mp4").is_ok());
        for bad in [
            "",
            "..",
            "../x.mp4",
            "Game/../../x.mp4",
            "/abs/x.mp4",
            "C:/x.mp4",
            "C:\\x.mp4",
            "Game\\a.mp4",
            "Game//a.mp4",
            "./a.mp4",
            "Game/a.mp4:stream",
            "Game/a.txt",
            "Game/...mp4/",
            "x/y.mp4.",
        ] {
            assert!(f.lib.resolve(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn scan_list_and_filter() {
        let f = fixture();
        put(&f, "Valorant/one.mp4", 100);
        put(&f, "Valorant/two_recording.mp4", 200);
        put(&f, "Minecraft/three.mkv", 300);
        put(&f, "loose.mp4", 50);
        put(&f, "Valorant/notes.txt", 5);
        let report = f.lib.scan().unwrap();
        assert_eq!(report.added, 4);
        assert!(!f.lib.scan().unwrap().changed());

        let all = f.lib.list(&ClipFilter::default()).unwrap();
        assert_eq!(all.len(), 4);
        let rec = all.iter().find(|c| c.id == "Valorant/two_recording.mp4").unwrap();
        assert_eq!(rec.kind, "recording");
        assert_eq!(rec.game, "Valorant");
        assert_eq!(all.iter().find(|c| c.id == "loose.mp4").unwrap().game, "Other");
        assert!(rec.url.starts_with("/media/Valorant/two_recording.mp4?v="));
        assert_eq!(rec.thumb, None);

        let by = |f2: ClipFilter| f.lib.list(&f2).unwrap().len();
        assert_eq!(
            by(ClipFilter {
                game: Some("Valorant".into()),
                ..Default::default()
            }),
            2
        );
        assert_eq!(
            by(ClipFilter {
                kind: Some("recording".into()),
                ..Default::default()
            }),
            1
        );
        assert_eq!(
            by(ClipFilter {
                q: Some("THREE".into()),
                ..Default::default()
            }),
            1
        );
        assert_eq!(
            by(ClipFilter {
                favorite: true,
                ..Default::default()
            }),
            0
        );

        f.lib
            .update(
                "loose.mp4",
                None,
                Some(true),
                Some(&["funny".into(), "Funny".into(), " ".into()]),
            )
            .unwrap();
        assert_eq!(
            by(ClipFilter {
                favorite: true,
                ..Default::default()
            }),
            1
        );
        assert_eq!(
            by(ClipFilter {
                shelf: Some("funny".into()),
                ..Default::default()
            }),
            1
        );
        assert_eq!(f.lib.shelves().unwrap(), vec!["funny"]);
        let games = f.lib.games().unwrap();
        assert_eq!(
            games.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            ["Minecraft", "Other", "Valorant"]
        );
        assert_eq!(games[2].size_bytes, 300);

        fs::remove_file(f.root.join("Valorant/one.mp4")).unwrap();
        let report = f.lib.scan().unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(f.lib.list(&ClipFilter::default()).unwrap().len(), 3);
    }

    #[test]
    fn markers_are_read_from_the_sidecar() {
        let f = fixture();
        let path = put(&f, "G/a.mp4", 10);
        fs::write(markers_path(&path), r#"{"markers":[20.0,12.5]}"#).unwrap();
        f.lib.scan().unwrap();
        assert_eq!(f.lib.get("G/a.mp4").unwrap().markers, vec![12.5, 20.0]);
    }

    #[test]
    fn rename_moves_sidecars_and_keeps_favorite() {
        let f = fixture();
        let path = put(&f, "G/old.mp4", 10);
        fs::write(markers_path(&path), r#"{"markers":[1.0]}"#).unwrap();
        f.lib.scan().unwrap();
        f.lib.update("G/old.mp4", None, Some(true), None).unwrap();
        let thumb = f.lib.thumbs_dir().join(thumb_file_name("G/old.mp4", false));
        fs::write(&thumb, b"jpg").unwrap();

        let clip = f
            .lib
            .update("G/old.mp4", Some("  New name.mp4 "), None, None)
            .unwrap();
        assert_eq!(clip.id, "G/New name.mp4");
        assert_eq!(clip.title, "New name");
        assert!(clip.favorite);
        assert_eq!(clip.markers, vec![1.0]);
        assert!(!path.exists());
        assert!(f.root.join("G/New name.mp4").exists());
        assert!(f.root.join("G/New name.mp4.markers.json").exists());
        assert!(!thumb.exists());
        assert!(
            f.lib
                .thumbs_dir()
                .join(thumb_file_name("G/New name.mp4", false))
                .exists()
        );
        assert!(f.lib.get("G/old.mp4").is_err());

        put(&f, "G/other.mp4", 5);
        f.lib.scan().unwrap();
        for bad in ["", "a/b", "x?", "CON", "ends."] {
            assert!(
                f.lib.update("G/other.mp4", Some(bad), None, None).is_err(),
                "{bad:?}"
            );
        }
        let err = f
            .lib
            .update("G/other.mp4", Some("New name"), None, None)
            .unwrap_err();
        assert!(matches!(
            err.downcast_ref::<LibraryError>(),
            Some(LibraryError::Invalid(_))
        ));
    }

    #[test]
    fn trash_removes_file_sidecar_and_row() {
        let f = fixture();
        let path = put(&f, "G/a.mp4", 10);
        fs::write(markers_path(&path), "{}").unwrap();
        f.lib.scan().unwrap();
        f.lib.trash("G/a.mp4").unwrap();
        assert!(!path.exists() && !markers_path(&path).exists());
        assert!(f.lib.list(&ClipFilter::default()).unwrap().is_empty());
        let err = f.lib.trash("G/a.mp4").unwrap_err();
        assert!(matches!(
            err.downcast_ref::<LibraryError>(),
            Some(LibraryError::NotFound(_))
        ));
    }

    #[test]
    fn cleanup_by_age_oldest_first_and_spares_favorites() {
        let f = fixture();
        for name in ["old", "older", "fav", "new"] {
            put(&f, &format!("G/{name}.mp4"), 1000);
        }
        f.lib.scan().unwrap();
        let now = now_millis() / 1000;
        let day = 86_400;
        set_created(&f, "G/older.mp4", now - 100 * day);
        set_created(&f, "G/old.mp4", now - 50 * day);
        set_created(&f, "G/fav.mp4", now - 200 * day);
        set_created(&f, "G/new.mp4", now - day);
        f.lib.update("G/fav.mp4", None, Some(true), None).unwrap();

        let cfg = ClipsConfig {
            max_age_days: 40,
            ..ClipsConfig::default()
        };
        let (n, freed) = f.lib.cleanup(&cfg).unwrap();
        assert_eq!((n, freed), (2, 2000));
        let left: Vec<String> = f
            .lib
            .list(&ClipFilter::default())
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(left, ["G/new.mp4", "G/fav.mp4"]);
        assert!(!f.root.join("G/old.mp4").exists());
        assert_eq!(f.lib.cleanup(&ClipsConfig::default()).unwrap(), (0, 0));
    }

    #[test]
    fn cleanup_preview_lists_what_would_go_without_deleting() {
        let f = fixture();
        for name in ["soon", "later", "fav"] {
            put(&f, &format!("G/{name}.mp4"), 1000);
        }
        f.lib.scan().unwrap();
        let now = now_millis() / 1000;
        let day = 86_400;
        set_created(&f, "G/soon.mp4", now - 30 * day + 3600); // 30 days old in an hour
        set_created(&f, "G/later.mp4", now - 20 * day);
        set_created(&f, "G/fav.mp4", now - 90 * day);
        f.lib.update("G/fav.mp4", None, Some(true), None).unwrap();
        let cfg = ClipsConfig {
            max_age_days: 30,
            ..ClipsConfig::default()
        };
        let ids = |ahead_h: u64| -> Vec<String> {
            f.lib
                .cleanup_preview(&cfg, std::time::Duration::from_secs(ahead_h * 3600))
                .unwrap()
                .into_iter()
                .map(|c| c.id)
                .collect()
        };
        assert!(ids(0).is_empty());
        assert_eq!(ids(24), ["G/soon.mp4"]);
        assert_eq!(ids(24 * 11), ["G/soon.mp4", "G/later.mp4"]);
        assert!(f.root.join("G/soon.mp4").exists());
        let none = f
            .lib
            .cleanup_preview(
                &ClipsConfig::default(),
                std::time::Duration::from_secs(86_400 * 999),
            )
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn cleanup_does_nothing_when_the_folder_is_missing() {
        let f = fixture();
        put(&f, "G/a.mp4", 10);
        f.lib.scan().unwrap();
        fs::remove_dir_all(&f.root).unwrap();
        let cfg = ClipsConfig {
            max_age_days: 1,
            ..ClipsConfig::default()
        };
        assert_eq!(f.lib.cleanup(&cfg).unwrap(), (0, 0));
        // The index keeps its rows, so favorites survive an unplugged drive.
        assert_eq!(f.lib.list(&ClipFilter::default()).unwrap().len(), 1);
    }

    #[test]
    fn a_separate_recordings_folder_is_indexed_with_its_own_ids() {
        let f = fixture();
        put(&f, "G/a.mp4", 10);
        let rec = f.root.parent().unwrap().join("recs");
        fs::create_dir_all(rec.join("G")).unwrap();
        let file = rec.join("G").join("b_recording.mp4");
        fs::write(&file, vec![7u8; 20]).unwrap();
        backdate(&file);
        f.lib.set_recordings_dir(Some(rec.clone()));
        f.lib.scan().unwrap();
        let mut ids: Vec<String> = f
            .lib
            .list(&ClipFilter::default())
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            [":recordings/G/b_recording.mp4".to_string(), "G/a.mp4".to_string()]
        );
        assert_eq!(f.lib.resolve(":recordings/G/b_recording.mp4").unwrap(), file);
        assert!(f.lib.resolve(":recordings/../x.mp4").is_err());
        // Saved right now: lands under the recordings prefix too.
        let again = f.lib.add_file(&file).unwrap();
        assert_eq!(again.id, ":recordings/G/b_recording.mp4");
        // An unplugged recordings drive keeps its rows; the clips folder still syncs.
        // (The thumbnail worker may still have the file open for a moment.)
        let away = rec.with_extension("away");
        let moved = (0..50).any(|_| {
            fs::rename(&rec, &away).is_ok() || {
                std::thread::sleep(Duration::from_millis(100));
                false
            }
        });
        assert!(moved);
        f.lib.scan().unwrap();
        assert_eq!(f.lib.list(&ClipFilter::default()).unwrap().len(), 2);
        // Back to one folder: the recordings' rows go.
        f.lib.set_recordings_dir(None);
        assert_eq!(f.lib.list(&ClipFilter::default()).unwrap().len(), 1);
        assert!(f.lib.resolve(":recordings/G/b_recording.mp4").is_err());
    }

    #[test]
    fn titles_can_not_start_with_a_dot() {
        assert!(clean_title(".hidden", "mp4").is_err());
        assert!(clean_title("  .hidden", "mp4").is_err());
        assert!(clean_title(".mp4", "mp4").is_err());
        assert_eq!(clean_title("  fine.name ", "mp4").unwrap(), "fine.name");
        let f = fixture();
        put(&f, "G/a.mp4", 10);
        f.lib.scan().unwrap();
        assert!(f.lib.update("G/a.mp4", Some(".x"), None, None).is_err());
        assert!(f.root.join("G/a.mp4").exists());
    }

    #[test]
    fn cleanup_by_size_deletes_oldest_until_under_the_limit() {
        let f = fixture();
        let gb = 1u64 << 30;
        // Sparse files: only the metadata matters.
        for (name, age) in [("a", 30), ("b", 20), ("c", 10), ("d", 5)] {
            let p = put(&f, &format!("G/{name}.mp4"), 1);
            fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_len(gb)
                .unwrap();
            backdate(&p);
            let _ = age;
        }
        f.lib.scan().unwrap();
        let now = now_millis() / 1000;
        for (name, age) in [("a", 30), ("b", 20), ("c", 10), ("d", 5)] {
            set_created(&f, &format!("G/{name}.mp4"), now - age * 86_400);
        }
        f.lib.update("G/a.mp4", None, Some(true), None).unwrap();
        let cfg = ClipsConfig {
            max_total_gb: 2,
            ..ClipsConfig::default()
        };
        let (n, freed) = f.lib.cleanup(&cfg).unwrap();
        // 4 GB -> 2 GB: the favorite `a` stays, so `b` and `c` go, then it fits.
        assert_eq!((n, freed), (2, 2 * gb));
        let left: Vec<String> = f
            .lib
            .list(&ClipFilter::default())
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(left, ["G/d.mp4", "G/a.mp4"]);
    }

    #[test]
    fn disk_reports_something() {
        let f = fixture();
        put(&f, "G/a.mp4", 123);
        f.lib.scan().unwrap();
        let (free, total, used) = f.lib.disk().unwrap();
        assert!(total > 0 && free <= total);
        assert_eq!(used, 123);
    }

    #[test]
    fn add_file_adds_one_clip_and_rejects_outsiders() {
        let f = fixture();
        let path = put(&f, "G/a.mp4", 10);
        let clip = f.lib.add_file(&path).unwrap();
        assert_eq!(clip.id, "G/a.mp4");
        assert_eq!(f.lib.add_file(&path).unwrap(), clip);
        let outside = f._tmp.path().join("x.mp4");
        fs::write(&outside, b"x").unwrap();
        assert!(f.lib.add_file(&outside).is_err());
    }

    #[test]
    fn url_encoding() {
        assert_eq!(encode_path("A B/c#d.mp4"), "A%20B/c%23d.mp4");
    }

    #[test]
    fn thumbnails_are_generated_for_a_real_video() {
        let src = Path::new(
            r"C:\Users\piete\AppData\Local\Temp\claude\d109dc1bc7f1c277\1434c99c-b3c8-476a-83af-9c09a9d924d1\scratchpad\clipr_spike_30s_h264.mp4",
        );
        if !src.exists() {
            return;
        }
        let f = fixture();
        let dst = f.root.join("Spike").join("clip.mp4");
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(src, &dst).unwrap();
        let clip = f.lib.add_file(&dst).unwrap();
        assert!(clip.duration_secs.is_some());
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let c = f.lib.get(&clip.id).unwrap();
            if c.thumb.is_some() {
                assert!(c.sprite.is_some(), "sprite should exist too");
                break;
            }
            assert!(Instant::now() < deadline, "no thumbnail after 60 s: {c:?}");
            std::thread::sleep(Duration::from_millis(200));
        }
        let thumb = f.lib.thumbs_dir().join(thumb_file_name(&clip.id, false));
        let sprite = f.lib.thumbs_dir().join(thumb_file_name(&clip.id, true));
        assert!(fs::metadata(&thumb).unwrap().len() > 500);
        assert!(fs::metadata(&sprite).unwrap().len() > 500);
        // Keep a copy for a look by eye.
        let out = Path::new(
            r"C:\Users\piete\AppData\Local\Temp\claude\d109dc1bc7f1c277\1434c99c-b3c8-476a-83af-9c09a9d924d1\scratchpad",
        );
        if out.is_dir() {
            let _ = fs::copy(&thumb, out.join("library_test_thumb.jpg"));
            let _ = fs::copy(&sprite, out.join("library_test_sprite.jpg"));
        }
    }
}
