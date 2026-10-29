//! The viewer's performance preset, like a game's graphics quality option: Low, Medium or High. It sets the
//! frame-rate cap the viewer advertises (`fps-N`) and how the host spends bandwidth (`perf-*`, see
//! [`Performance::cap`]). Resolution and voice quality stay per session.

use serde::{Deserialize, Serialize};

use super::frame_rate::DEFAULT_FPS;

/// One preset. Partners that list none get [`Performance::Medium`], today's behavior.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Performance {
    /// 30 FPS, half the H.264 bitrate: saves CPU, battery and data.
    Low,
    /// 60 FPS at the usual bitrate.
    #[default]
    Medium,
    /// The viewer monitor's refresh rate, twice the bitrate, H.264 as soon as something moves.
    High,
}

impl Performance {
    /// Every preset, in the order the settings dialog shows them.
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    /// The frame-rate setting it stands for (see `frame_rate::viewer_cap`): 30, 60, or 0 for this monitor's
    /// refresh rate. Also stored as `Settings::max_fps`, so an older build reading the config behaves the same.
    pub fn fps_setting(self) -> u32 {
        match self {
            Self::Low => 30,
            Self::Medium => DEFAULT_FPS,
            Self::High => 0,
        }
    }

    /// The preset of a config written before presets existed (`Settings::max_fps`): 30 is Low, 60 Medium, Auto
    /// (0) and Maximum (240) High. Hand-edited values go to the nearest one.
    pub fn from_max_fps(fps: u32) -> Self {
        match fps {
            0 => Self::High,
            1..=30 => Self::Low,
            31..=DEFAULT_FPS => Self::Medium,
            _ => Self::High,
        }
    }

    /// Factor on the host's H.264 target bitrate; congestion still lowers the result.
    pub fn bitrate_factor(self) -> f32 {
        match self {
            Self::Low => 0.5,
            Self::Medium => 1.0,
            Self::High => 2.0,
        }
    }

    /// Motion goes to H.264 at once instead of after a few heavy JPEG frames (hardware encoders only).
    pub fn prefers_h264(self) -> bool {
        self == Self::High
    }

    /// The capability a viewer lists for it (`perf-low`, `perf-medium`, `perf-high`).
    pub fn cap(self) -> &'static str {
        match self {
            Self::Low => "perf-low",
            Self::Medium => "perf-medium",
            Self::High => "perf-high",
        }
    }

    /// The preset a capability names, if it names one.
    pub fn from_cap(cap: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.cap() == cap)
    }

    /// Position in [`Self::ALL`] (the settings dialog's segment).
    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|&p| p == self).unwrap_or(1) as i32
    }

    /// The preset at `index` in [`Self::ALL`]; out of range is Medium.
    pub fn from_index(index: i32) -> Self {
        usize::try_from(index).ok().and_then(|i| Self::ALL.get(i).copied()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_frame_rate_settings_map_to_presets() {
        assert_eq!(Performance::from_max_fps(30), Performance::Low);
        assert_eq!(Performance::from_max_fps(60), Performance::Medium);
        assert_eq!(Performance::from_max_fps(0), Performance::High, "Auto followed the monitor");
        assert_eq!(Performance::from_max_fps(240), Performance::High, "Maximum");
        assert_eq!(
            (Performance::from_max_fps(24), Performance::from_max_fps(50)),
            (Performance::Low, Performance::Medium)
        );
        // And back: what an older build reads from `max_fps`.
        for p in Performance::ALL {
            assert_eq!(Performance::from_max_fps(p.fps_setting()), p);
        }
    }

    #[test]
    fn caps_indices_and_wire_names_round_trip() {
        for p in Performance::ALL {
            assert_eq!(Performance::from_cap(p.cap()), Some(p));
            assert_eq!(Performance::from_index(p.index()), p);
            assert!(p.cap().len() <= 32 && p.cap().bytes().all(|b| b.is_ascii_lowercase() || b == b'-'));
        }
        assert_eq!(Performance::from_cap("perf-ultra"), None);
        assert_eq!(
            (Performance::from_index(-1), Performance::from_index(7)),
            (Performance::Medium, Performance::Medium)
        );
        assert_eq!(serde_json::to_string(&Performance::High).unwrap(), r#""high""#);
        assert_eq!(serde_json::from_str::<Performance>(r#""low""#).unwrap(), Performance::Low);
    }

    #[test]
    fn presets_scale_bitrate_and_mode() {
        let factors: Vec<f32> = Performance::ALL.iter().map(|p| p.bitrate_factor()).collect();
        assert_eq!(factors, [0.5, 1.0, 2.0]);
        assert!(Performance::High.prefers_h264() && !Performance::Medium.prefers_h264());
        assert_eq!(Performance::default(), Performance::Medium);
    }
}
