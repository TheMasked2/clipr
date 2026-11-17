//! Opt-in crash reports (`privacy.crash_reports`). Nothing is ever sent by itself.
//!
//! When it's on, a crash (a Rust panic, or a native exception such as an access violation in a GPU driver)
//! leaves a small record (`crashes/<id>.json`) and a minidump (`crashes/<id>.dmp`) in the data folder. On
//! the next start, Home shows what happened with a report the user can review, copy, or open as a
//! pre-filled GitHub issue. The report has the user name, PC name, profile paths and LAN addresses removed.
//!
//! Release builds abort on panic and carry no symbols, so stacks are written as `module+offset`; they
//! resolve against the release's PDB.

use std::ffi::c_void;
use std::fmt::Write as _;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::Diagnostics::Debug::{
    EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MiniDumpNormal, MiniDumpWithThreadInfo,
    MiniDumpWriteDump, RtlCaptureStackBackTrace, SetUnhandledExceptionFilter,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId};

/// Where issues are filed.
const NEW_ISSUE: &str = "https://github.com/TheMasked2/clipr/issues/new";
/// Older crash records than the newest this many are deleted.
const KEEP: usize = 10;
/// GitHub refuses very long URLs; the issue body is shortened to stay under this.
const MAX_ISSUE_URL: usize = 7500;
/// Lines of log before the crash in the report.
const LOG_LINES: usize = 40;
/// Stack frames in the report.
const MAX_FRAMES: usize = 40;

const EXCEPTION_EXECUTE_HANDLER: i32 = 1;
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

static DIR: OnceLock<PathBuf> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Set by the first crash, so a crash inside the handler doesn't recurse.
static HANDLING: AtomicBool = AtomicBool::new(false);

/// What's written at crash time. Facts about the machine are added when the report is built.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub version: String,
    /// Unix seconds.
    pub time: u64,
    /// `panic` or `exception`.
    pub kind: String,
    pub thread: String,
    /// The panic message, or the exception's name.
    pub message: String,
    /// `file:line` of a panic.
    pub location: Option<String>,
    /// The exception code (e.g. `0xC0000005`).
    pub code: Option<u32>,
    /// `module+offset` of the faulting instruction.
    pub at: Option<String>,
    /// `module+offset` per frame, innermost first.
    pub frames: Vec<String>,
}

/// Turns crash capture on: a panic hook (after the existing one) and an unhandled-exception filter. Records
/// are written only while [`set_enabled`] is on.
pub fn install(dir: PathBuf, enabled: bool) {
    let _ = DIR.set(dir);
    set_enabled(enabled);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        on_panic(info);
    }));
    // SAFETY: installs a process-wide filter function; it stays valid for the whole run.
    unsafe { SetUnhandledExceptionFilter(Some(on_exception)) };
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
}

/// For testing the whole path: `CLIPR_CRASH_TEST=panic` or `=access` crashes clipr on purpose a few seconds
/// after start (on a thread of its own).
pub fn crash_test_from_env() {
    let Ok(kind) = std::env::var("CLIPR_CRASH_TEST") else {
        return;
    };
    tracing::warn!("CLIPR_CRASH_TEST={kind}: crashing on purpose in 5 s");
    let _ = std::thread::Builder::new()
        .name("clipr-crash-test".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if kind == "access" {
                // SAFETY: deliberately not safe: an access violation is the point of this test.
                unsafe { std::ptr::write_volatile(std::ptr::dangling_mut::<u8>().wrapping_sub(1), 1) };
            }
            panic!("CLIPR_CRASH_TEST: a test crash");
        });
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `name+0x1234` for an address inside a loaded module, else the bare address.
fn module_offset(addr: usize) -> String {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS reads `addr` as an address, not a string; UNCHANGED_REFCOUNT needs no release.
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            addr as *const u16,
            &mut module,
        )
    };
    if ok == 0 || module.is_null() {
        return format!("0x{addr:x}");
    }
    let mut buf = [0u16; 260];
    // SAFETY: `buf` is a valid buffer of the given length.
    let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    // The file name only: the folder can hold the user name.
    let name = path.rsplit(['\\', '/']).next().unwrap_or("?");
    format!("{name}+0x{:x}", addr.wrapping_sub(module as usize))
}

