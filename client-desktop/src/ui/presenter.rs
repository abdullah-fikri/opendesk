//! [`UiPort`] for the Slint window. Every call may come from any thread; it is forwarded to the UI thread
//! with `upgrade_in_event_loop` and applied there.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use parking_lot::Mutex;
use slint::{ComponentHandle, Image, Weak};

use super::{MainWindow, session_model, transfer_model, view_model};
use crate::domain::history::HistoryRecord;
use crate::service::file_transfer::{self, IN_FLIGHT_CHUNKS, LAST_RECEIVED_MS};
use crate::service::ui_port::{Frame, RelayStatus, RemoteHost, SessionChange, TransferUpdate, UiPort};

/// The frame-rate line in the status bar yields to file-transfer progress for this long after a transfer.
const TRANSFER_STATUS_HOLD_MS: u64 = 2000;

/// Newest frame per session not yet shown. A session has an entry exactly while a closure that will show it is
/// queued on the UI thread, so a busy UI thread holds one frame per session instead of a backlog. Frames
/// overwritten here are display-only: the decoder has already applied every delta to its own framebuffer.
type PendingFrames = Arc<Mutex<HashMap<String, Frame>>>;

/// Newest monitor-wall thumbnail per host not yet shown; non-empty exactly while a closure is queued.
#[cfg(feature = "admin")]
type PendingWall = Arc<Mutex<HashMap<String, slint::SharedPixelBuffer<slint::Rgba8Pixel>>>>;

/// Presents service events on the main window.
pub struct SlintUi {
    window: Weak<MainWindow>,
    frames: PendingFrames,
    #[cfg(feature = "admin")]
    wall: PendingWall,
}

impl SlintUi {
    /// Must be created on the UI thread, before the window runs.
    pub fn new(window: &MainWindow) -> Self {
        session_model::install(window);
        transfer_model::install(window);
        #[cfg(feature = "admin")]
        super::monitor_wall_model::install(window);
        Self {
            window: window.as_weak(),
            frames: PendingFrames::default(),
            #[cfg(feature = "admin")]
            wall: PendingWall::default(),
        }
    }

    fn apply(&self, f: impl FnOnce(&MainWindow) + Send + 'static) {
        let _ = self.window.upgrade_in_event_loop(move |w| f(&w));
    }
}

fn transfer_busy() -> bool {
    IN_FLIGHT_CHUNKS.load(Ordering::Relaxed) > 0
        || file_transfer::now_ms().saturating_sub(LAST_RECEIVED_MS.load(Ordering::Relaxed)) < TRANSFER_STATUS_HOLD_MS
}

impl UiPort for SlintUi {
    fn status(&self, text: String) {
        self.apply(move |w| w.set_status_text(text.into()));
    }

    fn relay_status(&self, status: RelayStatus) {
        self.apply(move |w| match status {
            RelayStatus::Connecting => {
                w.set_status_text("Connecting...".into());
                w.set_is_relay_online(false);
            }
            RelayStatus::Registering => {
                w.set_status_text("Registering device...".into());
                w.set_is_relay_online(false);
            }
            RelayStatus::Online => {
                w.set_status_text("Online".into());
                w.set_is_relay_online(true);
            }
            RelayStatus::Offline { reason } => {
                w.set_is_relay_online(false);
                w.set_hosting(false);
                w.set_hosting_mic_muted(true);
                w.set_status_text(reason.unwrap_or_else(|| "Disconnected".into()).into());
            }
        });
    }

    fn transfer(&self, update: TransferUpdate) {
        self.apply(move |_| transfer_model::apply(update));
    }

    fn gateway_changed(&self, url: String) {
        self.apply(move |w| w.set_gateway_url(url.into()));
    }

    fn host_list(&self, hosts: Vec<RemoteHost>) {
        self.apply(move |w| w.set_server_hosts(view_model::host_items(hosts)));
    }

    fn history(&self, records: Vec<HistoryRecord>) {
        self.apply(move |w| w.set_history_items(view_model::history_items(&records)));
    }

    fn hosting_started(&self, partner: String) {
        self.apply(move |w| {
            w.set_hosting_partner(partner.into());
            w.set_hosting_mic_muted(false);
            w.set_hosting(true);
            w.set_status_text("Session active".into());
        });
    }

    fn hosting_ended(&self) {
        self.apply(|w| {
            let was_hosting = w.get_hosting();
            w.set_hosting(false);
            w.set_hosting_mic_muted(true);
            if was_hosting && session_model::is_empty() {
                w.set_status_text("Online".into());
            }
        });
    }

    fn hosting_mic(&self, muted: bool) {
        self.apply(move |w| w.set_hosting_mic_muted(muted));
    }

    fn session_opened(&self, id: String, title: String, subtitle: String, resolution: i32, audio_quality: i32) {
        self.apply(move |w| {
            session_model::ensure(&id, title, subtitle, resolution, audio_quality);
            w.set_show_new_session(false);
        });
    }

    fn session_connected(&self, id: String, title: String) {
        self.apply(move |w| {
            session_model::ensure(&id, title, String::new(), 1, 2);
            session_model::update(&id, |r| {
                r.connecting = true;
                r.mic_muted = false;
                r.privacy_mode = 0;
                r.block_input = false;
                r.status = session_model::CONNECTING.into();
            });
            w.set_show_new_session(false);
            if w.get_focused_session() == id.as_str() {
                w.set_status_text(session_model::CONNECTING.into());
            }
        });
    }

