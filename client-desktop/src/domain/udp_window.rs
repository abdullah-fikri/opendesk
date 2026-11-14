//! How many video frames the host may have in flight on the UDP path. Each frame waits for the viewer's
//! acknowledgement, so a fixed window of `n` frames caps the frame rate at `n / RTT`: two frames allow ~66 FPS
//! at 30 ms, ~33 FPS through a UDP relay at 60 ms. The dynamic window covers one round trip at the target
//! rate plus one frame, `clamp(ceil(RTT × fps) + 1, 2, 8)`, and shrinks back toward 2 as the capture loop's
//! congestion level rises, so a queue that lengthens the RTT cannot widen the window without bound.

use std::time::Duration;

/// The window without measurements, and the old fixed window (`OPENDESK_DYNAMIC_WINDOW=0`).
pub const MIN_FRAMES_IN_FLIGHT: usize = 2;
/// Upper bound: beyond it a lost acknowledgement or a queue costs too much latency.
pub const MAX_FRAMES_IN_FLIGHT: usize = 8;
/// Environment switch: `0` keeps the fixed window of [`MIN_FRAMES_IN_FLIGHT`].
pub const DYNAMIC_WINDOW_ENV: &str = "OPENDESK_DYNAMIC_WINDOW";

/// True unless the environment value is `0`.
pub fn dynamic_enabled(env: Option<&str>) -> bool {
    env.map(str::trim) != Some("0")
}

/// Frames allowed in flight for a smoothed round trip `srtt` (None before the first acknowledgement) at
/// `target_fps`, at congestion level `congestion` (0..1, `CongestionMeter`).
pub fn frames_in_flight(srtt: Option<Duration>, target_fps: u32, congestion: f32) -> usize {
    let Some(srtt) = srtt else { return MIN_FRAMES_IN_FLIGHT };
    let needed = (srtt.as_secs_f64() * f64::from(target_fps)).ceil() as usize + 1;
    let window = needed.clamp(MIN_FRAMES_IN_FLIGHT, MAX_FRAMES_IN_FLIGHT);
    let room = (window - MIN_FRAMES_IN_FLIGHT) as f32 * (1.0 - congestion.clamp(0.0, 1.0));
    MIN_FRAMES_IN_FLIGHT + room.round() as usize
}

/// Smoothed round-trip time of frame acknowledgements, like TCP's SRTT (RFC 6298): each sample moves the
/// estimate an eighth of the way.
#[derive(Clone, Copy, Debug, Default)]
pub struct RttEstimator {
    srtt: Option<Duration>,
}

impl RttEstimator {
    /// Adds one measured round trip.
    pub fn sample(&mut self, rtt: Duration) {
        self.srtt = Some(match self.srtt {
            None => rtt,
            Some(s) => s.mul_f64(7.0 / 8.0) + rtt.mul_f64(1.0 / 8.0),
        });
    }

    /// The current estimate; None before the first sample.
    pub fn get(&self) -> Option<Duration> {
        self.srtt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Option<Duration> {
        Some(Duration::from_millis(n))
    }

    #[test]
    fn window_covers_one_round_trip_plus_a_frame() {
        assert_eq!(frames_in_flight(None, 144, 0.0), 2, "no measurement yet");
        assert_eq!(frames_in_flight(ms(2), 60, 0.0), 2, "LAN keeps the old window");
        assert_eq!(frames_in_flight(ms(30), 60, 0.0), 3);
        assert_eq!(frames_in_flight(ms(30), 120, 0.0), 5);
        assert_eq!(frames_in_flight(ms(60), 60, 0.0), 5, "UDP relay: two legs");
        assert_eq!(frames_in_flight(ms(200), 144, 0.0), 8, "capped");
        assert_eq!(frames_in_flight(ms(30), 0, 0.0), 2);
    }

    #[test]
    fn congestion_shrinks_the_window_back_to_two() {
        assert_eq!(frames_in_flight(ms(60), 120, 0.0), 8);
        assert_eq!(frames_in_flight(ms(60), 120, 0.5), 5);
        assert_eq!(frames_in_flight(ms(60), 120, 1.0), 2);
        assert_eq!(frames_in_flight(ms(60), 120, 7.0), 2, "level clamped");
    }

    #[test]
    fn kill_switch() {
        assert!(dynamic_enabled(None) && dynamic_enabled(Some("1")) && dynamic_enabled(Some("")));
        assert!(!dynamic_enabled(Some("0")) && !dynamic_enabled(Some(" 0")));
    }

    #[test]
    fn rtt_moves_an_eighth_per_sample() {
        let mut r = RttEstimator::default();
        assert_eq!(r.get(), None);
        r.sample(Duration::from_millis(40));
        assert_eq!(r.get(), ms(40));
        r.sample(Duration::from_millis(80));
        assert_eq!(r.get(), ms(45));
    }
}