fn stack() -> Vec<String> {
    let mut ptrs = [std::ptr::null_mut::<c_void>(); 62];
    // SAFETY: `ptrs` has room for the 62 frames asked for; the hash pointer may be null.
    let n = unsafe { RtlCaptureStackBackTrace(1, 62, ptrs.as_mut_ptr(), std::ptr::null_mut()) };
    ptrs[..usize::from(n)]
        .iter()
        .map(|p| module_offset(*p as usize))
        .collect()
}

fn thread_name() -> String {
    std::thread::current()
        .name()
        .map_or_else(|| "unnamed".to_string(), str::to_string)
}

fn record_paths(dir: &Path) -> (String, PathBuf, PathBuf) {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let id = format!("{millis}");
    (
        id.clone(),
        dir.join(format!("{id}.json")),
        dir.join(format!("{id}.dmp")),
    )
}

/// Writes a minidump of this process. Best effort: it runs while crashing.
fn write_dump(path: &Path, pointers: *const EXCEPTION_POINTERS) {
    let Ok(file) = std::fs::File::create(path) else {
        return;
    };
    let info = MINIDUMP_EXCEPTION_INFORMATION {
        // SAFETY: no preconditions.
        ThreadId: unsafe { GetCurrentThreadId() },
        ExceptionPointers: pointers.cast_mut(),
        ClientPointers: 0,
    };
    let exception = if pointers.is_null() {
        std::ptr::null()
    } else {
        &raw const info
    };
    // SAFETY: valid process/file handles; `exception` is null or points to `info`, alive for the call.
    let ok = unsafe {
        MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            file.as_raw_handle(),
            MiniDumpNormal | MiniDumpWithThreadInfo,
            exception,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    drop(file);
    if ok == 0 {
        let _ = std::fs::remove_file(path);
    }
}

fn save(record: &Record, pointers: *const EXCEPTION_POINTERS) {
    let Some(dir) = DIR.get() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let (_, json_path, dump_path) = record_paths(dir);
    write_dump(&dump_path, pointers);
    if let Ok(text) = serde_json::to_vec_pretty(record) {
        let _ = std::fs::write(json_path, text);
    }
}

fn on_panic(info: &std::panic::PanicHookInfo<'_>) {
    if !ENABLED.load(Ordering::SeqCst) || HANDLING.swap(true, Ordering::SeqCst) {
        return;
    }
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".into());
    let record = Record {
        version: clipr_core::VERSION.into(),
        time: now(),
        kind: "panic".into(),
        thread: thread_name(),
        message,
        location: info.location().map(|l| format!("{}:{}", l.file(), l.line())),
        frames: stack(),
        ..Record::default()
    };
    save(&record, std::ptr::null());
    // Debug builds unwind, and a caught panic isn't the end of the run: allow the next one to be recorded.
    if cfg!(debug_assertions) {
        HANDLING.store(false, Ordering::SeqCst);
    }
}

unsafe extern "system" fn on_exception(pointers: *const EXCEPTION_POINTERS) -> i32 {
    if !ENABLED.load(Ordering::SeqCst) || HANDLING.swap(true, Ordering::SeqCst) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    // SAFETY: Windows passes valid exception pointers to the filter.
    let (code, addr) = unsafe {
        let rec = (*pointers).ExceptionRecord;
        if rec.is_null() {
            (0, 0)
        } else {
            ((*rec).ExceptionCode as u32, (*rec).ExceptionAddress as usize)
        }
    };
    let at = module_offset(addr);
    let mut frames = stack();
    // Start at the faulting frame: the ones before it are this handler and Windows' exception dispatch.
    if let Some(i) = frames.iter().position(|f| *f == at) {
        frames.drain(..i);
    }
    let record = Record {
        version: clipr_core::VERSION.into(),
        time: now(),
        kind: "exception".into(),
        thread: thread_name(),
        message: exception_name(code).into(),
        code: Some(code),
        at: Some(at),
        frames,
        ..Record::default()
    };
    save(&record, pointers);
    // Recorded: end the process quietly (no "stopped working" dialog; its report wouldn't reach us).
    EXCEPTION_EXECUTE_HANDLER
}

fn exception_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "access violation",
        0xC000_00FD => "stack overflow",
        0xC000_0409 => "stack buffer overrun",
        0xC000_001D => "illegal instruction",
        0xC000_0094 => "integer division by zero",
        0xC000_0374 => "heap corruption",
        0xE06D_7363 => "C++ exception",
        0x8000_0003 => "breakpoint",
        _ => "unhandled exception",
    }
}

