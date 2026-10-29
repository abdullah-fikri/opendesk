//! CPU time and memory of this process: `GetProcessTimes` / `K32GetProcessMemoryInfo` on Windows,
//! `/proc/self/stat` and `/proc/self/statm` on Linux. A few syscalls per sample; callers sample at most once a
//! second.

use std::time::{Duration, Instant};

use crate::domain::perf_stats::ProcessUsage;

/// CPU usage since the previous [`UsageSampler::sample`], plus the current memory.
pub struct UsageSampler {
    last: Option<(Instant, Duration)>,
    cores: usize,
}

impl UsageSampler {
    /// A sampler whose first sample only sets the baseline.
    pub fn new() -> Self {
        Self { last: None, cores: std::thread::available_parallelism().map_or(1, |n| n.get()) }
    }

    /// Usage since the previous call; None on the first call or where the OS gives no numbers.
    pub fn sample(&mut self, now: Instant) -> Option<ProcessUsage> {
        let (cpu, ram) = read()?;
        let prev = self.last.replace((now, cpu))?;
        let (wall, used) = (now.saturating_duration_since(prev.0), cpu.saturating_sub(prev.1));
        Some(ProcessUsage::from_times(used, wall, self.cores, ram))
    }
}

/// (user + kernel CPU time, working set bytes).
#[cfg(windows)]
fn read() -> Option<(Duration, u64)> {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct MemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut MemoryCounters, cb: u32) -> i32;
    }

    let ticks = |t: &FileTime| (u64::from(t.high) << 32) | u64::from(t.low);
    let (mut creation, mut exit, mut kernel, mut user) = Default::default();
    let mut mem = MemoryCounters { cb: std::mem::size_of::<MemoryCounters>() as u32, ..Default::default() };
    // SAFETY: the pseudo-handle of this process needs no closing; both calls only write the structs given.
    let ok = unsafe {
        let me = GetCurrentProcess();
        GetProcessTimes(me, &mut creation, &mut exit, &mut kernel, &mut user) != 0
            && K32GetProcessMemoryInfo(me, &mut mem, mem.cb) != 0
    };
    // FILETIME counts 100 ns units.
    ok.then(|| (Duration::from_nanos((ticks(&kernel) + ticks(&user)) * 100), mem.working_set_size as u64))
}

/// (user + system CPU time, resident set bytes).
#[cfg(target_os = "linux")]
fn read() -> Option<(Duration, u64)> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    // SAFETY: sysconf only reads configuration values.
    let (tick, page) = unsafe { (libc::sysconf(libc::_SC_CLK_TCK), libc::sysconf(libc::_SC_PAGESIZE)) };
    parse_proc(&stat, &statm, tick, page)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn read() -> Option<(Duration, u64)> {
    None
}

/// Parses `/proc/self/stat` (utime and stime, fields 14 and 15, in clock ticks) and `/proc/self/statm`
/// (resident pages, the second field).
#[cfg(any(target_os = "linux", test))]
fn parse_proc(stat: &str, statm: &str, tick_hz: i64, page_size: i64) -> Option<(Duration, u64)> {
    // The command name (field 2) may contain spaces and parentheses; the fields after it never do.
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let hz = u64::try_from(tick_hz).ok().filter(|&h| h > 0)?;
    let cpu = Duration::from_micros((utime + stime) * 1_000_000 / hz);
    Some((cpu, resident * u64::try_from(page_size).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_files_parse() {
        let stat = "4242 (open desk) S 1 4242 4242 0 -1 4194560 2000 0 0 0 250 50 0 0 20 0 12 0 100 0 0";
        assert_eq!(
            parse_proc(stat, "50000 38400 9000 100 0 20000 0", 100, 4096),
            Some((Duration::from_secs(3), 157_286_400))
        );
        assert_eq!(parse_proc("garbage", "1 2", 100, 4096), None);
        assert_eq!(parse_proc(stat, "50000 38400", 0, 4096), None);
    }

    #[test]
    fn sampler_reports_from_the_second_call() {
        let mut s = UsageSampler::new();
        let t0 = Instant::now();
        assert_eq!(s.sample(t0), None);
        let u = s.sample(t0 + Duration::from_secs(1)).expect("this OS reports process usage");
        assert!((0.0..=100.0).contains(&u.cpu_pct) && u.ram_mb > 1.0, "{u:?}");
    }
}
