//! Frame-rate rules: the cap a viewer advertises (`fps-N`, see `capability::max_fps_cap`), the rate a host
//! streams at, and how congestion lowers that rate before it lowers quality.

use std::time::{Duration, Instant};

/// Frame rate of today's pipeline: partners without a cap, relayed sessions and software encoders get this.
pub const DEFAULT_FPS: u32 = 60;
/// Highest rate ever streamed; the High preset follows the monitor up to it (1.2.0's "Maximum" asked for it).
pub const FPS_MAXIMUM: u32 = 240;
/// Environment variable that caps the frame rate on both sides (`OPENDESK_MAX_FPS=60` restores the 60 FPS
/// pipeline).
pub const MAX_FPS_ENV: &str = "OPENDESK_MAX_FPS";

/// Reads the `OPENDESK_MAX_FPS` value; anything but a positive number means "no cap".
pub fn env_cap(raw: Option<&str>) -> Option<u32> {
    raw.and_then(|v| v.trim().parse().ok()).filter(|&n| n > 0)
}

/// The cap a viewer advertises: its monitor's refresh rate for `setting` 0 (Auto, the High preset), else the
/// setting itself (30 or 60, see `Performance::fps_setting`), lowered by the environment cap.
pub fn viewer_cap(setting: u32, monitor_hz: u32, env: Option<u32>) -> u32 {
    let wanted = if setting == 0 { monitor_hz } else { setting };
    wanted.min(env.unwrap_or(u32::MAX)).clamp(1, FPS_MAXIMUM)
}

/// Environment variable that, set to `0`, keeps sessions on the WebSocket relay at 60 FPS (the rule before
/// performance presets: only a direct or UDP-relayed path went above 60).
pub const WS_HIGH_FPS_ENV: &str = "OPENDESK_WS_HIGH_FPS";

/// Reads the `OPENDESK_WS_HIGH_FPS` value: anything but `0` allows more than 60 FPS over the WebSocket relay.
pub fn ws_high_fps(raw: Option<&str>) -> bool {
    raw.map(str::trim) != Some("0")
}

/// The host's target rate: the viewer's cap (0 = none = 60), never more than the host monitor shows. A monitor
/// slower than 60 Hz keeps today's 60. Above 60 needs a hardware H.264 encoder, on any path: P2P, the UDP relay
/// or (unless `ws_high_fps` is off) the WebSocket relay, where congestion steps the rate down like elsewhere.
pub fn host_target(
    host_hz: u32,
    viewer_max: u32,
    direct: bool,
    hw_encoder: bool,
    ws_high_fps: bool,
    env: Option<u32>,
) -> u32 {
    let viewer = if viewer_max == 0 { DEFAULT_FPS } else { viewer_max };
    let mut fps = viewer.min(host_hz.max(DEFAULT_FPS));
    if !(hw_encoder && (direct || ws_high_fps)) {
        fps = fps.min(DEFAULT_FPS);
    }
    fps.min(env.unwrap_or(u32::MAX)).clamp(1, FPS_MAXIMUM)
}

/// Rates a congested stream above 60 FPS steps down through, one at a time. 60 is the floor: below it,
/// congestion lowers quality and bitrate as before.
const FPS_STEPS: [u32; 4] = [144, 120, 90, 60];
/// Congestion level (see `CongestionMeter`) at which the rate steps down...
pub const CONGESTION_HIGH: f32 = 0.3;
/// ...and under which it counts as recovered.
const CONGESTION_LOW: f32 = 0.05;
/// Time between two steps down, so the meter sees the effect of the last one.
const STEP_DOWN_HOLD: Duration = Duration::from_secs(1);
/// Time the level must stay low before the rate steps back up one step.
const RECOVER_AFTER: Duration = Duration::from_secs(2);

/// The next lower rate (144 -> 120 -> 90 -> 60); 60 and below stay.
pub fn step_down(fps: u32) -> u32 {
    if fps <= DEFAULT_FPS { fps } else { FPS_STEPS.into_iter().find(|&s| s < fps).unwrap_or(DEFAULT_FPS) }
}

/// The next higher rate, at most `ceiling`.
pub fn step_up(fps: u32, ceiling: u32) -> u32 {
    FPS_STEPS.into_iter().rev().find(|&s| s > fps).unwrap_or(ceiling).min(ceiling)
}

/// Lowers the target rate under congestion and restores it step by step once the link keeps up.
pub struct FpsGovernor {
    /// Upper limit from congestion; `u32::MAX` = none.
    limit: u32,
    last_down: Option<Instant>,
    low_since: Option<Instant>,
}

impl FpsGovernor {
    /// A governor that does not limit anything yet.
    pub fn new() -> Self {
        Self { limit: u32::MAX, last_down: None, low_since: None }
    }

    /// The rate to stream at, given the negotiated `ceiling` and the current congestion `level`.
    pub fn update(&mut self, ceiling: u32, level: f32, now: Instant) -> u32 {
        if ceiling <= DEFAULT_FPS {
            // Nothing to step down: today's quality path handles congestion alone.
            *self = Self::new();
            return ceiling;
        }
        let fps = self.limit.min(ceiling);
        if level >= CONGESTION_HIGH {
            self.low_since = None;
            if fps > DEFAULT_FPS && self.last_down.is_none_or(|t| now.duration_since(t) >= STEP_DOWN_HOLD) {
                self.limit = step_down(fps);
                self.last_down = Some(now);
            }
        } else if level <= CONGESTION_LOW {
            let since = *self.low_since.get_or_insert(now);
            if fps < ceiling && now.duration_since(since) >= RECOVER_AFTER {
                let up = step_up(fps, ceiling);
                self.limit = if up >= ceiling { u32::MAX } else { up };
                self.low_since = Some(now);
            }
        } else {
            self.low_since = None;
        }
        self.limit.min(ceiling)
    }

