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
    /// Name an administrator gave the device on the relay; empty when none.
    pub alias: String,
    pub platform: String,
    pub online: bool,
    /// RFC 3339 timestamp from the relay.
    pub last_seen: String,
}

impl RemoteHost {
    /// The administrator's name for the device, else the name it reports.
    #[cfg(feature = "admin")]
    pub fn display_name(&self) -> &str {
        if self.alias.is_empty() { &self.device_name } else { &self.alias }
    }
}

/// The relay's device allowlist (admin edition).
#[cfg(feature = "admin")]
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SecurityView {
    /// The relay supports the allowlist.
    pub supported: bool,
    pub enabled: bool,
    /// The relay also has OPENDESK_DEVICE_CREDENTIALS configured.
    pub managed_by_env: bool,
    pub devices: Vec<crate::protocol::SecurityDevice>,
}

/// A decoded frame ready for display. The pixel buffer is shared, not copied, on its way to the UI.
pub struct Frame {
    pub pixels: SharedPixelBuffer<Rgba8Pixel>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The session path label (see `viewer::stats::path_label`).
    pub path: &'static str,
    /// Call [`FrameLatency::shown`] when the image reaches the UI toolkit; dropping it records nothing.
    pub latency: crate::service::viewer::stats::FrameLatency,
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
    /// Stats overlay text: newline-separated labels and values (`viewer::stats::SessionStats::columns`).
    Stats(String, String),
}

/// What a monitor-wall tile shows besides its picture (admin edition).
#[cfg(feature = "admin")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileState {
    /// Asked the relay; no answer yet.
    Waiting = 0,
    Live = 1,
    Offline = 2,
    /// Android or an older desktop client.
    Unsupported = 3,
    /// This device is not an administrator on the relay.
    Denied = 4,
}

/// One tile of the visible monitor-wall page.
#[cfg(feature = "admin")]
#[derive(Clone, Debug, PartialEq)]
pub struct WallTile {
    /// Clean desk ID.
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub state: TileState,
}

/// One device in the monitor wall's selection list.
#[cfg(feature = "admin")]
#[derive(Clone, Debug, PartialEq)]
pub struct WallPick {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub selected: bool,
}

/// Everything the monitor wall shows except pictures.
#[cfg(feature = "admin")]
#[derive(Clone, Debug, PartialEq)]
pub struct WallView {
    pub tiles: Vec<WallTile>,
    pub picks: Vec<WallPick>,
    /// Zero-based page and page count.
    pub page: usize,
    pub pages: usize,
    /// Hosts on the wall across all pages.
    pub total: usize,
    pub settings: crate::domain::settings::MonitorSettings,
    /// The relay advertised the monitor capability.
    pub relay_supported: bool,
}

/// Where one file transfer stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferState {
    /// Bytes are moving.
    Active,
    /// Every byte was sent; waiting for the receiver to verify it.
    Verifying,
    Done,
    Failed(String),
}

/// Progress of one file transfer, for the transfer list. Sent again whenever its percentage or state changes.
#[derive(Clone, Debug)]
pub struct TransferUpdate {
    /// Unique per transfer and direction.
    pub id: String,
    /// File name, or the path inside a sent folder.
    pub name: String,
    pub outgoing: bool,
    pub done: u64,
    pub total: u64,
    pub state: TransferState,
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

    /// A file transfer started, progressed or finished.
    fn transfer(&self, _update: TransferUpdate) {}

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

    /// The monitor wall's page, tiles or settings changed (admin edition).
    #[cfg(feature = "admin")]
    fn wall_changed(&self, _view: WallView) {}
    /// New pictures for monitor-wall tiles, at most a few times per second, all in one batch.
    #[cfg(feature = "admin")]
    fn wall_frames(&self, _frames: Vec<(String, SharedPixelBuffer<Rgba8Pixel>)>) {}
    /// The relay's device allowlist changed or was fetched (admin edition).
    #[cfg(feature = "admin")]
    fn security_changed(&self, _view: SecurityView) {}
    /// What the relay supports for administrators, known after each registration (admin edition).
    #[cfg(feature = "admin")]
    fn relay_admin_caps(&self, _device_security: bool, _host_alias: bool) {}
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
