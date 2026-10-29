//! Per-stage timing for the periodic `host.perf` / `viewer.perf` log lines. A [`Stage`] accumulates
//! durations (count, sum, max) without allocating; [`Stage::take_summary`] reads and resets it once per
//! [`PerfWindow`].
//!
//! Host line (`host.perf`, one per window while a session is hosted):
//! - `fps`: frames sent per second; `target`: the capture loop's pacing rate.
//! - `grab`, `wait`, `encode`, `gate`: avg/max milliseconds per tick for the work of `DesktopCapturer::grab`
//!   (copy and scale, without DXGI's wait for a desktop change), that wait itself (the whole tick budget on an
//!   idle screen; not a cost), `StreamEncoder::encode` (block diff + JPEG/H.264) and the wait for the previous
//!   frame to leave (`can_send_frame`).
//! - `latency`: avg/max ms from capture to handing a sent frame to the outbox (the `Host` overlay row).
//! - `kbps`: video message bytes sent, in kilobits per second.
//! - `idle_frames`: grabs that produced nothing to send (no change); `gate_skips`: ticks skipped because the
//!   previous frame was still on its way when the tick ended.
//! - `h264`: `off`, or `on(<encoder name>)` once the motion encoder is running.
//! - `cpu`, `ram`: this process over the window (see [`ProcessUsage`]).
//!
//! Viewer line (`viewer.perf`, one per window while the session is visible and receiving video):
//! - `session`: the viewer session id; `fps`: frames handed to the UI per second.
//! - `process`: avg/max ms to decode and compose a batch into an output buffer.
//! - `handoff`: avg/max ms to pass the buffer to the UI thread.
//! - `kbps`: video message bytes received, in kilobits per second; `batches`: inbox drains decoded.
//! - `latency host/net/decode/display/total`: avg/max ms per stage of displayed frames (`domain::latency`);
//!   `-` where unmeasured (hosts without `frame-stamp`, or no clock offset yet).
//! - `cpu`, `ram`: this process over the window (see [`ProcessUsage`]).

use std::fmt;
use std::time::{Duration, Instant};

/// How often the perf lines are logged.
pub const PERF_INTERVAL: Duration = Duration::from_secs(10);
/// How often the stats overlay (the host's `host-stats` and the viewer's own numbers) is refreshed.
pub const STATS_INTERVAL: Duration = Duration::from_secs(1);

/// Accumulated durations of one stage, read by two independent windows: the log line
/// ([`Stage::take_summary`]) and the stats overlay ([`Stage::take_lap`]).
#[derive(Default)]
pub struct Stage {
    window: Acc,
    lap: Acc,
}

#[derive(Default)]
struct Acc {
    count: u32,
    sum_us: u64,
    max_us: u64,
}

impl Acc {
    fn record(&mut self, us: u64) {
        self.count = self.count.saturating_add(1);
        self.sum_us = self.sum_us.saturating_add(us);
        self.max_us = self.max_us.max(us);
    }

    fn take(&mut self) -> StageSummary {
        let s = StageSummary {
            count: self.count,
            avg_ms: if self.count == 0 { 0.0 } else { self.sum_us as f64 / self.count as f64 / 1000.0 },
            max_ms: self.max_us as f64 / 1000.0,
        };
        *self = Self::default();
        s
    }
}

impl Stage {
    /// Adds one measurement.
    pub fn record(&mut self, d: Duration) {
        let us = d.as_micros() as u64;
        self.window.record(us);
        self.lap.record(us);
    }

    /// Returns the log window's summary and starts a new window.
    pub fn take_summary(&mut self) -> StageSummary {
        self.window.take()
    }

    /// Returns the summary since the previous lap (stats overlay) and starts a new lap.
    pub fn take_lap(&mut self) -> StageSummary {
        self.lap.take()
    }
}

/// An event or byte count, read by the same two windows as [`Stage`].
#[derive(Default)]
pub struct Counter {
    total: u64,
    logged: u64,
    lapped: u64,
}

impl std::ops::AddAssign<u64> for Counter {
    fn add_assign(&mut self, n: u64) {
        self.total = self.total.wrapping_add(n);
    }
}

impl Counter {
    /// The count so far in the current log window.
    pub fn in_window(&self) -> u64 {
        self.total.wrapping_sub(self.logged)
    }

    /// The count since the previous log window; starts a new one.
    pub fn take(&mut self) -> u64 {
        let n = self.total.wrapping_sub(self.logged);
        self.logged = self.total;
        n
    }

    /// The count since the previous lap; starts a new one.
    pub fn take_lap(&mut self) -> u64 {
        let n = self.total.wrapping_sub(self.lapped);
        self.lapped = self.total;
        n
    }
}

