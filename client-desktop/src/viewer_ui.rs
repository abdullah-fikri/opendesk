//! UI-thread side of viewer sessions: the `sessions` model of the multi-session UI contract
//! (docs/ui-contract.md), focus handling and the per-session actions behind its callbacks.
//! The old single-session properties (`remote_frame`, `is_mic_muted`, ...) mirror the focused session, so the
//! old viewer canvas keeps working unchanged.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use core_engine::input::InputInjector;
use serde_json::{json, Value};
use slint::{ComponentHandle, Image, Model, ModelRc, VecModel};

use crate::audio::VoiceEngine;
use crate::config::format_desk_id;
use crate::keymap::{self, KeyForwarder};
use crate::protocol::OutgoingMsg;
use crate::viewer::{self, FrameOut, Monitor, Session, VIEWERS};
use crate::{file_transfer, privacy, session};
use crate::{MainWindow, MonitorItem, SessionView};

/// Handles shared by everything that changes viewer sessions (UI thread and network thread).
#[derive(Clone)]
pub struct Ctx {
    pub tx: Sender<OutgoingMsg>,
    /// Target of keyboard, clipboard and file actions: the focused connected viewer session, or the viewer
    /// this machine hosts (host role, unchanged).
    pub active_partner: Arc<Mutex<String>>,
    pub is_host: Arc<AtomicBool>,
    pub voice: Arc<VoiceEngine>,
    pub injector: Arc<InputInjector>,
    pub last_clip_seq: Arc<AtomicU32>,
    pub win_title: String,
}

pub const CONNECTING: &str = "Menghubungkan...";

thread_local! {
    static MODEL: RefCell<Option<Rc<VecModel<SessionView>>>> = const { RefCell::new(None) };
    /// Keys held on each host, so focus loss or window deactivation can lift them.
    static KEYS: RefCell<HashMap<String, KeyForwarder>> = RefCell::new(HashMap::new());
    /// A click that only focused its tile; its button-up is not sent either.
    static SWALLOW_UP: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Debounce timers for automatic resolution, per session.
    static RES_TIMERS: RefCell<HashMap<String, Rc<slint::Timer>>> = RefCell::new(HashMap::new());
}

pub fn init(win: &MainWindow) {
    let model = Rc::new(VecModel::<SessionView>::default());
    win.set_sessions(ModelRc::from(Rc::clone(&model)));
    MODEL.with(|m| *m.borrow_mut() = Some(model));
}

fn with_model<R>(f: impl FnOnce(&VecModel<SessionView>) -> R) -> Option<R> {
    MODEL.with(|m| m.borrow().as_ref().map(|model| f(model)))
}

/// Updates one row in place (no model rebuild). False if the session has no row.
pub fn row_update(id: &str, f: impl FnOnce(&mut SessionView)) -> bool {
    with_model(|m| {
        for i in 0..m.row_count() {
            if let Some(mut row) = m.row_data(i) {
                if row.id == id {
                    f(&mut row);
                    m.set_row_data(i, row);
                    return true;
                }
            }
        }
        false
    })
    .unwrap_or(false)
}

fn row_get(id: &str) -> Option<SessionView> {
    with_model(|m| m.iter().find(|r| r.id == id)).flatten()
}

fn add_row(row: SessionView) {
    with_model(|m| m.push(row));
}

fn remove_row(id: &str) {
    with_model(|m| {
        if let Some(i) = m.iter().position(|r| r.id == id) {
            m.remove(i);
        }
    });
}

fn new_row(sess: &Session, title: String, subtitle: String) -> SessionView {
    SessionView {
        id: sess.id.clone().into(),
        title: title.into(),
        subtitle: subtitle.into(),
        connecting: true,
        cursor: 1,
        path: "Relay".into(),
        status: CONNECTING.into(),
        resolution_mode: sess.res_mode.load(Ordering::SeqCst) as i32,
        audio_quality: sess.audio_quality.load(Ordering::SeqCst) as i32,
        ..Default::default()
    }
}