// ---- reading records and building reports ---------------------------------------------------------------

/// A crash id is the record's file stem: digits only.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())
}

/// The records in `dir`, newest first. Deletes all but the newest [`KEEP`] (and their dumps).
pub fn list(dir: &Path) -> Vec<(String, Record)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Record)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()? != "json" {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_string();
            if !valid_id(&id) {
                return None;
            }
            let record = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            Some((id, record))
        })
        .collect();
    out.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| b.0.cmp(&a.0)));
    for (id, _) in out.drain(KEEP.min(out.len())..) {
        let _ = dismiss(dir, &id);
    }
    out
}

/// Deletes a record and its dump.
pub fn dismiss(dir: &Path, id: &str) -> std::result::Result<(), String> {
    if !valid_id(id) {
        return Err("No such crash report.".into());
    }
    let json = dir.join(format!("{id}.json"));
    if !json.is_file() {
        return Err("No such crash report.".into());
    }
    let _ = std::fs::remove_file(dir.join(format!("{id}.dmp")));
    std::fs::remove_file(&json).map_err(|e| format!("Couldn't delete the crash report: {e}"))
}

pub fn dump_path(dir: &Path, id: &str) -> Option<PathBuf> {
    valid_id(id)
        .then(|| dir.join(format!("{id}.dmp")))
        .filter(|p| p.is_file())
}

/// One line saying what happened.
pub fn summary(r: &Record) -> String {
    match r.kind.as_str() {
        "panic" => {
            let first = r.message.lines().next().unwrap_or_default();
            let mut msg: String = first.chars().take(160).collect();
            if msg.len() < first.len() {
                msg.push('…');
            }
            match &r.location {
                Some(at) => format!("panic on thread {}: {msg} (at {at})", r.thread),
                None => format!("panic on thread {}: {msg}", r.thread),
            }
        }
        _ => format!(
            "{} (0x{:08X}) in {} on thread {}",
            r.message,
            r.code.unwrap_or(0),
            r.at.as_deref().unwrap_or("?"),
            r.thread
        ),
    }
}

/// What gets removed from a report: the profile folder, the user and PC names.
#[derive(Clone, Debug, Default)]
pub struct Scrub {
    pub profile: Option<String>,
    pub user: Option<String>,
    pub computer: Option<String>,
}

impl Scrub {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| v.trim().len() >= 2);
        Self {
            profile: var("USERPROFILE"),
            user: var("USERNAME"),
            computer: var("COMPUTERNAME"),
        }
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        if let Some(p) = &self.profile {
            out = replace_ci(&out, p, "%USERPROFILE%");
            out = replace_ci(&out, &p.replace('\\', "/"), "%USERPROFILE%");
        }
        if let Some(u) = &self.user {
            out = replace_ci(&out, u, "<user>");
        }
        if let Some(c) = &self.computer {
            out = replace_ci(&out, c, "<pc>");
        }
        hide_ips(&out)
    }
}

/// Replaces every case-insensitive (ASCII) match of `needle`.
fn replace_ci(hay: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return hay.to_string();
    }
    let lower_hay = hay.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(hay.len());
    let mut last = 0;
    for (i, _) in lower_hay.match_indices(&lower_needle) {
        if i < last {
            continue;
        }
        out.push_str(&hay[last..i]);
        out.push_str(with);
        last = i + needle.len();
    }
    out.push_str(&hay[last..]);
    out
}

