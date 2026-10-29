//! Input and control events carried by `relay-input` / `input-command` (and by the P2P input channel).
//! Coordinates are normalized to 0.0..=1.0 of the streamed monitor.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::performance::Performance;

/// One event. Viewer -> host unless noted.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "action", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum InputEvent {
    #[serde(rename = "mousemove")]
    MouseMove {
        norm_x: f64,
        norm_y: f64,
    },
    #[serde(rename = "mousedown")]
    MouseDown {
        norm_x: f64,
        norm_y: f64,
        #[serde(default)]
        button: i32,
    },
    #[serde(rename = "mouseup")]
    MouseUp {
        norm_x: f64,
        norm_y: f64,
        #[serde(default)]
        button: i32,
    },
    /// Down + up (Android viewers).
    Click {
        norm_x: f64,
        norm_y: f64,
        #[serde(default)]
        button: i32,
    },
    /// Two clicks (Android viewers).
    #[serde(rename = "dblclick")]
    DoubleClick {
        norm_x: f64,
        norm_y: f64,
        #[serde(default)]
        button: i32,
    },
    Wheel {
        norm_x: f64,
        norm_y: f64,
        delta_y: i32,
    },
    /// Physical key down (Win32 virtual-key code); tracked so a session end releases it.
    #[serde(rename = "keydown")]
    KeyDown {
        #[serde(alias = "vk")]
        key_code: u8,
    },
    #[serde(rename = "keyup")]
    KeyUp {
        #[serde(alias = "vk")]
        key_code: u8,
    },
    /// Key with modifiers, pressed and released at once (viewer keyboard hook: Win, Alt+Tab, ...).
    #[serde(rename = "keycombo")]
    KeyCombo {
        #[serde(alias = "vk")]
        key_code: u8,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        shift: bool,
    },
    /// Unicode text typed on the viewer (layout and IME independent).
    Text {
        text: String,
    },
    /// Viewer clipboard text, sent right before a forwarded Ctrl+V.
    Clipboard {
        text: String,
    },
    /// Cycles to the next host monitor (viewers that predate `monitors`, Android).
    SwitchMonitor,
    SelectMonitor {
        index: u64,
    },
    /// `mode`: 0 native, 480/720/1080 presets. `auto` + `width`/`height`: stream at the viewer canvas size.
    SetResolution {
        mode: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u64>,
    },
    /// 0 = 16 kHz, 1 = 24 kHz, 2 = 48 kHz. Either direction.
    SetAudioQuality {
        quality: u64,
    },
    /// The viewer's performance preset and the frame-rate cap it stands for (what `perf-*` and `fps-N` say at
    /// registration, 0 = none): sent at session start and when the user picks another preset, so it applies
    /// without reconnecting. Older hosts ignore it and keep the registration caps.
    SetPerformance {
        preset: Performance,
        #[serde(default)]
        max_fps: u32,
    },
    /// A peer may mute the other side's microphone, never unmute it. Either direction.
    SetMic {
        muted: bool,
    },
    /// 0 normal, 1 frozen screen, 2 maintenance screen, 3 black screen.
    SetPrivacy {
        mode: i32,
    },
    SetBlockInput {
        blocked: bool,
    },
    /// "taskmgr", "win", "explorer", "desktop" or "alt-tab".
    Shortcut {
        name: String,
    },
    /// The viewer lost a delta frame and needs a key frame.
    VideoKeyframe,
    /// Host -> viewer: current cursor shape code (see `core_engine::capture::cursor_shape`).
    CursorShape {
        shape: i32,
    },
    /// Host -> viewer: monitor list (primary first) and the streamed index.
    Monitors {
        list: Vec<MonitorInfo>,
        active: u64,
    },
    /// The viewer shows (`on`) or hides its stats overlay; the host sends `host-stats` only in between.
    Stats {
        on: bool,
    },
    /// Round-trip probe of the stats overlay, about once a second while it is shown. `t`: viewer clock, ms.
    StatsPing {
        t: u64,
    },
    /// Host -> viewer: answers `stats-ping` at once with the same `t`, and `h`: the host's monotonic clock in
    /// microseconds (the viewer's clock offset for `frame-stamp`; absent from hosts up to 1.2.0).
    StatsPong {
        t: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        h: Option<u64>,
    },
    /// Host -> viewer: about once a second while the viewer asked for `stats`.
    HostStats(HostStats),
}

impl InputEvent {
    /// Decodes an `input-command` event; unknown or malformed events yield `None`.
    pub fn parse(event: &Value) -> Option<Self> {
        Self::deserialize(event).ok()
    }
}

/// One host monitor in a `monitors` event.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub index: u64,
    pub width: i64,
    pub height: i64,
    #[serde(default)]
    pub primary: bool,
}