    /// The congestion level handed to the encoder's quality and bitrate: held at [`CONGESTION_HIGH`] while the
    /// rate is above 60 and can still step down, passed through unchanged at 60 and below.
    pub fn quality_level(level: f32, fps: u32) -> f32 {
        if fps > DEFAULT_FPS { level.min(CONGESTION_HIGH) } else { level }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_never_go_below_60() {
        assert_eq!((step_down(240), step_down(144), step_down(120), step_down(90)), (144, 120, 90, 60));
        assert_eq!((step_down(165), step_down(75), step_down(60), step_down(30)), (144, 60, 60, 30));
        assert_eq!((step_up(60, 144), step_up(90, 144), step_up(120, 144), step_up(144, 240)), (90, 120, 144, 240));
        assert_eq!((step_up(60, 75), step_up(120, 120)), (75, 120));
    }

    #[test]
    fn governor_steps_down_then_recovers_one_step_at_a_time() {
        let mut g = FpsGovernor::new();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(g.update(144, 0.0, at(0)), 144);
        assert_eq!(g.update(144, 0.8, at(10)), 120);
        // Held for a second before the next step.
        assert_eq!(g.update(144, 0.8, at(500)), 120);
        assert_eq!(g.update(144, 0.8, at(1010)), 90);
        assert_eq!(g.update(144, 0.8, at(2020)), 60);
        assert_eq!(g.update(144, 0.8, at(3030)), 60, "never below 60");
        // A medium level neither steps nor counts as recovered.
        assert_eq!(g.update(144, 0.1, at(3100)), 60);
        assert_eq!(g.update(144, 0.0, at(3200)), 60);
        assert_eq!(g.update(144, 0.0, at(5100)), 60);
        assert_eq!(g.update(144, 0.0, at(5200)), 90);
        assert_eq!(g.update(144, 0.0, at(6000)), 90);
        // Congestion again resets the recovery timer.
        assert_eq!(g.update(144, 0.2, at(7000)), 90);
        assert_eq!(g.update(144, 0.0, at(7100)), 90);
        assert_eq!(g.update(144, 0.0, at(9100)), 120);
        assert_eq!(g.update(144, 0.0, at(11_100)), 144);
        assert_eq!(g.update(240, 0.0, at(11_200)), 240, "a recovered stream follows a higher ceiling");
    }

    #[test]
    fn governor_is_inert_at_60_and_below() {
        let mut g = FpsGovernor::new();
        let t0 = Instant::now();
        assert_eq!(g.update(60, 1.0, t0), 60);
        assert_eq!(g.update(30, 1.0, t0), 30);
        assert_eq!(FpsGovernor::quality_level(0.9, 60), 0.9);
        assert_eq!(FpsGovernor::quality_level(0.9, 144), CONGESTION_HIGH);
        assert_eq!(FpsGovernor::quality_level(0.1, 144), 0.1);
    }

    #[test]
    fn viewer_cap_follows_setting_and_monitor() {
        assert_eq!(viewer_cap(0, 144, None), 144);
        assert_eq!(viewer_cap(0, 60, None), 60);
        assert_eq!(viewer_cap(30, 144, None), 30);
        assert_eq!(viewer_cap(FPS_MAXIMUM, 60, None), FPS_MAXIMUM);
        assert_eq!(viewer_cap(0, 144, Some(60)), 60);
        assert_eq!(viewer_cap(0, 500, None), FPS_MAXIMUM);
    }

    #[test]
    fn host_target_is_60_unless_everything_allows_more() {
        // Old viewers (no cap) and the kill switch keep today's 60, whatever the hardware.
        assert_eq!(host_target(144, 0, true, true, true, None), 60);
        assert_eq!(host_target(144, 60, true, true, true, None), 60);
        assert_eq!(host_target(144, 144, true, true, true, Some(60)), 60);
        // Software encoder: at most 60 on every path.
        assert_eq!(host_target(144, 144, true, false, true, None), 60);
        assert_eq!(host_target(144, 144, false, false, true, None), 60);
        // Everything allows more: the lower of both monitors.
        assert_eq!(host_target(144, 240, true, true, true, None), 144);
        assert_eq!(host_target(240, 120, true, true, true, None), 120);
        // A slow host monitor still streams 60; a viewer asking for 30 gets 30.
        assert_eq!(host_target(30, 0, false, false, true, None), 60);
        assert_eq!(host_target(144, 30, false, false, true, None), 30);
    }

    #[test]
    fn websocket_relay_goes_above_60_unless_switched_off() {
        // A hardware encoder is enough on the WebSocket relay too.
        assert_eq!(host_target(144, 144, false, true, true, None), 144);
        // OPENDESK_WS_HIGH_FPS=0: the old rule, 60 on the WebSocket relay, more only on a direct path.
        assert_eq!(host_target(144, 144, false, true, false, None), 60);
        assert_eq!(host_target(144, 144, true, true, false, None), 144);
        assert!(ws_high_fps(None) && ws_high_fps(Some("1")) && ws_high_fps(Some("")));
        assert!(!ws_high_fps(Some("0")) && !ws_high_fps(Some(" 0 ")));
    }

    #[test]
    fn env_cap_ignores_junk() {
        assert_eq!(env_cap(Some(" 60 ")), Some(60));
        assert_eq!((env_cap(Some("0")), env_cap(Some("fast")), env_cap(None)), (None, None, None));
    }
}
