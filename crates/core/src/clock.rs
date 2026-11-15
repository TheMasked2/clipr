//! The one clock every capture source stamps its data with: Windows' performance counter in 100 ns units.
//!
//! WASAPI reports audio packet times on this same clock (`u64QPCPosition`), so video frames and audio
//! samples line up without any conversion.

/// Now, in 100 ns units of the performance counter.
#[cfg(windows)]
pub fn now_100ns() -> i64 {
    let (mut count, mut freq) = (0i64, 0i64);
    // SAFETY: valid out-pointers; both calls always succeed on Windows XP and later.
    unsafe {
        QueryPerformanceCounter(&mut count);
        QueryPerformanceFrequency(&mut freq);
    }
    (count as i128 * 10_000_000 / freq.max(1) as i128) as i64
}

#[cfg(windows)]
unsafe extern "system" {
    fn QueryPerformanceCounter(count: *mut i64) -> i32;
    fn QueryPerformanceFrequency(freq: *mut i64) -> i32;
}

/// Now, in 100 ns units since an arbitrary start (non-Windows builds, for tests).
#[cfg(not(windows))]
pub fn now_100ns() -> i64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    (START.get_or_init(Instant::now).elapsed().as_nanos() / 100) as i64
}

#[cfg(test)]
mod tests {
    #[test]
    fn moves_forward_in_100ns_units() {
        let a = super::now_100ns();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let d = super::now_100ns() - a;
        assert!((150_000..2_000_000).contains(&d), "{d}");
    }
}