/// IPv4 addresses other than loopback and 0.0.0.0 become `<ip>`.
fn hide_ips(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let starts = bytes[i].is_ascii_digit() && (i == 0 || !is_ip_char(bytes[i - 1]));
        if starts {
            let end = (i..bytes.len())
                .find(|&j| !is_ip_char(bytes[j]))
                .unwrap_or(bytes.len());
            let candidate = &text[i..end];
            let candidate = candidate.trim_end_matches('.');
            let parts: Vec<&str> = candidate.split('.').collect();
            let is_ip = parts.len() == 4
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok());
            if is_ip {
                if candidate.starts_with("127.") || candidate == "0.0.0.0" {
                    out.push_str(candidate);
                } else {
                    out.push_str("<ip>");
                }
                i += candidate.len();
                continue;
            }
            out.push_str(&text[i..end]);
            i = end;
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_ip_char(b: u8) -> bool {
    b.is_ascii_digit() || b == b'.'
}

/// `2026-11-17T16:09:10Z` for Unix seconds (UTC), the way the log lines start.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The last log entries written up to `time` (a log entry may span lines; continuation lines stay with
/// their entry). Reads that day's log file.
pub fn log_before(log_dir: &Path, time: u64) -> Vec<String> {
    let stamp = rfc3339(time);
    let day = &stamp[..10];
    let Ok(text) = std::fs::read_to_string(log_dir.join(format!("clipr.{day}.log"))) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let mut keep = false;
    for line in text.lines() {
        let b = line.as_bytes();
        if b.len() >= 20 && b[4] == b'-' && b[10] == b'T' && line.is_char_boundary(19) {
            // To the second: a line written within the crash's second counts.
            keep = line[..19] <= stamp[..19];
        }
        if keep {
            lines.push(line.to_string());
        }
    }
    let start = lines.len().saturating_sub(LOG_LINES);
    lines.split_off(start)
}

/// Facts about this PC for the report (read when the report is built, not while crashing).
#[derive(Clone, Debug, Default)]
pub struct Machine {
    pub os: String,
    pub gpus: Vec<String>,
}

impl Machine {
    /// Windows version and GPU names. Slow-ish (DXGI): call once.
    pub fn detect() -> Self {
        let gpus = clipr_encode::probe::list_adapters()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| !a.software && a.vendor_id != 0x1414)
            .map(|a| a.name)
            .collect();
        Self { os: os_line(), gpus }
    }
}

/// `Windows 11 24H2 (build 26100.2314)` from the registry.
fn os_line() -> String {
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
    };
    let key: Vec<u16> = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\0"
        .encode_utf16()
        .collect();
    let string = |name: &str| -> Option<String> {
        let name: Vec<u16> = format!("{name}\0").encode_utf16().collect();
        let mut buf = [0u16; 128];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: NUL-terminated names; `buf` holds `size` bytes.
        let err = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        (err == 0).then(|| {
            let n = (size as usize / 2).min(buf.len());
            String::from_utf16_lossy(&buf[..n])
                .trim_end_matches('\0')
                .to_string()
        })
    };
    let ubr = {
        let name: Vec<u16> = "UBR\0".encode_utf16().collect();
        let mut value = 0u32;
        let mut size = 4u32;
        // SAFETY: NUL-terminated names; `value` holds 4 bytes.
        let err = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut value).cast(),
                &mut size,
            )
        };
        (err == 0).then_some(value)
    };
    let build = string("CurrentBuild").unwrap_or_default();
    let name = if build.parse::<u32>().is_ok_and(|b| b >= 22_000) {
        "Windows 11"
    } else {
        "Windows 10"
    };
    let mut line = name.to_string();
    if let Some(v) = string("DisplayVersion") {
        line.push(' ');
        line.push_str(&v);
    }
    match ubr {
        Some(u) => line.push_str(&format!(" (build {build}.{u})")),
        None => line.push_str(&format!(" (build {build})")),
    }
    line
}

/// The report as Markdown, ready to paste into an issue. `log` is already scrubbed.
pub fn report(r: &Record, machine: &Machine, log: &[String], frames: usize) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "### What happened\n");
    let _ = writeln!(s, "clipr {} crashed: {}\n", r.version, summary(r));
    let _ = writeln!(s, "### What were you doing?\n");
    let _ = writeln!(
        s,
        "<!-- A game? Recording? Saving a clip? Changing a setting? Anything helps. -->\n"
    );
    let _ = writeln!(s, "### System\n");
    let _ = writeln!(s, "- clipr {}", r.version);
    let _ = writeln!(s, "- {}", machine.os);
    if !machine.gpus.is_empty() {
        let _ = writeln!(s, "- GPU: {}", machine.gpus.join(", "));
    }
    let _ = writeln!(s, "- When: {}\n", rfc3339(r.time));
    if frames > 0 && !r.frames.is_empty() {
        let _ = writeln!(s, "### Stack\n\n```text");
        for f in r.frames.iter().take(frames) {
            let _ = writeln!(s, "{f}");
        }
        let _ = writeln!(s, "```\n");
    }
    if !log.is_empty() {
        let _ = writeln!(s, "### Log before the crash\n\n```text");
        for line in log {
            let _ = writeln!(s, "{line}");
        }
        let _ = writeln!(s, "```");
    }
    s
}

