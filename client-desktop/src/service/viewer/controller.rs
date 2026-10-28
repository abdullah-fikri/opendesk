//! Viewer use cases: opening, focusing and closing sessions, and every action a user takes on one
//! (input, microphone, privacy, resolution, monitors, files). Rules from `docs/features/viewer-sessions.md`:
//! only the focused session gets keyboard input, a click on an unfocused tile only focuses it, and the
//! clipboard follows focus.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::domain::capability::PeerCaps;
use crate::domain::keyboard::{KeyOut, Mods};
use crate::domain::{desk_id, resolution};
use crate::infra::{logger, privacy, user_clipboard, virtual_clipboard, window};
use crate::protocol::{ClientMessage, InputEvent, Outgoing};
use crate::service::context::{AppContext, Ctx};
use crate::service::file_transfer;
use crate::service::ui_port::SessionChange;
use crate::service::viewer::decoder;
use crate::service::viewer::registry::{Open, Session, clipboard_push_needed};

const MSG_FULL_MULTI: &str = "At most 4 sessions can be open at once. Close one first.";
const MSG_FULL_SINGLE: &str = "This relay supports one session at a time. Close the running session first.";

/// Mouse input kinds from the session canvas.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mouse {
    Move,
    Down(i32),
    Up(i32),
    Wheel(i32),
}

// ---- Focus ----

/// Applies the registry's focus outside the UI: current partner, keyboard hook, voice and file caps.
/// The host role (this PC being controlled) is never touched.
pub fn apply_focus(ctx: &AppContext) {
    let focused = ctx.viewers.focused().filter(|s| s.is_connected());
    if !ctx.is_hosting() {
        ctx.set_partner(focused.as_ref().map(|s| s.id.as_str()).unwrap_or(""));
        match &focused {
            Some(s) => privacy::set_viewer_hook_state(true, &s.id),
            None => privacy::set_viewer_hook_state(false, ""),
        }
    }
    if let Some(s) = focused {
        ctx.voice.is_mic_muted.store(s.mic_muted.load(Ordering::SeqCst), Ordering::SeqCst);
        ctx.voice.audio_quality.store(s.audio_quality.load(Ordering::SeqCst), Ordering::SeqCst);
        // While hosting, voice goes to the hosted viewer, whose codec the host session chose.
        if !ctx.is_hosting() {
            ctx.voice.use_opus.store(file_transfer::caps_of(&s.id).audio_opus, Ordering::SeqCst);
        }
    }
}

/// Completes a focus change away from `prev`: its held keys are released and the newly focused host gets the
/// viewer's clipboard.
fn after_focus_change(ctx: &AppContext, prev: &str) {
    apply_focus(ctx);
    let focused = ctx.viewers.focused_id();
    if prev != focused {
        if !prev.is_empty() {
            release_keys(ctx, prev);
        }
        if let Some(s) = ctx.viewers.get(&focused).filter(|s| s.is_connected()) {
            push_clipboard(ctx, &s);
        }
    }
    ctx.ui.focus_changed(focused);
}

/// Sends the viewer's clipboard to a newly focused host unless that host already has it.
fn push_clipboard(ctx: &AppContext, s: &Arc<Session>) {
    let current = core_engine::clipboard::sequence_number();
    if !clipboard_push_needed(current, s.clip_seq.load(Ordering::SeqCst)) {
        return;
    }
    s.clip_seq.store(current, Ordering::SeqCst);
    // The clipboard watcher must not send the same copy again.
    ctx.clipboard_seq.store(current, Ordering::SeqCst);
    let clip = user_clipboard::read();
    if file_transfer::caps_of(&s.id).file_offer {
        let files = clip.files;
        if !files.is_empty() {
            // Files that came from a host are never offered back.
            if !file_transfer::is_received_clipboard(&files) {
                s.offered_seq.store(current, Ordering::SeqCst);
                file_transfer::offer::offer_files(ctx, files, s.id.clone());
            }
            return;
        }
    }
    if let Some(text) = clip.text {
        ctx.send(ClientMessage::RelayClipboard { target_clean: s.id.clone(), text });
    }
}

/// Focuses session `id` (tab click, tile click).
pub fn focus(ctx: &AppContext, id: &str) {
    match ctx.viewers.set_focus(id) {
        Some(prev) => after_focus_change(ctx, &prev),
        None => ctx.ui.focus_changed(ctx.viewers.focused_id()),
    }
}

