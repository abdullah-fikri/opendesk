//! Per-stage timing for the periodic `host.perf` / `viewer.perf` log lines. A [`Stage`] accumulates
//! durations (count, sum, max) without allocating; [`Stage::take_summary`] reads and resets it once per
//! [`PerfWindow`].
//!
//! Host line (`host.perf`, one per window while a session is hosted):
//! - `fps`: frames sent per second; `target`: the capture loop's pacing rate.
//! - `grab`, `encode`, `gate`: avg/max milliseconds per tick for `DesktopCapturer::grab` (includes DXGI's
//!   wait for a desktop change), `StreamEncoder::encode` (block diff + JPEG/H.264) and the wait for the
//!   previous frame to leave (`can_send_frame`).
//! - `kbps`: video message bytes sent, in kilobits per second.
//! - `idle_frames`: grabs that produced nothing to send (no change); `gate_skips`: ticks skipped because the
//!   previous frame was still on its way when the tick ended.
//! - `h264`: `off`, or `on(<encoder name>)` once the motion encoder is running.
//!
//! Viewer line (`viewer.perf`, one per window while the session is visible and receiving video):
//! - `session`: the viewer session id; `fps`: frames handed to the UI per second.
//! - `process`: avg/max ms to decode and compose a batch into an output buffer.
//! - `handoff`: avg/max ms to pass the buffer to the UI thread.
//! - `kbps`: video message bytes received, in kilobits per second; `batches`: inbox drains decoded.

use std::fmt;
use std::time::{Duration, Instant};

/// How often the perf lines are logged.
pub const PERF_INTERVAL: Duration = Duration::from_secs(10);

/// Accumulated durations of one stage since the last summary.
#[derive(Default)]
pub struct Stage {
    count: u32,
    sum_us: u64,
    max_us: u64,
}

impl Stage {
    /// Adds one measurement.
    pub fn record(&mut self, d: Duration) {
        let us = d.as_micros() as u64;
        self.count += 1;
        self.sum_us += us;
        self.max_us = self.max_us.max(us);
    }

    /// Returns the window's summary and starts a new window.
    pub fn take_summary(&mut self) -> StageSummary {
        let s = StageSummary {
            count: self.count,
            avg_ms: if self.count == 0 { 0.0 } else { self.sum_us as f64 / self.count as f64 / 1000.0 },
            max_ms: self.max_us as f64 / 1000.0,
        };
        *self = Self::default();
        s
    }
}

/// One stage's numbers over a window; displays as `avg/maxms`.
#[derive(Debug, PartialEq)]
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

/// Fixed-length logging window.
pub struct PerfWindow {
    started: Instant,
}

impl PerfWindow {
    /// A window starting at `now`.
    pub fn new(now: Instant) -> Self {
        Self { started: now }
    }

    /// Once [`PERF_INTERVAL`] has passed: restarts the window and returns its length in seconds.
    pub fn due(&mut self, now: Instant) -> Option<f64> {
        let elapsed = now.duration_since(self.started);
        if elapsed < PERF_INTERVAL {
            return None;
        }
        self.started = now;
        Some(elapsed.as_secs_f64())
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
    fn kbps_from_bytes() {
        assert_eq!(kbps(3_062_500, 10.0), 2450);
        assert_eq!(kbps(5, 0.0), 0);
    }
}
