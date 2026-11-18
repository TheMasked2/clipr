//! Performance numbers for Home: one sample a second over the last two minutes, from the capture's frame
//! counters and the process's own CPU and memory use.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

use super::Stats;

/// How many samples are kept (one a second).
const KEEP: usize = 120;

/// One second of capture.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Sample {
    /// When it was taken, in ms since 1970.
    pub at_ms: u64,
    /// Frames encoded per second (new pictures plus repeats).
    pub fps: f64,
    /// Frames with a new picture per second; the rest repeat the last one (the screen didn't change, or the
    /// game didn't deliver one in time).
    pub fresh: f64,
    /// Frames encoded later than their slot, and slots skipped altogether, this second.
    pub late: u64,
    pub dropped: u64,
    /// Average time the capture thread spent on a frame (scaling it and handing it to the encoder), in ms.
    pub frame_ms: f64,
    /// Video bitrate actually written, in kbit/s.
    pub kbps: f64,
    /// clipr's CPU use, in % of the whole machine.
    pub cpu: f64,
    /// clipr's memory (working set), in MB.
    pub mem_mb: f64,
    /// What the replay buffer holds: seconds of video and MB in memory.
    pub buffer_secs: f64,
    pub buffer_mb: f64,
    /// Whether capture ran during this second.
    pub capturing: bool,
}

/// The samples the web UI reads.
pub type History = Arc<Mutex<VecDeque<Sample>>>;

/// Takes the samples (on the supervisor thread).
pub struct Sampler {
    history: History,
    last_at: Instant,
    last_stats: Stats,
    last_cpu: u64,
}

impl Sampler {
    pub fn new(history: History) -> Self {
        Self {
            history,
            last_at: Instant::now(),
            last_stats: Stats::default(),
            last_cpu: process_cpu_100ns(),
        }
    }

    /// The capture's counters start again at zero (a new pipeline).
    pub fn counters_reset(&mut self) {
        self.last_stats = Stats::default();
    }

    /// Records one sample from the counters as they are now. `stats` restarts at zero with each pipeline;
    /// a counter that went down counts from zero.
    pub fn sample(&mut self, stats: Stats, capturing: bool, buffer: (f64, usize)) {
        let now = Instant::now();
        let secs = now.duration_since(self.last_at).as_secs_f64().max(0.001);
        let cpu_now = process_cpu_100ns();
        let delta = |now: u64, then: u64| now.checked_sub(then).unwrap_or(now);
        let d = Stats {
            fresh: delta(stats.fresh, self.last_stats.fresh),
            repeated: delta(stats.repeated, self.last_stats.repeated),
            late: delta(stats.late, self.last_stats.late),
            dropped: delta(stats.dropped, self.last_stats.dropped),
            work_100ns: delta(stats.work_100ns, self.last_stats.work_100ns),
            video_bytes: delta(stats.video_bytes, self.last_stats.video_bytes),
        };
        let frames = d.fresh + d.repeated;
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
        let cpu = delta(cpu_now, self.last_cpu) as f64 / 1e7 / secs / cores * 100.0;
        let sample = Sample {
            at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            fps: frames as f64 / secs,
            fresh: d.fresh as f64 / secs,
            late: d.late,
            dropped: d.dropped,
            frame_ms: if frames > 0 {
                d.work_100ns as f64 / frames as f64 / 1e4
            } else {
                0.0
            },
            kbps: d.video_bytes as f64 * 8.0 / 1000.0 / secs,
            cpu: (cpu * 10.0).round() / 10.0,
            mem_mb: process_memory_bytes() as f64 / 1_048_576.0,
            buffer_secs: buffer.0,
            buffer_mb: buffer.1 as f64 / 1_048_576.0,
            capturing,
        };
        self.last_at = now;
        self.last_stats = stats;
        self.last_cpu = cpu_now;
        let mut h = self.history.lock().unwrap();
        if h.len() == KEEP {
            h.pop_front();
        }
        h.push_back(sample);
    }
}

/// CPU time this process has used, kernel plus user, in 100 ns units.
fn process_cpu_100ns() -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let mut t = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let [created, exited, kernel, user] = &mut t;
    // SAFETY: the pseudo handle needs no closing; the out pointers are valid for the call.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), created, exited, kernel, user) };
    if ok == 0 {
        return 0;
    }
    let get = |f: &FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    get(&t[2]) + get(&t[3])
}

/// This process's working set in bytes.
fn process_memory_bytes() -> usize {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: zeroed is a valid PROCESS_MEMORY_COUNTERS; cb tells the call its size.
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) == 0 {
            return 0;
        }
        c.WorkingSetSize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_rates_and_keeps_two_minutes() {
        let history = History::default();
        let mut s = Sampler::new(history.clone());
        s.last_at = Instant::now() - std::time::Duration::from_secs(1);
        let stats = Stats {
            fresh: 50,
            repeated: 10,
            late: 2,
            dropped: 0,
            work_100ns: 60 * 30_000, // 3 ms a frame
            video_bytes: 1_875_000,  // 15 Mbit
        };
        s.sample(stats, true, (30.0, 64 << 20));
        let first = history.lock().unwrap()[0].clone();
        assert!((first.fps - 60.0).abs() < 2.0, "{first:?}");
        assert!((first.frame_ms - 3.0).abs() < 0.01);
        assert!((first.kbps - 15_000.0).abs() < 500.0);
        assert_eq!((first.late, first.buffer_mb), (2, 64.0));
        // A new pipeline starts its counters at zero: that's not a negative rate.
        s.sample(Stats::default(), false, (0.0, 0));
        assert_eq!(history.lock().unwrap()[1].late, 0);
        for _ in 0..200 {
            s.sample(Stats::default(), false, (0.0, 0));
        }
        assert_eq!(history.lock().unwrap().len(), KEEP);
    }
}