/// The host's capture-loop numbers over the last second (`host-stats`); fields documented in
/// `domain::perf_stats`. Missing fields read as zero, so either side may add some.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct HostStats {
    /// Frames sent per second and the capture loop's pacing rate.
    pub fps: f32,
    pub target_fps: u32,
    /// Average and maximum milliseconds per tick.
    pub grab_ms: f32,
    pub grab_max_ms: f32,
    pub encode_ms: f32,
    pub encode_max_ms: f32,
    pub kbps: u64,
    /// Grabs with nothing to send, and ticks skipped because the previous frame was still on its way.
    pub idle: u32,
    pub gate_skips: u32,
    /// 0..1 (`CongestionMeter`).
    pub congestion: f32,
    /// H.264 encoder name; empty when the stream is JPEG blocks only.
    pub encoder: String,
    /// The host process's CPU (percent of the machine) and working set (MiB); absent up to 1.2.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_mb: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_keep_their_wire_names() {
        let e = serde_json::to_value(InputEvent::KeyDown { key_code: 65 }).unwrap();
        assert_eq!(e, json!({ "action": "keydown", "keyCode": 65 }));
        let e = serde_json::to_value(InputEvent::SetResolution { mode: 1080, auto: None, width: None, height: None })
            .unwrap();
        assert_eq!(e, json!({ "action": "set-resolution", "mode": 1080 }));
        assert_eq!(serde_json::to_value(InputEvent::VideoKeyframe).unwrap(), json!({ "action": "video-keyframe" }));
        assert_eq!(
            serde_json::to_value(InputEvent::Wheel { norm_x: 0.5, norm_y: 0.25, delta_y: -120 }).unwrap(),
            json!({ "action": "wheel", "normX": 0.5, "normY": 0.25, "deltaY": -120 })
        );
    }

    #[test]
    fn performance_event_wire_shape() {
        let e = InputEvent::SetPerformance { preset: Performance::High, max_fps: 144 };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({ "action": "set-performance", "preset": "high", "maxFps": 144 })
        );
        assert_eq!(
            InputEvent::parse(&json!({ "action": "set-performance", "preset": "low" })),
            Some(InputEvent::SetPerformance { preset: Performance::Low, max_fps: 0 })
        );
        // A preset from a later release is ignored as a whole.
        assert_eq!(InputEvent::parse(&json!({ "action": "set-performance", "preset": "ultra", "maxFps": 60 })), None);
    }

    #[test]
    fn android_and_legacy_shapes_decode() {
        assert_eq!(
            InputEvent::parse(&json!({ "action": "click", "button": 2, "normX": 0.1, "normY": 0.2 })),
            Some(InputEvent::Click { norm_x: 0.1, norm_y: 0.2, button: 2 })
        );
        assert_eq!(
            InputEvent::parse(&json!({ "action": "keydown", "vk": 13 })),
            Some(InputEvent::KeyDown { key_code: 13 })
        );
        assert_eq!(InputEvent::parse(&json!({ "action": "switch-monitor" })), Some(InputEvent::SwitchMonitor));
        assert_eq!(InputEvent::parse(&json!({ "action": "unknown-thing" })), None);
    }

    #[test]
    fn stats_events_round_trip() {
        assert_eq!(
            serde_json::to_value(InputEvent::Stats { on: true }).unwrap(),
            json!({ "action": "stats", "on": true })
        );
        assert_eq!(
            serde_json::to_value(InputEvent::StatsPing { t: 1234 }).unwrap(),
            json!({ "action": "stats-ping", "t": 1234 })
        );
        // Hosts up to 1.2.0 answer without their clock; newer ones add it.
        assert_eq!(
            InputEvent::parse(&json!({ "action": "stats-pong", "t": 1234 })),
            Some(InputEvent::StatsPong { t: 1234, h: None })
        );
        assert_eq!(
            serde_json::to_value(InputEvent::StatsPong { t: 1234, h: Some(99) }).unwrap(),
            json!({ "action": "stats-pong", "t": 1234, "h": 99 })
        );
        assert_eq!(
            serde_json::to_value(InputEvent::StatsPong { t: 1, h: None }).unwrap(),
            json!({ "action": "stats-pong", "t": 1 })
        );
        let stats = HostStats {
            fps: 58.5,
            target_fps: 60,
            grab_ms: 1.25,
            encode_ms: 3.5,
            kbps: 2450,
            gate_skips: 2,
            encoder: "Intel QSV (hardware MFT)".into(),
            ..Default::default()
        };
        let wire = serde_json::to_value(InputEvent::HostStats(stats.clone())).unwrap();
        assert_eq!(
            (&wire["action"], &wire["targetFps"], &wire["grabMs"], &wire["gateSkips"], &wire["encoder"]),
            (&json!("host-stats"), &json!(60), &json!(1.25), &json!(2), &json!("Intel QSV (hardware MFT)"))
        );
        assert_eq!(InputEvent::parse(&wire), Some(InputEvent::HostStats(stats.clone())));
        assert!(wire.get("cpu").is_none() && wire.get("ramMb").is_none());
        let with_usage = HostStats { cpu: Some(3.5), ram_mb: Some(148.0), ..stats };
        let wire = serde_json::to_value(InputEvent::HostStats(with_usage.clone())).unwrap();
        assert_eq!((&wire["cpu"], &wire["ramMb"]), (&json!(3.5), &json!(148.0)));
        assert_eq!(InputEvent::parse(&wire), Some(InputEvent::HostStats(with_usage)));
        // A newer host may add fields, an older one may lack some.
        assert_eq!(
            InputEvent::parse(&json!({ "action": "host-stats", "fps": 30.0, "future": 1 })),
            Some(InputEvent::HostStats(HostStats { fps: 30.0, ..Default::default() }))
        );
    }
}
