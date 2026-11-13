//! Host role: a remote viewer controls this machine. One hosted session at a time.
//!
//! - [`capture`]: the screen capture and encode loop.
//! - [`input`]: applies the viewer's input and control events.
//! - [`monitor_stream`]: view-only thumbnails for administrators' monitor walls, independent of the session.

pub mod capture;
pub mod input;
pub mod monitor_stream;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use core_engine::capture::{MonitorBounds, enumerate_monitors};
use core_engine::input::InputInjector;
use parking_lot::Mutex;

use crate::domain::capability::PeerCaps;
use crate::domain::{desk_id, resolution};
use crate::infra::send_gate::SendGate;
use crate::infra::{audio, logger, privacy};
use crate::protocol::InputEvent;
use crate::service::context::{AppContext, Ctx};

/// State of the hosted session, shared by the relay connection, the capture loop and the UI.
pub struct HostState {
    active: AtomicBool,
    /// Bumped on every start and stop; a capture loop exits once its generation is stale.
    capture_generation: AtomicU64,
    /// Monitor the viewer asked for (the capture loop wraps it to the monitor count).
    pub monitor_index: AtomicUsize,
    /// Requested stream size (see `domain::resolution::host_mode`).
    pub resolution_mode: AtomicUsize,
    /// Monitor being streamed; maps normalized pointer coordinates to the desktop.
    pub monitor_bounds: Mutex<MonitorBounds>,
    /// Backpressure gate: the capture loop only encodes while the previous frame has left the socket.
    /// Shared with the P2P path, whose frame acknowledgements also open it.
    pub can_send_frame: Arc<SendGate>,
    /// The hosted session's video goes over a direct P2P path (updated by the relay connection).
    pub p2p_active: AtomicBool,
    /// What the hosted viewer supports.
    pub peer_caps: Mutex<PeerCaps>,
    /// The viewer lost a delta frame; the capture loop answers with a key frame.
    pub keyframe_requested: AtomicBool,
    /// The viewer shows its stats overlay: the capture loop sends `host-stats` once a second.
    pub stats_requested: AtomicBool,
    /// Keys injected as down and not yet released, so a session end never leaves one stuck.
    held_keys: Mutex<Vec<u8>>,
    /// An administrator's monitor wall shows this machine (see [`monitor_stream`]).
    pub monitored: AtomicBool,
    /// Bumped on every monitor-stream start and stop; a stream thread exits once its generation is stale.
    pub monitor_generation: AtomicU64,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            capture_generation: AtomicU64::new(0),
            monitor_index: AtomicUsize::new(0),
            resolution_mode: AtomicUsize::new(resolution::DEFAULT_PRESET),
            monitor_bounds: Mutex::new(enumerate_monitors()[0]),
            can_send_frame: Arc::new(SendGate::new(true)),
            p2p_active: AtomicBool::new(false),
            peer_caps: Mutex::default(),
            keyframe_requested: AtomicBool::new(false),
            stats_requested: AtomicBool::new(false),
            held_keys: Mutex::new(Vec::new()),
            monitored: AtomicBool::new(false),
            monitor_generation: AtomicU64::new(0),
        }
    }
}

impl HostState {
    /// True while a viewer controls this machine.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// True while the capture loop of `generation` should keep running.
    fn is_current(&self, generation: u64) -> bool {
        self.capture_generation.load(Ordering::SeqCst) == generation
    }

    /// Presses `vk` and remembers it.
    pub fn key_down(&self, injector: &InputInjector, vk: u8) {
        injector.key_down(vk);
        let mut held = self.held_keys.lock();
        if !held.contains(&vk) {
            held.push(vk);
        }
    }

    /// Releases `vk`.
    pub fn key_up(&self, injector: &InputInjector, vk: u8) {
        injector.key_up(vk);
        self.held_keys.lock().retain(|&k| k != vk);
    }

    /// Releases every key the viewer left pressed (disconnect, lost relay, viewer crash).
    pub fn release_all_keys(&self, injector: &InputInjector) {
        let keys = std::mem::take(&mut *self.held_keys.lock());
        for vk in keys.into_iter().rev() {
            injector.key_up(vk);
        }
    }
}

