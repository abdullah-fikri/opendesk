//! Frame-rate rules: the cap a viewer advertises (`fps-N`, see `capability::max_fps_cap`) and the rate a host
//! streams at.

/// Frame rate of today's pipeline: partners without a cap, relayed sessions and software encoders get this.
pub const DEFAULT_FPS: u32 = 60;
/// Highest rate ever streamed; the "Maximum" setting asks for it.
pub const FPS_MAXIMUM: u32 = 240;
/// Environment variable that caps the frame rate on both sides (`OPENDESK_MAX_FPS=60` restores the 60 FPS
/// pipeline).
pub const MAX_FPS_ENV: &str = "OPENDESK_MAX_FPS";

/// Reads the `OPENDESK_MAX_FPS` value; anything but a positive number means "no cap".
pub fn env_cap(raw: Option<&str>) -> Option<u32> {
    raw.and_then(|v| v.trim().parse().ok()).filter(|&n| n > 0)
}

/// The cap a viewer advertises: its monitor's refresh rate for `setting` 0 (Auto), else the setting itself
/// (30, 60 or [`FPS_MAXIMUM`]), lowered by the environment cap.
pub fn viewer_cap(setting: u32, monitor_hz: u32, env: Option<u32>) -> u32 {
    let wanted = if setting == 0 { monitor_hz } else { setting };
    wanted.min(env.unwrap_or(u32::MAX)).clamp(1, FPS_MAXIMUM)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn env_cap_ignores_junk() {
        assert_eq!(env_cap(Some(" 60 ")), Some(60));
        assert_eq!((env_cap(Some("0")), env_cap(Some("fast")), env_cap(None)), (None, None, None));
    }
}