/// Adds the tab for a new session unless it already has one.
pub fn ensure_row(sess: &Session, title: String, subtitle: String) {
    if row_get(&sess.id).is_none() {
        add_row(new_row(sess, title, subtitle));
    }
}

fn send_input(ctx: &Ctx, id: &str, event: Value) {
    if !id.is_empty() {
        let _ = ctx.tx.send(OutgoingMsg::Json(json!({ "type": "relay-input", "targetClean": id, "event": event })));
    }
}

/// The old single-session properties follow this session (the focused one, or a non-viewer partner).
fn mirrors(id: &str) -> bool {
    VIEWERS.get(id).is_none() || VIEWERS.is_focused(id)
}

// ---- Focus ----

/// Applies the registry's focus outside the UI: input target, keyboard hook, voice and file caps.
/// Safe on any thread. The host role (this PC being controlled) is never touched.
pub fn apply_focus_core(ctx: &Ctx) {
    let focused = VIEWERS.focused().filter(|s| s.is_connected());
    if !ctx.is_host.load(Ordering::SeqCst) {
        *ctx.active_partner.lock().unwrap() = focused.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        match &focused {
            Some(s) => privacy::set_viewer_hook_state(true, &s.id, &ctx.win_title),
            None => privacy::set_viewer_hook_state(false, "", ""),
        }
    }
    if let Some(s) = focused {
        ctx.voice.is_mic_muted.store(s.mic_muted.load(Ordering::SeqCst), Ordering::SeqCst);
        ctx.voice.audio_quality.store(s.audio_quality.load(Ordering::SeqCst), Ordering::SeqCst);
        // Code paths that only know "the partner" (clipboard watcher, Ctrl+V) read these globals.
        file_transfer::PEER_FILE_ACK.store(file_transfer::peer_ack(&s.id), Ordering::SeqCst);
        file_transfer::PEER_FILE_OFFER.store(file_transfer::peer_offer(&s.id), Ordering::SeqCst);
        file_transfer::PEER_FILE_FOLDER.store(file_transfer::peer_folder(&s.id), Ordering::SeqCst);
    }
}

/// UI part of a focus change. `prev` is the focus before the change: its held keys are released and the
/// newly focused host gets the viewer's clipboard (clipboard follows focus).
pub fn sync_focus(win: &MainWindow, ctx: &Ctx, prev: &str) {
    apply_focus_core(ctx);
    let fid = VIEWERS.focused_id();
    if prev != fid {
        if !prev.is_empty() {
            release_keys(ctx, prev);
        }
        if let Some(s) = VIEWERS.get(&fid).filter(|s| s.is_connected()) {
            push_clipboard(win, ctx, &s);
        }
    }
    win.set_focused_session(fid.clone().into());
    if let Some(r) = row_get(&fid) {
        win.set_active_partner_id(format_desk_id(&fid).into());
        win.set_has_remote_frame(r.has_frame);
        if r.has_frame {
            win.set_remote_frame(r.frame.clone());
        }
        win.set_remote_cursor(r.cursor);
        win.set_is_mic_muted(r.mic_muted);
        win.set_privacy_mode(r.privacy_mode);
        win.set_block_remote_input(r.block_input);
        win.set_resolution_mode(r.resolution_mode);
        win.set_audio_quality(r.audio_quality);
    }
}