/// A viewer connected to this machine: start streaming to it, with this machine's microphone muted when
/// `mic_muted` (the local setting).
pub fn start(ctx: &Ctx, partner: &str, caps: PeerCaps, mic_muted: bool) {
    ctx.set_partner(partner);
    *ctx.host.peer_caps.lock() = caps;
    // Host sound by default toward a viewer that plays it; `set-host-sound` switches later.
    ctx.voice.mode.set(audio::session_audio(true, caps.audio_system, true));
    // The viewer keyboard hook belongs to viewing; this machine is being controlled now.
    privacy::set_viewer_hook_state(false, "");
    ctx.host.monitor_index.store(0, Ordering::SeqCst);
    // An automatic size belongs to the viewer that asked for it; a new viewer starts from the default.
    if resolution::is_auto(ctx.host.resolution_mode.load(Ordering::SeqCst)) {
        ctx.host.resolution_mode.store(resolution::DEFAULT_PRESET, Ordering::SeqCst);
    }
    ctx.host.can_send_frame.open();
    ctx.host.p2p_active.store(false, Ordering::SeqCst);
    ctx.host.stats_requested.store(false, Ordering::SeqCst);
    ctx.host.active.store(true, Ordering::SeqCst);
    let generation = ctx.host.capture_generation.fetch_add(1, Ordering::SeqCst) + 1;
    capture::spawn(Arc::clone(ctx), partner.to_string(), generation);
    ctx.ui.hosting_started(desk_id::format(partner));
    set_mic(ctx, mic_muted);
    logger::info("host", &format!("Hosting session started for {partner}."));
}

/// The hosted session resumed after its relay connection came back: capture starts over, so the viewer gets
/// a key frame, the monitor list and the cursor shape again.
pub fn restart_capture(ctx: &Ctx) {
    if !ctx.host.active.load(Ordering::SeqCst) {
        return;
    }
    let partner = ctx.partner();
    ctx.host.can_send_frame.open();
    let generation = ctx.host.capture_generation.fetch_add(1, Ordering::SeqCst) + 1;
    capture::spawn(Arc::clone(ctx), partner, generation);
    logger::info("host", "Hosted session resumed.");
}

/// The hosted session ended: stop capturing and undo everything the viewer changed on this machine.
pub fn stop(ctx: &AppContext) {
    if !ctx.host.active.swap(false, Ordering::SeqCst) {
        return;
    }
    ctx.host.capture_generation.fetch_add(1, Ordering::SeqCst);
    privacy::reset_host_privacy();
    ctx.host.release_all_keys(&ctx.injector);
    ctx.voice.mode.set(audio::SessionAudio::Voice);
    logger::info("host", "Hosting session ended.");
}

/// Sets this machine's microphone while hosting, shows it in the host panel and reports it to a viewer that
/// can switch it (`remote-mic`).
pub(crate) fn set_mic(ctx: &AppContext, muted: bool) {
    ctx.voice.is_mic_muted.store(muted, Ordering::SeqCst);
    ctx.ui.hosting_mic(muted);
    if ctx.host.peer_caps.lock().remote_mic {
        ctx.send_input(&ctx.partner(), InputEvent::HostMic { muted });
    }
}

/// Toggles this machine's microphone toward the hosted viewer and tells it.
pub fn toggle_mic(ctx: &AppContext) {
    let muted = !ctx.voice.is_mic_muted.load(Ordering::SeqCst);
    set_mic(ctx, muted);
    ctx.send_input(&ctx.partner(), InputEvent::SetMic { muted });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::test_support::{MicRecorder, context_with_ui};

    #[test]
    fn hosted_sessions_start_with_the_microphone_setting_and_the_panel_shows_it() {
        let ui = Arc::new(MicRecorder::default());
        let (ctx, _outbox) = context_with_ui(ui.clone());
        set_mic(&ctx, true);
        assert!(ctx.voice.is_mic_muted.load(Ordering::SeqCst));
        assert_eq!(ui.shown.lock().last(), Some(&("hosting".to_string(), true)));
        // The local user can still turn it on.
        toggle_mic(&ctx);
        assert!(!ctx.voice.is_mic_muted.load(Ordering::SeqCst));
        assert_eq!(ui.shown.lock().last(), Some(&("hosting".to_string(), false)));
    }

    #[test]
    fn the_host_reports_its_microphone_only_to_viewers_that_can_switch_it() {
        let (ctx, outbox) = crate::service::test_support::context_with_outbox();
        ctx.set_partner("222222222");
        let sent = |outbox: &std::sync::mpsc::Receiver<crate::protocol::Outgoing>| {
            outbox.try_iter().map(|o| format!("{o:?}")).filter(|o| o.contains("HostMic")).count()
        };
        set_mic(&ctx, true);
        assert_eq!(sent(&outbox), 0, "an older viewer does not know host-mic");
        ctx.host.peer_caps.lock().remote_mic = true;
        set_mic(&ctx, false);
        assert!(!ctx.voice.is_mic_muted.load(Ordering::SeqCst), "the viewer may turn it on");
        assert_eq!(sent(&outbox), 1);
    }
}