/// 0 = tabs, 1 = split view. Sessions that become visible ask their host for a key frame themselves.
pub fn set_layout(ctx: &AppContext, layout: i32) {
    ctx.viewers.set_layout(layout);
    ctx.ui.layout_changed(ctx.viewers.layout());
}

// ---- Opening and closing ----

/// Opens a session to `clean` (pending until the relay connects it) or focuses an open one.
pub fn request_connect(ctx: &AppContext, clean: &str, title: String, subtitle: String) {
    if clean.is_empty() {
        // The relay answers with its own "enter a target ID" error.
        ctx.send(ClientMessage::ConnectRequest { target_id: String::new() });
        return;
    }
    let res_mode = ctx.viewers.default_resolution();
    let viewport = ctx.viewers.default_viewport();
    let quality = ctx.voice.audio_quality.load(Ordering::SeqCst);
    let mut opened = ctx.viewers.open(clean, res_mode, viewport, quality);
    if matches!(opened, Open::Full) && !ctx.viewers.multi() {
        // Single-session relay: a connect still waiting (Android approval) is replaced by the new one;
        // a running session is never ended implicitly.
        if let Some(pending) = ctx.viewers.all().into_iter().find(|s| !s.is_connected()) {
            close(ctx, &pending.id, None);
            opened = ctx.viewers.open(clean, res_mode, viewport, quality);
        }
    }
    match opened {
        Open::Exists(_) => focus(ctx, clean),
        Open::Full => ctx.ui.status(if ctx.viewers.multi() { MSG_FULL_MULTI } else { MSG_FULL_SINGLE }.into()),
        Open::Opened(s) => {
            ctx.ui.session_opened(
                s.id.clone(),
                title,
                subtitle,
                s.res_mode.load(Ordering::SeqCst) as i32,
                s.audio_quality.load(Ordering::SeqCst) as i32,
            );
            focus(ctx, clean);
            ctx.send(ClientMessage::ConnectRequest { target_id: clean.to_string() });
            ctx.ui.status(format!("Connecting to {}...", desk_id::format(clean)));
            logger::info("viewer", &format!("Connection requested to {clean}."));
        }
    }
}

/// The relay connected session `id`. Returns the session, or `None` when there is no room for it (the relay
/// enforces the same limit, so this is a stale session and it is ended).
pub fn on_connected(ctx: &Ctx, id: &str, caps: PeerCaps) -> Option<Arc<Session>> {
    let res_mode = ctx.viewers.default_resolution();
    let viewport = ctx.viewers.default_viewport();
    let quality = ctx.voice.audio_quality.load(Ordering::SeqCst);
    let sess = match ctx.viewers.open(id, res_mode, viewport, quality) {
        Open::Opened(s) => {
            let title = desk_id::format(id);
            ctx.ui.session_opened(id.to_string(), title, String::new(), res_mode as i32, quality as i32);
            s
        }
        Open::Exists(s) => s,
        Open::Full => {
            logger::warn("viewer", "No room for a new viewer session; ending it.");
            ctx.send(ClientMessage::disconnect(id));
            return None;
        }
    };
    sess.connected.store(true, Ordering::SeqCst);
    sess.mic_muted.store(false, Ordering::SeqCst);
    sess.clip_seq.store(core_engine::clipboard::sequence_number(), Ordering::SeqCst);
    file_transfer::set_session_caps(id, caps);
    ctx.ui.session_changed(
        id.to_string(),
        SessionChange::HostFeatures { privacy: caps.privacy_screen, block_input: caps.block_input },
    );
    if ctx.viewers.focused_id().is_empty() {
        ctx.viewers.set_focus(id);
    }
    apply_focus(ctx);

    let (ui_ctx, stats_ctx) = (Arc::clone(ctx), Arc::clone(ctx));
    let (crash_ctx, crashed) = (Arc::clone(ctx), Arc::clone(&sess));
    decoder::spawn(
        Arc::clone(&sess),
        ctx.outbox.clone(),
        move |s, frame| ui_ctx.ui.session_frame(s.id.clone(), frame),
        move |s, (labels, values)| stats_ctx.ui.session_changed(s.id.clone(), SessionChange::Stats(labels, values)),
        // A dead decoder leaves a frozen tab: close it like the user would, unless it was already replaced.
        move || {
            if crash_ctx.viewers.get(&crashed.id).is_some_and(|s| Arc::ptr_eq(&s, &crashed)) {
                close(&crash_ctx, &crashed.id, Some("The session was closed after an internal error.".into()));
            }
        },
    );
    if ctx.viewers.stats_overlay() {
        request_stats(ctx, &sess, true);
    }

    // Tell the host the preferred resolution and voice quality. Automatic mode uses the last canvas size;
    // the new canvas reports again if it differs.
    ctx.send_input(id, resolution_event(&sess));
    ctx.send_input(id, InputEvent::SetAudioQuality { quality: sess.audio_quality.load(Ordering::SeqCst) as u64 });
    ctx.ui.session_connected(id.to_string(), desk_id::format(id));
    ctx.ui.focus_changed(ctx.viewers.focused_id());
    Some(sess)
}