fn push_clipboard(win: &MainWindow, ctx: &Ctx, s: &Arc<Session>) {
    let cur = ctx.injector.get_clipboard_sequence_number();
    if !viewer::clipboard_push_needed(cur, s.clip_seq.load(Ordering::SeqCst)) {
        return;
    }
    s.clip_seq.store(cur, Ordering::SeqCst);
    // The watcher must not send the same copy again (it compares against this).
    ctx.last_clip_seq.store(cur, Ordering::SeqCst);
    if file_transfer::peer_offer(&s.id) {
        let files = ctx.injector.get_clipboard_files();
        if !files.is_empty() {
            // Files that came from a host are not re-offered (provenance, as in the clipboard watcher).
            if !file_transfer::is_received_clipboard(&files) {
                file_transfer::offer_files(files, s.id.clone(), ctx.tx.clone(), Some(win.as_weak()));
            }
            return;
        }
    }
    if let Some(text) = ctx.injector.get_clipboard_text().filter(|t| !t.is_empty()) {
        let _ = ctx.tx.send(OutgoingMsg::Json(json!({ "type": "relay-clipboard", "targetClean": s.id, "text": text })));
    }
}

pub fn focus(win: &MainWindow, ctx: &Ctx, id: &str) {
    match VIEWERS.set_focus(id) {
        Some(prev) => sync_focus(win, ctx, &prev),
        None => win.set_focused_session(VIEWERS.focused_id().into()),
    }
}

pub fn set_layout(win: &MainWindow, layout: i32) {
    // Sessions that become visible ask their host for a key frame from their decoder thread.
    VIEWERS.set_layout(layout);
    win.set_view_layout(VIEWERS.layout());
}

// ---- Opening and closing ----

/// `request_connect`: opens a new tab (pending until `session-connected`) or focuses an open one.
pub fn request_connect(win: &MainWindow, ctx: &Ctx, clean: &str, title: String, subtitle: String) {
    if clean.is_empty() {
        return;
    }
    let res_mode = session::VIEWER_RES_MODE.load(Ordering::SeqCst);
    let viewport = session::VIEWPORT_PX.load(Ordering::SeqCst);
    let quality = ctx.voice.audio_quality.load(Ordering::SeqCst);
    let mut opened = VIEWERS.open(clean, res_mode, viewport, quality);
    if matches!(opened, viewer::Open::Full) && !VIEWERS.multi() {
        // Old relay: one session. A connect still waiting (e.g. Android approval) is replaced by the new
        // one; a running session is never ended implicitly.
        if let Some(pending) = VIEWERS.all().into_iter().find(|s| !s.is_connected()) {
            close(win, ctx, &pending.id, None);
            opened = VIEWERS.open(clean, res_mode, viewport, quality);
        }
    }
    match opened {
        viewer::Open::Exists(_) => {
            focus(win, ctx, clean);
            win.set_show_new_session(false);
        }
        viewer::Open::Full => {
            let msg = if VIEWERS.multi() { viewer::MSG_FULL_MULTI } else { viewer::MSG_FULL_LEGACY };
            win.set_status_text(msg.into());
        }
        viewer::Open::Opened(sess) => {
            ensure_row(&sess, title, subtitle);
            focus(win, ctx, clean);
            win.set_show_new_session(false);
            let _ = ctx.tx.send(OutgoingMsg::Json(json!({ "type": "connect-request", "targetId": clean })));
            win.set_status_text(format!("Menghubungkan {}...", format_desk_id(clean)).into());
        }
    }
}

/// Non-UI part of ending a session: stops its decoder and drops its offer and caps. Any thread.
pub fn end_core(id: &str) -> bool {
    if VIEWERS.remove(id).is_none() {
        return false;
    }
    crate::virtual_clip::clear_offer(id);
    file_transfer::remove_session_caps(id);
    true
}