/// Percent-encodes everything but unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A new-issue link with title and body filled in, shortened (fewer log lines, then fewer frames) to fit
/// GitHub's URL limit.
pub fn issue_url(r: &Record, machine: &Machine, log: &[String]) -> String {
    let title = format!("Crash: {}", summary(r));
    let title: String = title.chars().take(120).collect();
    let note = "\n_The report was shortened to fit. The full one is on clipr's Home page (Copy report)._\n";
    let mut log_n = log.len();
    let mut frames = MAX_FRAMES;
    loop {
        let mut body = report(r, machine, &log[log.len() - log_n..], frames);
        if log_n < log.len() || frames < MAX_FRAMES {
            body.push_str(note);
        }
        let url = format!("{NEW_ISSUE}?title={}&body={}", encode(&title), encode(&body));
        if url.len() <= MAX_ISSUE_URL || (log_n == 0 && frames == 0) {
            return url;
        }
        if log_n > 0 {
            log_n = log_n.saturating_sub(5);
        } else {
            frames = frames.saturating_sub(5);
        }
    }
}

/// The JSON `GET /api/crashes` returns for each record.
pub fn to_json(
    id: &str,
    r: &Record,
    machine: &Machine,
    log_dir: &Path,
    scrub: &Scrub,
    has_dump: bool,
) -> Value {
    let log: Vec<String> = log_before(log_dir, r.time)
        .iter()
        .map(|l| scrub.apply(l))
        .collect();
    let mut clean = r.clone();
    clean.message = scrub.apply(&r.message);
    clean.location = r.location.as_deref().map(|l| scrub.apply(l));
    json!({
        "id": id,
        "time": r.time,
        "version": r.version,
        "summary": summary(&clean),
        "report": report(&clean, machine, &log, MAX_FRAMES),
        "issue_url": issue_url(&clean, machine, &log),
        "has_dump": has_dump,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panic_record() -> Record {
        Record {
            version: "0.1.0-beta.2".into(),
            time: 1_794_931_750, // 2026-11-17T16:09:10Z
            kind: "panic".into(),
            thread: "clipr-engine".into(),
            message: "index out of bounds: the len is 3 but the index is 7\nmore".into(),
            location: Some("crates\\clipr\\src\\engine\\mod.rs:123".into()),
            frames: (0..60).map(|i| format!("clipr.exe+0x{i:x}")).collect(),
            ..Record::default()
        }
    }

    #[test]
    fn dates_match_the_log_format() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_794_931_750), "2026-11-17T16:09:10Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn summaries() {
        let p = panic_record();
        assert_eq!(
            summary(&p),
            "panic on thread clipr-engine: index out of bounds: the len is 3 but the index is 7 \
             (at crates\\clipr\\src\\engine\\mod.rs:123)"
        );
        let e = Record {
            kind: "exception".into(),
            thread: "clipr-video".into(),
            message: exception_name(0xC000_0005).into(),
            code: Some(0xC000_0005),
            at: Some("nvwgf2umx.dll+0x1a2b".into()),
            ..Record::default()
        };
        assert_eq!(
            summary(&e),
            "access violation (0xC0000005) in nvwgf2umx.dll+0x1a2b on thread clipr-video"
        );
    }

    #[test]
    fn scrubs_names_paths_and_ips() {
        let s = Scrub {
            profile: Some("C:\\Users\\Piete".into()),
            user: Some("piete".into()),
            computer: Some("GAMING-PC".into()),
        };
        let text = "saved C:\\Users\\piete\\Videos\\clipr\\a.mp4 and c:/users/PIETE/x on gaming-pc; \
                    LAN 192.168.1.50:7845, local 127.0.0.1, version 0.1.0.2, bind 0.0.0.0";
        assert_eq!(
            s.apply(text),
            "saved %USERPROFILE%\\Videos\\clipr\\a.mp4 and %USERPROFILE%/x on <pc>; \
             LAN <ip>:7845, local 127.0.0.1, version <ip>, bind 0.0.0.0"
        );
        // Nothing to scrub: unchanged.
        assert_eq!(Scrub::default().apply("é ünïcode 1.2.3"), "é ünïcode 1.2.3");
    }

    #[test]
    fn log_lines_up_to_the_crash() {
        let dir = tempfile::tempdir().unwrap();
        let log = "2026-11-17T16:09:08.1Z  INFO a\n\
                   2026-11-17T16:09:10.9Z ERROR panic: boom\n\
                   continuation of the panic\n\
                   2026-11-17T16:09:11.2Z  INFO clipr 0.1.0 starting\n\
                   2026-11-17T16:09:12.0Z  INFO later\n";
        std::fs::write(dir.path().join("clipr.2026-11-17.log"), log).unwrap();
        let lines = log_before(dir.path(), 1_794_931_750);
        assert_eq!(
            lines,
            [
                "2026-11-17T16:09:08.1Z  INFO a",
                "2026-11-17T16:09:10.9Z ERROR panic: boom",
                "continuation of the panic"
            ]
        );
        assert!(log_before(dir.path(), 0).is_empty(), "no log for that day");
    }

    #[test]
    fn issue_urls_fit_and_shorten() {
        let r = panic_record();
        let machine = Machine {
            os: "Windows 11 build 26300".into(),
            gpus: vec!["NVIDIA GeForce RTX 3070".into()],
        };
        let short = issue_url(&r, &machine, &["one line".to_string()]);
        assert!(short.starts_with(NEW_ISSUE));
        assert!(short.contains("title=Crash%3A%20panic%20on%20thread%20clipr-engine"));
        assert!(!short.contains("shortened"));

        let long: Vec<String> = (0..400).map(|i| format!("{i:04} {}", "x".repeat(80))).collect();
        let url = issue_url(&r, &machine, &long);
        assert!(url.len() <= MAX_ISSUE_URL, "{}", url.len());
        assert!(url.contains("shortened"));
        assert!(url.contains("0399"), "the newest log lines are kept");
        assert!(!url.contains("0000%20"), "the oldest go first");
    }

    #[test]
    fn report_has_the_sections() {
        let r = panic_record();
        let machine = Machine {
            os: "Windows 11".into(),
            gpus: vec![],
        };
        let text = report(&r, &machine, &["log line".into()], 3);
        for part in [
            "### What happened",
            "### System",
            "- Windows 11",
            "clipr.exe+0x2",
            "log line",
        ] {
            assert!(text.contains(part), "{part}\n{text}");
        }
        assert!(!text.contains("clipr.exe+0x3"), "frames are capped");
        assert!(!text.contains("GPU:"), "no GPU line without GPUs");
    }

    #[test]
    fn records_list_newest_first_keep_ten_and_dismiss() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..12u64 {
            let r = Record {
                time: i,
                ..Record::default()
            };
            let id = 1_700_000_000_000 + i;
            std::fs::write(
                dir.path().join(format!("{id}.json")),
                serde_json::to_vec(&r).unwrap(),
            )
            .unwrap();
            std::fs::write(dir.path().join(format!("{id}.dmp")), b"dump").unwrap();
        }
        std::fs::write(dir.path().join("notes.json"), b"{}").unwrap();
        let listed = list(dir.path());
        assert_eq!(listed.len(), KEEP);
        assert_eq!(listed[0].1.time, 11);
        assert!(!dir.path().join("1700000000000.dmp").exists(), "old dumps go too");

        let newest = listed[0].0.clone();
        assert!(dump_path(dir.path(), &newest).is_some());
        dismiss(dir.path(), &newest).unwrap();
        assert!(dump_path(dir.path(), &newest).is_none());
        assert_eq!(list(dir.path()).len(), KEEP - 1);
        assert!(dismiss(dir.path(), "..\\x").is_err());
        assert!(dismiss(dir.path(), &newest).is_err(), "already gone");
    }
}