/// Removes session `id` locally (no message to the relay). Returns false if it did not exist.
fn remove(ctx: &AppContext, id: &str) -> bool {
    if ctx.viewers.remove(id).is_none() {
        return false;
    }
    virtual_clipboard::clear_offer(id);
    file_transfer::remove_session_caps(id);
    true
}

/// Ends session `id` without telling the relay (it told us). `status` replaces the status line.
pub fn end(ctx: &AppContext, id: &str, status: Option<String>) {
    let prev = ctx.viewers.focused_id();
    if !remove(ctx, id) {
        return;
    }
    logger::info("viewer", &format!("Viewer session with {id} closed."));
    after_end(ctx, id, &prev, status);
}

/// Ends session `id` at the user's request and tells the relay.
pub fn close(ctx: &AppContext, id: &str, status: Option<String>) {
    if ctx.viewers.get(id).is_none() {
        return;
    }
    ctx.send(ClientMessage::disconnect(id));
    end(ctx, id, status);
}

fn after_end(ctx: &AppContext, id: &str, prev: &str, status: Option<String>) {
    ctx.ui.session_closed(id.to_string(), status);
    after_focus_change(ctx, prev);
    if ctx.viewers.is_empty() && !ctx.is_hosting() {
        ctx.voice.is_session_active.store(false, Ordering::SeqCst);
        ctx.voice.is_mic_muted.store(true, Ordering::SeqCst);
    }
}

/// Ends every viewer session at once (relay connection lost, tray "Disconnect").
pub fn end_all(ctx: &AppContext) -> Vec<String> {
    let ids: Vec<String> = ctx.viewers.clear().into_iter().map(|s| s.id.clone()).collect();
    for id in &ids {
        virtual_clipboard::clear_offer(id);
        file_transfer::remove_session_caps(id);
    }
    if !ids.is_empty() {
        privacy::set_viewer_hook_state(false, "");
        ctx.ui.all_sessions_closed();
    }
    ids
}

// ---- Events from a viewed host ----

/// Applies an event sent by host `sess` (cursor, monitors, microphone, voice quality).
pub fn on_host_event(ctx: &AppContext, sess: &Session, event: InputEvent) {
    let id = sess.id.clone();
    let focused = ctx.viewers.is_focused(&id);
    match event {
        InputEvent::CursorShape { shape } => ctx.ui.session_changed(id, SessionChange::Cursor(shape)),
        // A remote peer may silence this microphone, but only the local user can enable it.
        InputEvent::SetMic { muted: true } => {
            sess.mic_muted.store(true, Ordering::SeqCst);
            if focused {
                ctx.voice.is_mic_muted.store(true, Ordering::SeqCst);
            }
            ctx.ui.session_changed(id, SessionChange::MicMuted(true));
        }
        InputEvent::SetAudioQuality { quality } => {
            let q = quality.min(2) as usize;
            sess.audio_quality.store(q, Ordering::SeqCst);
            if focused {
                ctx.voice.audio_quality.store(q, Ordering::SeqCst);
            }
            ctx.ui.session_changed(id, SessionChange::AudioQuality(q as i32));
        }
        InputEvent::Monitors { list, active } => {
            let (monitors, active) = crate::domain::monitor::from_announcement(
                list.iter().map(|m| (m.index, m.width, m.height, m.primary)),
                active,
            );
            sess.has_monitors.store(true, Ordering::SeqCst);
            ctx.ui.session_changed(id, SessionChange::Monitors(monitors, active));
        }
        // Shown with the decoder thread's next once-a-second refresh.
        InputEvent::HostStats(stats) => sess.stats.lock().host = Some(stats),
        InputEvent::StatsPong { t } => {
            sess.stats.lock().rtt_ms = Some(file_transfer::now_ms().saturating_sub(t));
        }
        _ => {}
    }
}

// ---- Stats overlay ----