/// UI part of ending a session. `prev` is the focus before it ended; `status` replaces the status line.
pub fn after_end(win: &MainWindow, ctx: &Ctx, id: &str, prev: &str, status: Option<String>) {
    KEYS.with(|k| k.borrow_mut().remove(id));
    RES_TIMERS.with(|t| t.borrow_mut().remove(id));
    SWALLOW_UP.with(|s| {
        if s.borrow().as_deref() == Some(id) {
            *s.borrow_mut() = None;
        }
    });
    remove_row(id);
    sync_focus(win, ctx, prev);
    if VIEWERS.is_empty() && !ctx.is_host.load(Ordering::SeqCst) {
        ctx.voice.is_session_active.store(false, Ordering::SeqCst);
        ctx.voice.is_mic_muted.store(true, Ordering::SeqCst);
        win.set_is_connected(false);
        win.set_is_host(false);
        win.set_is_mic_muted(true);
        win.set_privacy_mode(0);
        win.set_block_remote_input(false);
        win.set_show_session_options(false);
        win.set_has_remote_frame(false);
        win.set_show_new_session(false);
        win.set_status_text(status.unwrap_or_else(|| "Online".into()).into());
    } else if let Some(st) = status {
        win.set_status_text(st.into());
    }
}

/// `close_session`: ends one session (pending or connected) and tells the relay.
pub fn close(win: &MainWindow, ctx: &Ctx, id: &str, status: Option<String>) {
    if VIEWERS.get(id).is_none() {
        return;
    }
    let _ = ctx.tx.send(OutgoingMsg::Json(json!({ "type": "session-disconnect", "targetClean": id })));
    let prev = VIEWERS.focused_id();
    end_core(id);
    after_end(win, ctx, id, &prev, status);
}

/// Ends every viewer session on the UI side (relay connection lost). The caller resets the old properties.
pub fn clear_all_ui() {
    with_model(|m| m.set_vec(Vec::new()));
    KEYS.with(|k| k.borrow_mut().clear());
    RES_TIMERS.with(|t| t.borrow_mut().clear());
    SWALLOW_UP.with(|s| *s.borrow_mut() = None);
}

/// Non-UI part of ending every session. Returns the ended IDs.
pub fn end_all_core() -> Vec<String> {
    let ids: Vec<String> = VIEWERS.clear().into_iter().map(|s| s.id.clone()).collect();
    for id in &ids {
        crate::virtual_clip::clear_offer(id);
        file_transfer::remove_session_caps(id);
    }
    ids
}

// ---- Frames and host events (posted from the network/decoder threads) ----

pub fn show_frame(win: &MainWindow, id: &str, f: FrameOut) {
    let img = Image::from_rgba8(f.buf);
    let path = if f.p2p { "P2P" } else { "Relay" };
    let (w, h) = (f.width as i32, f.height as i32);
    let found = row_update(id, |r| {
        r.frame = img.clone();
        r.has_frame = true;
        r.connecting = false;
        r.fps = f.fps as i32;
        r.stream_width = w;
        r.stream_height = h;
        if r.path.as_str() != path {
            r.path = path.into();
        }
        if r.status.as_str() == CONNECTING {
            r.status = Default::default();
        }
    });
    if !found || !VIEWERS.is_focused(id) {
        return;
    }
    win.set_remote_frame(img);
    win.set_has_remote_frame(true);
    let file_busy = file_transfer::IN_FLIGHT_FILE_CHUNKS.load(Ordering::Relaxed) > 0
        || crate::network::now_epoch_ms().saturating_sub(file_transfer::LAST_FILE_RECV_MS.load(Ordering::Relaxed)) < 2000;
    if !win.get_is_host() && win.get_is_connected() && !file_busy {
        win.set_status_text(format!("{}x{} • {} FPS • {}", w, h, f.fps.max(60), path).into());
    }
}

pub fn set_cursor(win: &MainWindow, id: &str, shape: i32) {
    row_update(id, |r| r.cursor = shape);
    if VIEWERS.is_focused(id) {
        win.set_remote_cursor(shape);
    }
}

pub fn set_monitors(id: &str, monitors: Vec<Monitor>, active: i32) {
    let items: Vec<MonitorItem> = monitors
        .into_iter()
        .map(|m| MonitorItem { index: m.index, label: (m.index + 1).to_string().into(), width: m.width, height: m.height, primary: m.primary })
        .collect();
    row_update(id, |r| {
        r.monitors = ModelRc::from(Rc::new(VecModel::from(items)));
        r.active_monitor = active;
    });
}