/// One stage's numbers over a window; displays as `avg/maxms`.
#[derive(Clone, Debug, PartialEq)]
pub struct StageSummary {
    pub count: u32,
    pub avg_ms: f64,
    pub max_ms: f64,
}

impl fmt::Display for StageSummary {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:.1}/{:.1}ms", self.avg_ms, self.max_ms)
    }
}

/// Fixed-length window: [`PERF_INTERVAL`] for log lines, [`STATS_INTERVAL`] for the stats overlay.
pub struct PerfWindow {
    started: Instant,
    interval: Duration,
}

impl PerfWindow {
    /// A logging window starting at `now`.
    pub fn new(now: Instant) -> Self {
        Self::every(now, PERF_INTERVAL)
    }

    /// A window of `interval` starting at `now`.
    pub fn every(now: Instant, interval: Duration) -> Self {
        Self { started: now, interval }
    }

    /// Once the interval has passed: restarts the window and returns its length in seconds.
    pub fn due(&mut self, now: Instant) -> Option<f64> {
        let elapsed = now.duration_since(self.started);
        if elapsed < self.interval {
            return None;
        }
        self.started = now;
        Some(elapsed.as_secs_f64())
    }
}

/// CPU and memory of this process, as the stats overlay and the perf lines show it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProcessUsage {
    /// Percent of the whole machine (all cores = 100), like Task Manager.
    pub cpu_pct: f32,
    /// Working set (Windows) or resident set (Linux), MiB.
    pub ram_mb: f32,
}

impl ProcessUsage {
    /// Usage from `cpu` time consumed over `wall` time on a machine with `cores` logical processors.
    pub fn from_times(cpu: Duration, wall: Duration, cores: usize, ram_bytes: u64) -> Self {
        let wall = wall.as_secs_f64() * cores.max(1) as f64;
        let cpu_pct = if wall <= 0.0 { 0.0 } else { (cpu.as_secs_f64() / wall * 100.0).clamp(0.0, 100.0) };
        Self { cpu_pct: cpu_pct as f32, ram_mb: (ram_bytes as f64 / (1024.0 * 1024.0)) as f32 }
    }
}

impl fmt::Display for ProcessUsage {
    /// `cpu=3.2% ram=148MB`, the perf lines' form.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "cpu={:.1}% ram={:.0}MB", self.cpu_pct, self.ram_mb)
    }
}

/// Kilobits per second for `bytes` over `secs`.
pub fn kbps(bytes: u64, secs: f64) -> u64 {
    if secs <= 0.0 { 0 } else { (bytes as f64 * 8.0 / 1000.0 / secs).round() as u64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_summarises_and_resets() {
        let mut s = Stage::default();
        s.record(Duration::from_micros(1000));
        s.record(Duration::from_micros(3000));
        let sum = s.take_summary();
        assert_eq!((sum.count, sum.avg_ms, sum.max_ms), (2, 2.0, 3.0));
        assert_eq!(sum.to_string(), "2.0/3.0ms");
        assert_eq!(s.take_summary(), StageSummary { count: 0, avg_ms: 0.0, max_ms: 0.0 });
        // The overlay's lap is independent of the log window.
        assert_eq!(s.take_lap().count, 2);
        s.record(Duration::from_micros(500));
        assert_eq!((s.take_lap().max_ms, s.take_summary().count), (0.5, 1));
    }

    #[test]
    fn counter_feeds_both_windows() {
        let mut c = Counter::default();
        c += 5;
        assert_eq!(c.take_lap(), 5);
        c += 2;
        assert_eq!(c.in_window(), 7);
        assert_eq!((c.take(), c.take(), c.take_lap()), (7, 0, 2));
    }

    #[test]
    fn window_fires_after_interval() {
        let t0 = Instant::now();
        let mut w = PerfWindow::new(t0);
        assert_eq!(w.due(t0 + Duration::from_secs(9)), None);
        assert_eq!(w.due(t0 + Duration::from_secs(10)), Some(10.0));
        assert_eq!(w.due(t0 + Duration::from_secs(15)), None);
    }

    #[test]
    fn process_usage_is_a_share_of_the_whole_machine() {
        let u = ProcessUsage::from_times(Duration::from_millis(500), Duration::from_secs(1), 4, 150 << 20);
        assert_eq!((u.cpu_pct, u.ram_mb), (12.5, 150.0));
        assert_eq!(u.to_string(), "cpu=12.5% ram=150MB");
        assert_eq!(ProcessUsage::from_times(Duration::from_secs(9), Duration::from_secs(1), 2, 0).cpu_pct, 100.0);
        assert_eq!(ProcessUsage::from_times(Duration::from_secs(1), Duration::ZERO, 0, 0).cpu_pct, 0.0);
    }

    #[test]
    fn kbps_from_bytes() {
        assert_eq!(kbps(3_062_500, 10.0), 2450);
        assert_eq!(kbps(5, 0.0), 0);
    }
}