/// Shows or hides the stats overlay of every session (off at start, kept for the app run). Hosts send
/// `host-stats` only while it is shown.
pub fn set_stats_overlay(ctx: &AppContext, on: bool) {
    ctx.viewers.set_stats_overlay(on);
    for s in ctx.viewers.all().iter().filter(|s| s.is_connected()) {
        request_stats(ctx, s, on);
    }
}

fn request_stats(ctx: &AppContext, s: &Session, on: bool) {
    // Numbers from before the overlay was last hidden are stale.
    *s.stats.lock() = Default::default();
    s.stats_on.store(on, Ordering::Relaxed);
    ctx.send_input(&s.id, InputEvent::Stats { on });
}

// ---- Input ----

fn send_key_outs(ctx: &AppContext, id: &str, outs: Vec<KeyOut>) {
    for out in outs {
        let event = match out {
            KeyOut::Down(key_code) => InputEvent::KeyDown { key_code },
            KeyOut::Up(key_code) => InputEvent::KeyUp { key_code },
            KeyOut::Text(text) => InputEvent::Text { text },
            KeyOut::Paste => continue,
        };
        ctx.send_input(id, event);
    }
}

/// Lifts every key still held on host `id` (focus loss, window deactivation, tab switch).
pub fn release_keys(ctx: &AppContext, id: &str) {
    if let Some(s) = ctx.viewers.get(id) {
        let outs = s.keys.lock().release_all();
        send_key_outs(ctx, id, outs);
    }
}

/// Keyboard forwarding to the focused host: physical key down/up, unicode text, and Ctrl+V with the local
/// clipboard (text, or files that are sent and pasted into the remote Explorer folder).
pub fn key_event(ctx: &AppContext, id: &str, text: &str, pressed: bool, repeat: bool, mods: Mods) {
    if !ctx.viewers.is_focused(id) || !ctx.viewers.is_live(id) {
        return;
    }
    let Some(sess) = ctx.viewers.get(id) else { return };
    let outs = {
        let mut keys = sess.keys.lock();
        if pressed { keys.on_press(text, repeat, mods) } else { keys.on_release(text, mods) }
    };
    for out in outs {
        if out != KeyOut::Paste {
            send_key_outs(ctx, id, vec![out]);
            continue;
        }
        let clip = user_clipboard::read();
        let files = clip.files;
        // Files that just came from the host are already there, and files offered to this host in this session
        // are on its clipboard as virtual entries: only the keystroke is forwarded. Anything else (copied
        // before the session or before this host had focus) is sent and pasted, like a drop.
        let seq = core_engine::clipboard::sequence_number();
        let offered = file_transfer::caps_of(id).file_offer && sess.offered_seq.load(Ordering::SeqCst) == seq;
        if !files.is_empty() && !file_transfer::is_received_clipboard(&files) && !offered {
            // A second Ctrl+V while the first copy is still on its way would send and paste the files twice.
            if file_transfer::sender::is_sending(id, &files) {
                ctx.ui.status("Still sending these files; they are pasted when they arrive".into());
            } else {
                file_transfer::sender::send_files(ctx, files, id.to_string(), true);
            }
            continue;
        }
        if let Some(text) = clip.text {
            ctx.send_input(id, InputEvent::Clipboard { text });
        }
        // Ctrl is already held on the host, so V down pastes the text synced just above.
        let down = sess.keys.lock().paste_down();
        send_key_outs(ctx, id, down);
    }
}

/// Mouse input on session `id` with coordinates in 0..=10000. In split view the first click on another tile
/// only focuses it; its button-up is swallowed too.
pub fn mouse(ctx: &AppContext, id: &str, kind: Mouse, x_10000: i32, y_10000: i32) {
    let Some(sess) = ctx.viewers.get(id) else { return };
    if !ctx.viewers.is_focused(id) {
        if let Mouse::Down(_) = kind {
            focus(ctx, id);
            *ctx.viewers.swallowed_click.lock() = Some(id.to_string());
        }
        return;
    }
    if let Mouse::Up(_) = kind {
        let mut swallowed = ctx.viewers.swallowed_click.lock();
        if swallowed.as_deref() == Some(id) {
            *swallowed = None;
            return;
        }
    }
    if !sess.is_connected() {
        return;
    }
    let norm_x = (x_10000 as f64 / 10000.0).clamp(0.0, 1.0);
    let norm_y = (y_10000 as f64 / 10000.0).clamp(0.0, 1.0);
    let event = match kind {
        Mouse::Move => {
            // Coalesced per host by the relay connection.
            let _ = ctx.outbox.send(Outgoing::MouseMove(id.to_string(), norm_x, norm_y));
            return;
        }
        Mouse::Down(button) => InputEvent::MouseDown { norm_x, norm_y, button },
        Mouse::Up(button) => InputEvent::MouseUp { norm_x, norm_y, button },
        Mouse::Wheel(delta_y) => InputEvent::Wheel { norm_x, norm_y, delta_y },
    };
    ctx.send_input(id, event);
}