/// A host muted our mic (`set-mic`); only the local user can enable it again.
pub fn host_muted_mic(win: &MainWindow, id: &str) {
    row_update(id, |r| r.mic_muted = true);
    if VIEWERS.is_focused(id) {
        win.set_is_mic_muted(true);
    }
}

pub fn host_set_audio_quality(win: &MainWindow, id: &str, q: i32) {
    row_update(id, |r| r.audio_quality = q);
    if VIEWERS.is_focused(id) {
        win.set_audio_quality(q);
    }
}

// ---- Input ----

pub fn send_key_outs(tx: &Sender<OutgoingMsg>, clean: &str, outs: Vec<keymap::KeyOut>) {
    for out in outs {
        let event = match out {
            keymap::KeyOut::Down(vk) => json!({ "action": "keydown", "keyCode": vk }),
            keymap::KeyOut::Up(vk) => json!({ "action": "keyup", "keyCode": vk }),
            keymap::KeyOut::Text(text) => json!({ "action": "text", "text": text }),
            keymap::KeyOut::Paste => continue,
        };
        let _ = tx.send(OutgoingMsg::Json(json!({ "type": "relay-input", "targetClean": clean, "event": event })));
    }
}

/// Key-ups for everything still held on host `id` (its forwarder forgets them).
fn take_held(id: &str) -> Vec<keymap::KeyOut> {
    KEYS.with(|k| k.borrow_mut().get_mut(id).map(|f| f.release_all()).unwrap_or_default())
}

/// Runs one key event through host `id`'s own forwarder.
fn forward(id: &str, text: &str, pressed: bool, repeat: bool, mods: keymap::Mods) -> Vec<keymap::KeyOut> {
    KEYS.with(|k| {
        let mut map = k.borrow_mut();
        let fwd = map.entry(id.to_string()).or_default();
        if pressed { fwd.on_press(text, repeat, mods) } else { fwd.on_release(text, mods) }
    })
}

/// Lifts every key still held on host `id`.
pub fn release_keys(ctx: &Ctx, id: &str) {
    let outs = take_held(id);
    if !id.is_empty() {
        send_key_outs(&ctx.tx, id, outs);
    }
}