    fn session_frame(&self, id: String, frame: Frame) {
        if self.frames.lock().insert(id.clone(), frame).is_some() {
            return; // the queued closure shows this newer frame instead
        }
        let pending = Arc::clone(&self.frames);
        self.apply(move |w| {
            let Some(frame) = pending.lock().remove(&id) else { return };
            let image = Image::from_rgba8(frame.pixels);
            let path = frame.path;
            let (width, height) = (frame.width as i32, frame.height as i32);
            let fps = frame.fps as i32;
            // The picture changes every frame; the other fields only rarely, so they are written only on change.
            let found = session_model::update(&id, |r| {
                r.frame = image;
                if !r.has_frame || r.connecting {
                    r.has_frame = true;
                    r.connecting = false;
                }
                if r.fps != fps {
                    r.fps = fps;
                }
                if (r.stream_width, r.stream_height) != (width, height) {
                    r.stream_width = width;
                    r.stream_height = height;
                }
                if r.path.as_str() != path {
                    r.path = path.into();
                }
                if r.status.as_str() == session_model::CONNECTING {
                    r.status = Default::default();
                }
            });
            if found && w.get_focused_session() == id.as_str() && !w.get_hosting() && !transfer_busy() {
                w.set_status_text(format!("{width}x{height} • {} FPS • {path}", frame.fps).into());
            }
        });
    }

    fn session_changed(&self, id: String, change: SessionChange) {
        self.apply(move |_| {
            session_model::update(&id, |r| match change {
                SessionChange::Cursor(shape) => r.cursor = shape,
                SessionChange::Monitors(monitors, active) => {
                    r.monitors = view_model::monitor_items(monitors);
                    r.active_monitor = active;
                }
                SessionChange::ActiveMonitor(index) => r.active_monitor = index,
                SessionChange::MicMuted(muted) => r.mic_muted = muted,
                SessionChange::AudioQuality(q) => r.audio_quality = q,
                SessionChange::Privacy(mode) => r.privacy_mode = mode,
                SessionChange::BlockInput(blocked) => r.block_input = blocked,
                SessionChange::HostFeatures { privacy, block_input } => {
                    r.privacy_unavailable = !privacy;
                    r.block_input_unavailable = !block_input;
                }
                SessionChange::Resolution(mode) => r.resolution_mode = mode,
                SessionChange::Stats(labels, values) => {
                    r.stats_labels = labels.into();
                    r.stats_values = values.into();
                }
            });
        });
    }

    fn session_closed(&self, id: String, status: Option<String>) {
        self.frames.lock().remove(&id);
        self.apply(move |w| {
            session_model::remove(&id);
            if session_model::is_empty() {
                w.set_show_new_session(false);
                if !w.get_hosting() {
                    w.set_status_text(status.unwrap_or_else(|| "Online".into()).into());
                    return;
                }
            }
            if let Some(status) = status {
                w.set_status_text(status.into());
            }
        });
    }

    fn all_sessions_closed(&self) {
        self.frames.lock().clear();
        self.apply(|w| {
            session_model::clear();
            w.set_focused_session("".into());
            w.set_show_new_session(false);
        });
    }

    fn focus_changed(&self, id: String) {
        self.apply(move |w| w.set_focused_session(id.into()));
    }

    fn layout_changed(&self, layout: i32) {
        self.apply(move |w| w.set_view_layout(layout));
    }

    #[cfg(feature = "admin")]
    fn wall_changed(&self, view: crate::service::ui_port::WallView) {
        use super::monitor_wall_model;
        self.apply(move |w| {
            monitor_wall_model::set_tiles(view.tiles);
            w.set_monitor_picks(monitor_wall_model::picks(view.picks));
            w.set_monitor_grid(view.settings.grid as i32);
            w.set_monitor_page(view.page as i32);
            w.set_monitor_pages(view.pages as i32);
            w.set_monitor_total(view.total as i32);
            w.set_monitor_rotate(view.settings.rotate);
            w.set_monitor_rotate_secs(view.settings.rotate_secs as i32);
            w.set_monitor_all(view.settings.all);
            w.set_monitor_relay_ok(view.relay_supported);
        });
    }

    #[cfg(feature = "admin")]
    fn security_changed(&self, view: crate::service::ui_port::SecurityView) {
        self.apply(move |w| {
            w.set_security_supported(view.supported);
            w.set_security_enabled(view.enabled);
            w.set_security_managed_by_env(view.managed_by_env);
            w.set_security_items(view_model::security_items(view.devices));
        });
    }

    #[cfg(feature = "admin")]
    fn relay_admin_caps(&self, device_security: bool, host_alias: bool) {
        self.apply(move |w| {
            w.set_security_supported(device_security);
            w.set_host_alias_supported(host_alias);
        });
    }

    #[cfg(feature = "admin")]
    fn wall_frames(&self, frames: Vec<(String, slint::SharedPixelBuffer<slint::Rgba8Pixel>)>) {
        let mut pending = self.wall.lock();
        let idle = pending.is_empty();
        pending.extend(frames);
        if !idle {
            return; // the queued closure shows these too
        }
        drop(pending);
        let pending = Arc::clone(&self.wall);
        self.apply(move |_| {
            let frames: Vec<_> = pending.lock().drain().collect();
            super::monitor_wall_model::set_frames(frames);
        });
    }
}