// ---- Per-session settings ----

/// Toggles this machine's microphone toward host `id` and tells the host.
pub fn toggle_mic(ctx: &AppContext, id: &str) {
    let Some(s) = ctx.viewers.get(id) else { return };
    let muted = !s.mic_muted.load(Ordering::SeqCst);
    s.mic_muted.store(muted, Ordering::SeqCst);
    if ctx.viewers.is_focused(id) {
        ctx.voice.is_mic_muted.store(muted, Ordering::SeqCst);
    }
    ctx.ui.session_changed(id.to_string(), SessionChange::MicMuted(muted));
    ctx.send_input(id, InputEvent::SetMic { muted });
}

/// Privacy mode on host `id`: 0 normal, 1 frozen screen, 2 maintenance screen, 3 black screen.
pub fn set_privacy(ctx: &AppContext, id: &str, mode: i32) {
    let Some(s) = ctx.viewers.get(id) else { return };
    let mode = mode.clamp(0, 3);
    s.privacy_mode.store(mode, Ordering::SeqCst);
    ctx.ui.session_changed(id.to_string(), SessionChange::Privacy(mode));
    ctx.send_input(id, InputEvent::SetPrivacy { mode });
}

/// Blocks or unblocks the physical keyboard and mouse of host `id`.
pub fn toggle_block_input(ctx: &AppContext, id: &str) {
    let Some(s) = ctx.viewers.get(id) else { return };
    let blocked = !s.block_input.load(Ordering::SeqCst);
    s.block_input.store(blocked, Ordering::SeqCst);
    ctx.ui.session_changed(id.to_string(), SessionChange::BlockInput(blocked));
    ctx.send_input(id, InputEvent::SetBlockInput { blocked });
}

/// System shortcut from the toolbar menu of viewers without a system-key hook ("win", "alt-tab", "taskmgr",
/// "explorer", "desktop").
pub fn shortcut(ctx: &AppContext, id: &str, name: &str) {
    ctx.send_input(id, InputEvent::Shortcut { name: name.into() });
}

/// Voice quality toward host `id`: 0 = 16 kHz, 1 = 24 kHz, 2 = 48 kHz.
pub fn set_audio_quality(ctx: &AppContext, id: &str, quality: i32) {
    let Some(s) = ctx.viewers.get(id) else { return };
    let q = quality.clamp(0, 2) as usize;
    s.audio_quality.store(q, Ordering::SeqCst);
    if ctx.viewers.is_focused(id) {
        ctx.voice.audio_quality.store(q, Ordering::SeqCst);
    }
    ctx.ui.session_changed(id.to_string(), SessionChange::AudioQuality(q as i32));
    ctx.send_input(id, InputEvent::SetAudioQuality { quality: q as u64 });
}

/// Stream resolution for host `id` (also the default for sessions opened later).
pub fn set_resolution(ctx: &AppContext, id: &str, mode: i32) {
    let mode = resolution::sanitize_mode(mode);
    ctx.viewers.set_default_resolution(mode);
    let Some(s) = ctx.viewers.get(id) else { return };
    s.res_mode.store(mode, Ordering::SeqCst);
    ctx.ui.session_changed(id.to_string(), SessionChange::Resolution(mode as i32));
    if s.is_connected() {
        ctx.send_input(id, resolution_event(&s));
    }
}

/// The `set-resolution` event for a session's mode and canvas; records the box it reports.
pub fn resolution_event(s: &Session) -> InputEvent {
    let packed = s.viewport.load(Ordering::SeqCst);
    let request = resolution::request(s.res_mode.load(Ordering::SeqCst), resolution::unpack_box(packed));
    s.last_sent_box.store(if request.auto_box.is_some() { packed } else { 0 }, Ordering::SeqCst);
    let (auto, width, height) = match request.auto_box {
        Some((w, h)) => (Some(true), Some(w as u64), Some(h as u64)),
        None => (None, None, None),
    };
    InputEvent::SetResolution { mode: request.mode as u64, auto, width, height }
}

