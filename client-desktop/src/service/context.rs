//! Shared handles of the running client, built once by the composition root (`app.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;

use core_engine::input::InputInjector;
use parking_lot::Mutex;

use crate::domain::capability::RelayCaps;
use crate::domain::edition::Edition;
use crate::infra::audio::VoiceEngine;
use crate::protocol::{ClientMessage, InputEvent, Outgoing};
use crate::service::host::HostState;
use crate::service::ui_port::UiPort;
use crate::service::viewer::Registry;

/// Everything services share. Cheap to clone as [`Ctx`]; every field is thread-safe.
pub struct AppContext {
    pub edition: Edition,
    /// This device's clean desk ID.
    pub local_id: String,
    /// Title of the main window (drag-and-drop and the tray find the window by it).
    pub window_title: String,
    /// Outbound queue of the relay connection.
    pub outbox: Sender<Outgoing>,
    pub ui: Arc<dyn UiPort>,
    /// Viewer sessions (hosts this machine controls).
    pub viewers: Registry,
    /// Host role (a viewer controls this machine).
    pub host: HostState,
    /// Monitor wall (admin edition).
    #[cfg(feature = "admin")]
    pub wall: crate::service::monitor_wall::MonitorWall,
    pub voice: Arc<VoiceEngine>,
    pub injector: InputInjector,
    pub relay: RelayLink,
    /// Clipboard sequence number already synced with the partner, so the watcher never echoes a change back.
    pub clipboard_seq: AtomicU32,
    /// Current partner: the viewer this machine hosts, else the focused connected viewer session.
    /// Voice, clipboard sync and dropped files go to it.
    partner: Mutex<String>,
}

/// Shared handle passed to every service.
pub type Ctx = Arc<AppContext>;

/// Relay connection settings and what the relay supports.
pub struct RelayLink {
    pub gateway_url: Mutex<String>,
    /// Set to drop the connection and reconnect (gateway changed, tray "Reconnect").
    pub reconnect: AtomicBool,
    pub caps: Mutex<RelayCaps>,
    /// Registered with the relay (set on `registered-ack`, cleared when a connection attempt starts).
    pub online: AtomicBool,
}

impl AppContext {
    /// Builds the context; `outbox` must be the sender of the relay connection's queue.
    pub fn new(
        edition: Edition,
        local_id: String,
        gateway_url: String,
        outbox: Sender<Outgoing>,
        ui: Arc<dyn UiPort>,
        voice: Arc<VoiceEngine>,
    ) -> Self {
        Self {
            edition,
            window_title: format!("OpenDesk - {}", crate::domain::desk_id::format(&local_id)),
            local_id,
            outbox,
            ui,
            viewers: Registry::new(),
            host: HostState::default(),
            #[cfg(feature = "admin")]
            wall: Default::default(),
            voice,
            injector: InputInjector::new(),
            relay: RelayLink {
                gateway_url: Mutex::new(gateway_url),
                reconnect: AtomicBool::new(false),
                caps: Mutex::default(),
                online: AtomicBool::new(false),
            },
            clipboard_seq: AtomicU32::new(core_engine::clipboard::sequence_number()),
            partner: Mutex::new(String::new()),
        }
    }

    /// Queues a message for the relay.
    pub fn send(&self, message: ClientMessage) {
        let _ = self.outbox.send(Outgoing::Message(message));
    }

    /// Queues an input event for `target`; empty targets are ignored.
    pub fn send_input(&self, target: &str, event: InputEvent) {
        if !target.is_empty() {
            self.send(ClientMessage::input(target, event));
        }
    }

    /// The current partner's clean ID (empty: none).
    pub fn partner(&self) -> String {
        self.partner.lock().clone()
    }

    /// Replaces the current partner.
    pub fn set_partner(&self, id: &str) {
        *self.partner.lock() = id.to_string();
    }

    /// True while a viewer controls this machine.
    pub fn is_hosting(&self) -> bool {
        self.host.is_active()
    }

    /// Marks the current clipboard as already synced.
    pub fn mark_clipboard_synced(&self) -> u32 {
        let seq = core_engine::clipboard::sequence_number();
        self.clipboard_seq.store(seq, Ordering::SeqCst);
        seq
    }
}
