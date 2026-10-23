//! Input and control events carried by `relay-input` / `input-command` (and by the P2P input channel).
//! Coordinates are normalized to 0.0..=1.0 of the streamed monitor.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
}