/// Records the canvas size (physical px) of session `id`. Call on every resize.
pub fn viewport_changed(ctx: &AppContext, id: &str, w: i32, h: i32) {
    let packed = resolution::pack_box(w, h);
    ctx.viewers.set_default_viewport(packed);
    if let Some(s) = ctx.viewers.get(id) {
        s.viewport.store(packed, Ordering::SeqCst);
    }
}

/// Automatic resolution: reports the canvas size of `id` to its host when it changed noticeably. The UI calls
/// this once resizing has stopped, because every change recreates the host's capturer and costs a key frame.
pub fn report_viewport(ctx: &AppContext, id: &str) {
    let Some(s) = ctx.viewers.get(id) else { return };
    if s.res_mode.load(Ordering::SeqCst) != resolution::AUTO || !s.is_connected() {
        return;
    }
    let packed = s.viewport.load(Ordering::SeqCst);
    let size = resolution::unpack_box(packed);
    if size.0 <= 0
        || size.1 <= 0
        || !resolution::box_changed_enough(resolution::unpack_box(s.last_sent_box.load(Ordering::SeqCst)), size)
    {
        return;
    }
    ctx.send_input(id, resolution_event(&s));
}

/// Shows monitor `index` of host `id`. Hosts that never announced their monitors only know how to cycle.
pub fn select_monitor(ctx: &AppContext, id: &str, index: i32) {
    let Some(s) = ctx.viewers.get(id) else { return };
    if s.has_monitors.load(Ordering::SeqCst) {
        let index = index.max(0);
        ctx.send_input(id, InputEvent::SelectMonitor { index: index as u64 });
        ctx.ui.session_changed(id.to_string(), SessionChange::ActiveMonitor(index));
    } else {
        ctx.send_input(id, InputEvent::SwitchMonitor);
    }
    ctx.ui.status("Switching monitor...".into());
}

/// Lets the user pick files and sends them to `target` (a viewed host, or the hosted viewer).
pub fn pick_and_send_files(ctx: &Ctx, target: &str) {
    if target.is_empty() {
        return;
    }
    let (ctx, target) = (Arc::clone(ctx), target.to_string());
    crate::infra::thread::spawn_named("files-pick", move || {
        let paths = window::pick_files_dialog();
        if !paths.is_empty() {
            file_transfer::sender::send_files(&ctx, paths, target, true);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::keyboard::{VK_CONTROL, VK_SHIFT};
    use crate::service::viewer::registry::Registry;

    #[test]
    fn held_keys_are_per_host() {
        let r = Registry::new();
        r.set_multi(true);
        let Open::Opened(a) = r.open("111111111", 1, 0, 2) else { panic!() };
        let Open::Opened(b) = r.open("222222222", 1, 0, 2) else { panic!() };
        let ctrl = Mods { ctrl: true, alt: false, shift: false };
        let shift = Mods { ctrl: false, alt: false, shift: true };
        assert_eq!(a.keys.lock().on_press("\u{0011}", false, ctrl), vec![KeyOut::Down(VK_CONTROL)]);
        assert_eq!(b.keys.lock().on_press("\u{0010}", false, shift), vec![KeyOut::Down(VK_SHIFT)]);
        assert_eq!(a.keys.lock().release_all(), vec![KeyOut::Up(VK_CONTROL)]);
        assert!(a.keys.lock().release_all().is_empty(), "nothing stays stuck");
        assert_eq!(b.keys.lock().release_all(), vec![KeyOut::Up(VK_SHIFT)]);
    }

    #[test]
    fn resolution_event_is_per_session() {
        let r = Registry::new();
        r.set_multi(true);
        let Open::Opened(a) = r.open("111111111", resolution::AUTO, resolution::pack_box(1600, 900), 2) else {
            panic!()
        };
        let Open::Opened(b) = r.open("222222222", 720, resolution::pack_box(800, 450), 2) else { panic!() };
        assert_eq!(
            resolution_event(&a),
            InputEvent::SetResolution { mode: 1080, auto: Some(true), width: Some(1600), height: Some(900) }
        );
        assert_eq!(resolution::unpack_box(a.last_sent_box.load(Ordering::SeqCst)), (1600, 900));
        assert_eq!(
            resolution_event(&b),
            InputEvent::SetResolution { mode: 720, auto: None, width: None, height: None }
        );
        assert_eq!(b.last_sent_box.load(Ordering::SeqCst), 0);
    }
}
