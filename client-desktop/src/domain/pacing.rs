//! Pacing of one video frame's UDP fragments. Sent back to back, a key frame of a few hundred fragments
//! arrives at a home router or the relay's socket as one burst that overflows its queue, and one lost fragment
//! loses the whole frame. A token bucket instead lets a frame of average size (the bucket: the stream's
//! bitrate divided by the frame rate, at least [`IMMEDIATE_FRAGMENTS`]) leave at once and spreads the rest at
//! twice the stream's bitrate, but never over more than half a frame interval: the first fragment is never
//! delayed and a frame is complete at most half an interval later than in a burst.

use std::time::Duration;

/// Frames of up to this many fragments always leave at once.
pub const IMMEDIATE_FRAGMENTS: usize = 8;
/// The refill rate as a multiple of the stream's bitrate.
const RATE_HEADROOM: f64 = 2.0;
/// Environment switch: `0` sends every fragment of a frame in one burst.
pub const PACING_ENV: &str = "OPENDESK_UDP_PACING";

/// True unless the environment value is `0`.
pub fn pacing_enabled(env: Option<&str>) -> bool {
    env.map(str::trim) != Some("0")
}

/// When the fragments of one frame may leave: the first `burst` at once, then one every `step`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pacing {
    pub burst: usize,
    pub step: Duration,
}

impl Pacing {
    /// Every fragment at once.
    pub fn burst(fragments: usize) -> Self {
        Self { burst: fragments, step: Duration::ZERO }
    }

    /// When fragment `index` may leave, after the frame's first one.
    pub fn offset(&self, index: usize) -> Duration {
        if index < self.burst { Duration::ZERO } else { self.step * (index - self.burst + 1) as u32 }
    }
}

/// The schedule for a frame of `fragments` fragments of `fragment_bytes` each, in a stream at `target_fps`
/// sending `kbps` (kilobits per second, 0 = unknown: spread evenly). An unknown frame rate (0) sends a burst.
pub fn plan(fragments: usize, fragment_bytes: usize, target_fps: u32, kbps: u64) -> Pacing {
    if target_fps == 0 || fragments <= IMMEDIATE_FRAGMENTS {
        return Pacing::burst(fragments);
    }
    let bytes_per_sec = kbps as f64 * 1000.0 / 8.0;
    let average_frame = bytes_per_sec / f64::from(target_fps);
    let bucket = (average_frame / fragment_bytes.max(1) as f64) as usize;
    let burst = bucket.max(IMMEDIATE_FRAGMENTS);
    if burst >= fragments {
        return Pacing::burst(fragments);
    }
    let rest = (fragments - burst) as f64;
    let spread = 0.5 / f64::from(target_fps);
    let capped = spread / rest;
    let step =
        if bytes_per_sec > 0.0 { capped.min(fragment_bytes as f64 / (bytes_per_sec * RATE_HEADROOM)) } else { capped };
    Pacing { burst, step: Duration::from_secs_f64(step) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAG: usize = 1166;

    #[test]
    fn small_frames_leave_at_once() {
        let p = plan(5, FRAG, 60, 4000);
        assert_eq!(p, Pacing::burst(5));
        assert_eq!((p.offset(0), p.offset(4)), (Duration::ZERO, Duration::ZERO));
        assert_eq!(plan(300, FRAG, 0, 4000), Pacing::burst(300), "unknown frame rate");
    }

    #[test]
    fn a_frame_of_average_size_is_one_burst() {
        // 40 Mbps at 60 FPS: ~83 KB per frame, more than these 40 fragments.
        assert_eq!(plan(40, FRAG, 60, 40_000), Pacing::burst(40));
    }

    #[test]
    fn a_key_frame_spreads_over_half_an_interval_at_most() {
        let p = plan(300, FRAG, 60, 4000);
        assert_eq!(p.burst, IMMEDIATE_FRAGMENTS);
        assert_eq!(p.offset(0), Duration::ZERO, "the first fragment is never delayed");
        let last = p.offset(299);
        assert!(last <= Duration::from_micros(8_334) && last > Duration::from_millis(8), "{last:?}");
        assert!(p.offset(100) < p.offset(101));
    }

    #[test]
    fn a_fast_stream_paces_at_twice_its_bitrate() {
        // 20 Mbps = 2.5 MB/s, refilled at 5 MB/s: one fragment per ~233 µs, below the half-interval cap
        // (8.33 ms / 31 remaining fragments ~ 269 µs).
        let p = plan(66, FRAG, 60, 20_000);
        assert_eq!(p.burst, 35);
        assert_eq!(p.step, Duration::from_secs_f64(FRAG as f64 / 5_000_000.0));
        assert!(p.offset(65) < Duration::from_micros(8_333));
    }

    #[test]
    fn unknown_bitrate_spreads_evenly() {
        let p = plan(108, FRAG, 120, 0);
        assert_eq!(p.burst, IMMEDIATE_FRAGMENTS);
        let last = p.offset(107);
        assert!(last <= Duration::from_micros(4_167) && last > Duration::from_micros(4_100), "{last:?}");
    }

    #[test]
    fn kill_switch() {
        assert!(pacing_enabled(None) && pacing_enabled(Some("1")));
        assert!(!pacing_enabled(Some("0")));
    }
}
