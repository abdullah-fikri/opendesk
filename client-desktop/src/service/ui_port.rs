//! The only way services talk to the user interface.
//!
//! Services run on worker threads and must never reach into Slint directly. They report what happened
//! through [`UiPort`]; the Slint presenter (`ui::presenter`) turns that into widget updates on the UI
//! thread, and the headless host uses [`NoUi`]. Methods are fire-and-forget and must not block.

use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::domain::history::HistoryRecord;
use crate::domain::monitor::Monitor;

/// State of the relay connection.
#[derive(Clone, Debug, PartialEq)]
pub enum RelayStatus {
    Connecting,
    Registering,
    Online,
    /// Lost or refused; `reason` replaces the status line when present.
    Offline {
        reason: Option<String>,
    },
}

/// One device of the administrator host list.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteHost {
    pub desk_id: String,
    pub device_name: String,
    pub platform: String,
    pub online: bool,
    /// RFC 3339 timestamp from the relay.
    pub last_seen: String,
}

/// A decoded frame ready for display. The pixel buffer is shared, not copied, on its way to the UI.
pub struct Frame {
    pub pixels: SharedPixelBuffer<Rgba8Pixel>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Arrived over the direct P2P path rather than the relay.
    pub p2p: bool,
}

/// A change to one viewer session's presentation.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionChange {
    Cursor(i32),
    Monitors(Vec<Monitor>, i32),
    ActiveMonitor(i32),
    MicMuted(bool),
    AudioQuality(i32),
    Privacy(i32),
    BlockInput(bool),
    /// Which host controls the partner supports (privacy screen, input lock).
    HostFeatures {
        privacy: bool,
        block_input: bool,
    },
    Resolution(i32),
}

/// Everything services report to the user interface.
pub trait UiPort: Send + Sync {
    /// Replaces the status line.
    fn status(&self, text: String);
    /// The relay connection changed state.
    fn relay_status(&self, status: RelayStatus);
    /// The configured gateway URL changed (e.g. through the CLI).
    fn gateway_changed(&self, url: String);
    /// New administrator host list (this device excluded).
    fn host_list(&self, hosts: Vec<RemoteHost>);
    /// The connection history changed.
    fn history(&self, records: Vec<HistoryRecord>);

    /// A viewer started controlling this machine; `partner` is its display ID.
    fn hosting_started(&self, partner: String);
    /// The hosted session ended.
    fn hosting_ended(&self);
    /// The local microphone state changed while hosting.
    fn hosting_mic(&self, muted: bool);

    /// A viewer session tab was opened (pending until the relay connects it).
    fn session_opened(&self, id: String, title: String, subtitle: String, resolution: i32, audio_quality: i32);
    /// The relay connected the viewer session.
    fn session_connected(&self, id: String, title: String);
    /// A decoded frame for a viewer session.
    fn session_frame(&self, id: String, frame: Frame);
    /// A presentation change for a viewer session.
    fn session_changed(&self, id: String, change: SessionChange);
    /// The viewer session was removed; `status` replaces the status line when present.
    fn session_closed(&self, id: String, status: Option<String>);
    /// Every viewer session was removed at once (relay connection lost).
    fn all_sessions_closed(&self);
    /// Keyboard focus moved to another viewer session (empty: none).
    fn focus_changed(&self, id: String);
    /// The viewer layout changed (0 tabs, 1 split).
    fn layout_changed(&self, layout: i32);
}

/// Headless mode: nothing to show.
pub struct NoUi;

impl UiPort for NoUi {
    fn status(&self, _: String) {}
    fn relay_status(&self, _: RelayStatus) {}
    fn gateway_changed(&self, _: String) {}
    fn host_list(&self, _: Vec<RemoteHost>) {}
    fn history(&self, _: Vec<HistoryRecord>) {}
    fn hosting_started(&self, _: String) {}
    fn hosting_ended(&self) {}
    fn hosting_mic(&self, _: bool) {}
    fn session_opened(&self, _: String, _: String, _: String, _: i32, _: i32) {}
    fn session_connected(&self, _: String, _: String) {}
    fn session_frame(&self, _: String, _: Frame) {}
    fn session_changed(&self, _: String, _: SessionChange) {}
    fn session_closed(&self, _: String, _: Option<String>) {}
    fn all_sessions_closed(&self) {}
    fn focus_changed(&self, _: String) {}
    fn layout_changed(&self, _: i32) {}
}