/// Keyboard forwarding to host `id`: physical key down/up, unicode text, Ctrl+V clipboard/files.
#[allow(clippy::too_many_arguments)]
pub fn key_event(win: &MainWindow, ctx: &Ctx, id: &str, text: &str, pressed: bool, repeat: bool, mods: keymap::Mods) {
    if id.is_empty() {
        return;
    }
    for out in forward(id, text, pressed, repeat, mods) {
        if out != keymap::KeyOut::Paste {
            send_key_outs(&ctx.tx, id, vec![out]);
            continue;
        }
        // Ctrl+V: if the local clipboard has files, transfer & auto-paste them into the open remote folder.
        let clip_files = ctx.injector.get_clipboard_files();
        // Files that just came from the host are already there; only forward the keystroke.
        // Offer-capable hosts already have the copied files as virtual clipboard entries: just paste.
        if !clip_files.is_empty() && !file_transfer::is_received_clipboard(&clip_files) && !file_transfer::peer_offer(id) {
            file_transfer::send_files_async(clip_files, id.to_string(), ctx.tx.clone(), Some(win.as_weak()), true);
            continue;
        }
        if let Some(clip_text) = ctx.injector.get_clipboard_text() {
            if !clip_text.is_empty() {
                send_input(ctx, id, json!({ "action": "clipboard", "text": clip_text }));
            }
        }
        // Ctrl is already held on the host, so V down pastes the text synced just above.
        let down = KEYS.with(|k| k.borrow_mut().get_mut(id).map(|f| f.paste_down()).unwrap_or_default());
        send_key_outs(&ctx.tx, id, down);
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mouse {
    Move,
    Down(i32),
    Up(i32),
    Wheel(i32),
}

/// Mouse input on session `id` (coordinates 0..10000). Only the focused session gets input: in split view
/// the first click on another tile only focuses it.
pub fn mouse(win: &MainWindow, ctx: &Ctx, id: &str, kind: Mouse, nx_10000: i32, ny_10000: i32) {
    let Some(sess) = VIEWERS.get(id) else { return };
    if !VIEWERS.is_focused(id) {
        if let Mouse::Down(_) = kind {
            focus(win, ctx, id);
            SWALLOW_UP.with(|s| *s.borrow_mut() = Some(id.to_string()));
        }
        return;
    }
    if let Mouse::Up(_) = kind {
        let swallowed = SWALLOW_UP.with(|s| {
            let hit = s.borrow().as_deref() == Some(id);
            if hit {
                *s.borrow_mut() = None;
            }
            hit
        });
        if swallowed {
            return;
        }
    }
    if !sess.is_connected() {
        return;
    }
    let nx = (nx_10000 as f64 / 10000.0).clamp(0.0, 1.0);
    let ny = (ny_10000 as f64 / 10000.0).clamp(0.0, 1.0);
    let event = match kind {
        Mouse::Move => {
            let _ = ctx.tx.send(OutgoingMsg::MouseMove(id.to_string(), nx, ny));
            return;
        }
        Mouse::Down(btn) => json!({ "action": "mousedown", "normX": nx, "normY": ny, "button": btn }),
        Mouse::Up(btn) => json!({ "action": "mouseup", "normX": nx, "normY": ny, "button": btn }),
        Mouse::Wheel(dy) => json!({ "action": "wheel", "normX": nx, "normY": ny, "deltaY": dy }),
    };
    send_input(ctx, id, event);
}

// ---- Per-session settings (the old callbacks call these with the focused/active partner) ----

pub fn toggle_mic(win: &MainWindow, ctx: &Ctx, id: &str) {
    let next = match VIEWERS.get(id) {
        Some(s) => {
            let next = !s.mic_muted.load(Ordering::SeqCst);
            s.mic_muted.store(next, Ordering::SeqCst);
            if VIEWERS.is_focused(id) {
                ctx.voice.is_mic_muted.store(next, Ordering::SeqCst);
            }
            row_update(id, |r| r.mic_muted = next);
            next
        }
        None => {
            let next = !ctx.voice.is_mic_muted.load(Ordering::SeqCst);
            ctx.voice.is_mic_muted.store(next, Ordering::SeqCst);
            next
        }
    };
    if mirrors(id) {
        win.set_is_mic_muted(next);
    }
    send_input(ctx, id, json!({ "action": "set-mic", "muted": next }));
}

pub fn set_privacy(win: &MainWindow, ctx: &Ctx, id: &str, mode: i32) {
    row_update(id, |r| r.privacy_mode = mode);
    if mirrors(id) {
        win.set_privacy_mode(mode);
    }
    send_input(ctx, id, json!({ "action": "set-privacy", "mode": mode }));
}

pub fn toggle_block_input(win: &MainWindow, ctx: &Ctx, id: &str) {
    let current = row_get(id).map(|r| r.block_input).unwrap_or_else(|| win.get_block_remote_input());
    let next = !current;
    row_update(id, |r| r.block_input = next);
    if mirrors(id) {
        win.set_block_remote_input(next);
    }
    send_input(ctx, id, json!({ "action": "set-block-input", "blocked": next }));
}

pub fn shortcut(win: &MainWindow, ctx: &Ctx, id: &str, name: &str) {
    // Contract names -> the names hosts understand ("taskmgr", "win", "explorer", "desktop", "alt-tab").
    let name = match name {
        "task-manager" => "taskmgr",
        other => other,
    };
    if name == "ctrl-alt-del" {
        // The secure attention sequence cannot be injected by a user-mode app; hosts ignore the name anyway.
        win.set_status_text("Ctrl+Alt+Del belum didukung.".into());
        return;
    }
    send_input(ctx, id, json!({ "action": "shortcut", "name": name }));
}

pub fn set_audio_quality(win: &MainWindow, ctx: &Ctx, id: &str, quality: i32) {
    let q = (quality.max(0) as usize).min(2);
    match VIEWERS.get(id) {
        Some(s) => {
            s.audio_quality.store(q, Ordering::SeqCst);
            if VIEWERS.is_focused(id) {
                ctx.voice.audio_quality.store(q, Ordering::SeqCst);
            }
            row_update(id, |r| r.audio_quality = q as i32);
        }
        None => ctx.voice.audio_quality.store(q, Ordering::SeqCst),
    }
    if mirrors(id) {
        win.set_audio_quality(q as i32);
    }
    send_input(ctx, id, json!({ "action": "set-audio-quality", "quality": q }));
}

pub fn sanitize_res_mode(mode: i32) -> usize {
    match mode {
        0 => 0,
        1 => session::RES_AUTO,
        1080 => 1080,
        720 => 720,
        480 => 480,
        _ => 1080,
    }
}

pub fn set_resolution(win: &MainWindow, ctx: &Ctx, id: &str, mode: i32) {
    let safe = sanitize_res_mode(mode);
    // Also the default for sessions opened later, as before.
    session::VIEWER_RES_MODE.store(safe, Ordering::SeqCst);
    if mirrors(id) {
        win.set_resolution_mode(safe as i32);
    }
    let Some(s) = VIEWERS.get(id) else {
        // Not a viewer session (host role): same message as before.
        let packed = session::VIEWPORT_PX.load(Ordering::SeqCst);
        send_input(ctx, id, session::resolution_event(safe, session::unpack_box(packed)));
        return;
    };
    s.res_mode.store(safe, Ordering::SeqCst);
    row_update(id, |r| r.resolution_mode = safe as i32);
    if s.is_connected() {
        send_input(ctx, id, initial_resolution_event(&s));
    }
}

/// The `set-resolution` event for a session's mode and canvas; records the box it reports.
pub fn initial_resolution_event(s: &Session) -> Value {
    let mut packed = s.viewport.load(Ordering::SeqCst);
    if packed == 0 {
        packed = session::VIEWPORT_PX.load(Ordering::SeqCst);
    }
    let event = session::resolution_event(s.res_mode.load(Ordering::SeqCst), session::unpack_box(packed));
    s.last_sent_box.store(if event.get("auto").is_some() { packed } else { 0 }, Ordering::SeqCst);
    event
}

/// Automatic resolution: report the (debounced) canvas size when it changed noticeably.
fn send_auto_resolution(ctx: &Ctx, s: &Session) {
    if s.res_mode.load(Ordering::SeqCst) != session::RES_AUTO || !s.is_connected() {
        return;
    }
    let packed = s.viewport.load(Ordering::SeqCst);
    let size = session::unpack_box(packed);
    if size.0 <= 0 || size.1 <= 0 {
        return;
    }
    if !session::box_changed_enough(session::unpack_box(s.last_sent_box.load(Ordering::SeqCst)), size) {
        return;
    }
    s.last_sent_box.store(packed, Ordering::SeqCst);
    send_input(ctx, &s.id, session::resolution_event(session::RES_AUTO, size));
}

/// Canvas size of session `id` (physical px). Sent 300 ms after resizing stops: every change recreates the
/// host capturer/encoder and costs a key frame. Split tiles thereby get a stream sized to the tile.
pub fn viewport_resized(ctx: &Ctx, id: &str, w: i32, h: i32) {
    session::VIEWPORT_PX.store(session::pack_box(w, h), Ordering::SeqCst);
    let Some(s) = VIEWERS.get(id) else { return };
    s.viewport.store(session::pack_box(w, h), Ordering::SeqCst);
    let timer = RES_TIMERS.with(|t| Rc::clone(t.borrow_mut().entry(id.to_string()).or_insert_with(|| Rc::new(slint::Timer::default()))));
    let ctx = ctx.clone();
    timer.start(slint::TimerMode::SingleShot, Duration::from_millis(300), move || {
        send_auto_resolution(&ctx, &s);
    });
}

pub fn select_monitor(win: &MainWindow, ctx: &Ctx, id: &str, index: i32) {
    let announced = VIEWERS.get(id).is_some_and(|s| s.has_monitors.load(Ordering::SeqCst));
    if announced {
        send_input(ctx, id, json!({ "action": "select-monitor", "index": index.max(0) }));
        row_update(id, |r| r.active_monitor = index.max(0));
    } else {
        // Hosts that never announced their monitors only know how to cycle.
        send_input(ctx, id, json!({ "action": "switch-monitor" }));
    }
    if !id.is_empty() {
        win.set_status_text("Mengganti layar monitor...".into());
    }
}

pub fn send_files(win: &MainWindow, ctx: &Ctx, id: &str) {
    if id.is_empty() {
        return;
    }
    let (tx, weak, id) = (ctx.tx.clone(), win.as_weak(), id.to_string());
    thread::spawn(move || {
        let paths = crate::dragdrop::pick_files_dialog();
        if !paths.is_empty() {
            file_transfer::send_files_async(paths, id, tx, Some(weak), true);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{KeyOut, Mods, VK_CONTROL, VK_SHIFT};

    #[test]
    fn held_keys_are_per_host_and_released_on_focus_loss() {
        let ctrl = Mods { ctrl: true, alt: false, shift: false };
        let shift = Mods { ctrl: false, alt: false, shift: true };
        // Ctrl held on A, Shift held on B: each host only ever gets its own key-ups.
        assert_eq!(forward("111111111", "\u{0011}", true, false, ctrl), vec![KeyOut::Down(VK_CONTROL)]);
        assert_eq!(forward("222222222", "\u{0010}", true, false, shift), vec![KeyOut::Down(VK_SHIFT)]);
        // Focus moves A -> B: sync_focus releases what A still holds.
        assert_eq!(take_held("111111111"), vec![KeyOut::Up(VK_CONTROL)]);
        assert!(take_held("111111111").is_empty(), "nothing stays stuck");
        assert_eq!(take_held("222222222"), vec![KeyOut::Up(VK_SHIFT)]);
        assert!(take_held("333333333").is_empty(), "unknown host: nothing to send");
    }

    #[test]
    fn resolution_modes_are_sanitized_like_before() {
        assert_eq!(sanitize_res_mode(0), 0);
        assert_eq!(sanitize_res_mode(1), session::RES_AUTO);
        assert_eq!(sanitize_res_mode(720), 720);
        assert_eq!(sanitize_res_mode(12345), 1080);
    }

    #[test]
    fn auto_resolution_is_per_session() {
        let r = viewer::Registry::new();
        r.set_multi(true);
        let viewer::Open::Opened(a) = r.open("111111111", session::RES_AUTO, session::pack_box(1600, 900), 2) else { panic!() };
        let viewer::Open::Opened(b) = r.open("222222222", 720, session::pack_box(800, 450), 2) else { panic!() };
        let ev = initial_resolution_event(&a);
        assert_eq!((ev["auto"].as_bool(), ev["width"].as_i64(), ev["height"].as_i64()), (Some(true), Some(1600), Some(900)));
        assert_eq!(session::unpack_box(a.last_sent_box.load(Ordering::SeqCst)), (1600, 900));
        let ev = initial_resolution_event(&b);
        assert_eq!(ev["mode"], 720);
        assert!(ev.get("auto").is_none());
        assert_eq!(b.last_sent_box.load(Ordering::SeqCst), 0);
    }
}
