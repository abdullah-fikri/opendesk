//! Viewer sessions: one per host this machine controls, keyed by the host's clean desk ID
//! (see `docs/features/viewer-sessions.md`). Against a relay without the `multi-session` capability there is at
//! most one session and every message keeps its single-session shape.
//!
//! Threads: the registry is shared by the UI thread and the relay connection. Each connected session owns one
//! decoder thread, so nothing runs while no session is open.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use parking_lot::Mutex;

use crate::domain::keyboard::KeyForwarder;
use crate::infra::power;
use crate::service::viewer::decoder::VideoInbox;

/// Sessions a multi-session viewer may hold (the relay enforces the same limit).
pub const MAX_SESSIONS: usize = 4;

/// State of one viewer session, shared by the UI, relay and decoder threads.
pub struct Session {
    pub id: String,
    /// False while the connect is pending (Android approval); true after `session-connected`.
    pub connected: AtomicBool,
    pub inbox: Arc<VideoInbox>,
    /// The decoder thread runs while true.
    pub alive: AtomicBool,
    pub(super) decoder_started: AtomicBool,
    /// Shown on screen (focused tab, or any tile in split view). Hidden sessions are not decoded.
    pub visible: AtomicBool,
    /// Focused: its audio plays (shared with the P2P sink).
    pub audio_on: Arc<AtomicBool>,
    pub p2p_active: AtomicBool,
    pub res_mode: AtomicUsize,
    /// Canvas size in physical pixels (`resolution::pack_box`), 0 = unknown.
    pub viewport: AtomicU64,
    /// Box last sent to the host in automatic mode, 0 = none.
    pub last_sent_box: AtomicU64,
    pub mic_muted: AtomicBool,
    pub audio_quality: AtomicUsize,
    pub privacy_mode: AtomicI32,
    pub block_input: AtomicBool,
    /// Clipboard sequence number this host last saw (see `clipboard_push_needed`).
    pub clip_seq: AtomicU32,
    /// Clipboard sequence number whose files were offered to this host; Ctrl+V of any other copy sends them.
    pub offered_seq: AtomicU32,
    /// The host announced its monitors; otherwise monitor changes use `switch-monitor`.
    pub has_monitors: AtomicBool,
    /// Keys this viewer holds down on the host.
    pub keys: Mutex<KeyForwarder>,
    /// Full speed (no EcoQoS) from opening the session until it is dropped.
    _awake: power::Awake,
}

impl Session {
    fn new(id: &str, res_mode: usize, viewport: u64, audio_quality: usize) -> Self {
        Session {
            id: id.to_string(),
            connected: AtomicBool::new(false),
            inbox: Arc::new(VideoInbox::default()),
            alive: AtomicBool::new(true),
            decoder_started: AtomicBool::new(false),
            visible: AtomicBool::new(false),
            audio_on: Arc::new(AtomicBool::new(false)),
            p2p_active: AtomicBool::new(false),
            res_mode: AtomicUsize::new(res_mode),
            viewport: AtomicU64::new(viewport),
            last_sent_box: AtomicU64::new(0),
            mic_muted: AtomicBool::new(false),
            audio_quality: AtomicUsize::new(audio_quality),
            privacy_mode: AtomicI32::new(0),
            block_input: AtomicBool::new(false),
            clip_seq: AtomicU32::new(0),
            offered_seq: AtomicU32::new(0),
            has_monitors: AtomicBool::new(false),
            keys: Mutex::default(),
            _awake: power::awake(),
        }
    }

    /// True after the relay connected the session.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

/// Clipboard follows focus: push the viewer's clipboard to a newly focused host only if it changed since that
/// host last saw it (pushed, synced from it, or present when its session started).
pub fn clipboard_push_needed(current_seq: u32, session_seq: u32) -> bool {
    current_seq != 0 && current_seq != session_seq
}

/// Result of [`Registry::open`].
pub enum Open {
    Opened(Arc<Session>),
    Exists(Arc<Session>),
    Full,
}

#[derive(Default)]
struct Inner {
    sessions: Vec<Arc<Session>>,
    focused: String,
    layout: i32,
}

impl Inner {
    /// Visibility and audio follow focus and layout. Returns sessions that just became visible.
    fn refresh(&self) -> Vec<Arc<Session>> {
        let mut shown = Vec::new();
        for s in &self.sessions {
            let focused = s.id == self.focused;
            let visible = focused || self.layout == 1;
            if visible && !s.visible.swap(true, Ordering::SeqCst) {
                s.inbox.wake(); // the decoder asks for a key frame without waiting out its idle timeout
                shown.push(Arc::clone(s));
            } else if !visible {
                s.visible.store(false, Ordering::SeqCst);
            }
            s.audio_on.store(focused, Ordering::SeqCst);
        }
        shown
    }
}

/// All viewer sessions, in tab order.
pub struct Registry {
    multi: AtomicBool,
    inner: Mutex<Inner>,
    /// A click that only focused its tile; its button-up is not sent either.
    pub(super) swallowed_click: Mutex<Option<String>>,
    /// Resolution mode for new sessions: the last one the user picked.
    default_resolution: AtomicUsize,
    /// Last canvas size seen (`resolution::pack_box`), so a new session can ask for the right size at once.
    default_viewport: AtomicU64,
}

impl Registry {
    /// An empty registry in single-session mode.
    pub fn new() -> Self {
        Registry {
            multi: AtomicBool::new(false),
            inner: Mutex::default(),
            swallowed_click: Mutex::new(None),
            default_resolution: AtomicUsize::new(crate::domain::resolution::AUTO),
            default_viewport: AtomicU64::new(0),
        }
    }

    /// Resolution mode for sessions opened from now on.
    pub fn default_resolution(&self) -> usize {
        self.default_resolution.load(Ordering::SeqCst)
    }

    /// Sets the resolution mode for sessions opened from now on.
    pub fn set_default_resolution(&self, mode: usize) {
        self.default_resolution.store(mode, Ordering::SeqCst);
    }

    /// Last canvas size seen (`resolution::pack_box`).
    pub fn default_viewport(&self) -> u64 {
        self.default_viewport.load(Ordering::SeqCst)
    }

    /// Records the last canvas size seen.
    pub fn set_default_viewport(&self, packed: u64) {
        self.default_viewport.store(packed, Ordering::SeqCst);
    }

    /// The relay advertised `multi-session` on this connection.
    pub fn multi(&self) -> bool {
        self.multi.load(Ordering::SeqCst)
    }

    /// Enables up to four sessions (the relay advertised `multi-session`).
    pub fn set_multi(&self, on: bool) {
        self.multi.store(on, Ordering::SeqCst);
    }

    /// How many sessions may be open.
    pub fn limit(&self) -> usize {
        if self.multi() { MAX_SESSIONS } else { 1 }
    }

    /// True when no session is open.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().sessions.is_empty()
    }

    /// The session with host `id`.
    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        if id.is_empty() {
            return None;
        }
        self.inner.lock().sessions.iter().find(|s| s.id == id).cloned()
    }

    /// Every session, in tab order.
    pub fn all(&self) -> Vec<Arc<Session>> {
        self.inner.lock().sessions.clone()
    }

    /// The focused session.
    pub fn focused(&self) -> Option<Arc<Session>> {
        let inner = self.inner.lock();
        inner.sessions.iter().find(|s| s.id == inner.focused).cloned()
    }

    /// ID of the focused session (empty: none).
    pub fn focused_id(&self) -> String {
        self.inner.lock().focused.clone()
    }

    /// True when `id` is the focused session.
    pub fn is_focused(&self, id: &str) -> bool {
        !id.is_empty() && self.inner.lock().focused == id
    }

    /// True for a session that exists and is connected.
    pub fn is_live(&self, id: &str) -> bool {
        self.get(id).is_some_and(|s| s.is_connected())
    }

    /// The session an untagged message belongs to: the single session of a single-session connection.
    pub fn single(&self) -> Option<Arc<Session>> {
        if self.multi() {
            return None;
        }
        self.inner.lock().sessions.first().cloned()
    }

    /// Adds a (pending) session unless it exists or the limit is reached. Focus is left to the caller.
    pub fn open(&self, id: &str, res_mode: usize, viewport: u64, audio_quality: usize) -> Open {
        let limit = self.limit();
        let mut inner = self.inner.lock();
        if let Some(s) = inner.sessions.iter().find(|s| s.id == id) {
            return Open::Exists(Arc::clone(s));
        }
        if inner.sessions.len() >= limit {
            return Open::Full;
        }
        let s = Arc::new(Session::new(id, res_mode, viewport, audio_quality));
        inner.sessions.push(Arc::clone(&s));
        inner.refresh();
        Open::Opened(s)
    }

    /// Removes a session and stops its decoder. A removed focused session hands focus to its neighbour.
    pub fn remove(&self, id: &str) -> Option<Arc<Session>> {
        let mut inner = self.inner.lock();
        let pos = inner.sessions.iter().position(|s| s.id == id)?;
        let s = inner.sessions.remove(pos);
        s.alive.store(false, Ordering::SeqCst);
        s.visible.store(false, Ordering::SeqCst);
        s.audio_on.store(false, Ordering::SeqCst);
        s.inbox.wake(); // the decoder thread exits now instead of after its idle timeout
        if inner.focused == id {
            let next = inner.sessions.get(pos).or_else(|| inner.sessions.last()).map(|n| n.id.clone());
            inner.focused = next.unwrap_or_default();
        }
        inner.refresh();
        Some(s)
    }

    /// Moves focus; returns the previous focus, or None if `id` is unknown or already focused.
    pub fn set_focus(&self, id: &str) -> Option<String> {
        let mut inner = self.inner.lock();
        if inner.focused == id || !inner.sessions.iter().any(|s| s.id == id) {
            return None;
        }
        let prev = std::mem::replace(&mut inner.focused, id.to_string());
        inner.refresh();
        Some(prev)
    }

    /// 0 = tabs, 1 = split. Returns sessions that just became visible.
    pub fn set_layout(&self, layout: i32) -> Vec<Arc<Session>> {
        let mut inner = self.inner.lock();
        inner.layout = if layout == 1 { 1 } else { 0 };
        inner.refresh()
    }

    /// 0 = tabs, 1 = split view.
    pub fn layout(&self) -> i32 {
        self.inner.lock().layout
    }

    /// Ends every session (relay connection lost).
    pub fn clear(&self) -> Vec<Arc<Session>> {
        let mut inner = self.inner.lock();
        inner.focused.clear();
        let all: Vec<Arc<Session>> = std::mem::take(&mut inner.sessions);
        for s in &all {
            s.alive.store(false, Ordering::SeqCst);
            s.visible.store(false, Ordering::SeqCst);
            s.audio_on.store(false, Ordering::SeqCst);
            s.inbox.wake();
        }
        *self.swallowed_click.lock() = None;
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::binary;
    use core_engine::codec::frame::{FrameHeader, KIND_DELTA};

    #[test]
    fn single_session_registry_holds_one_session_and_routes_untagged_messages() {
        let r = Registry::new();
        assert!(!r.multi());
        assert!(matches!(r.open("111111111", 1, 0, 2), Open::Opened(_)));
        assert!(matches!(r.open("222222222", 1, 0, 2), Open::Full), "relay without multi-session: one session only");
        assert!(matches!(r.open("111111111", 1, 0, 2), Open::Exists(_)));
        assert_eq!(r.single().unwrap().id, "111111111");
        r.set_focus("111111111");
        let s = r.get("111111111").unwrap();
        assert!(s.visible.load(Ordering::SeqCst) && s.audio_on.load(Ordering::SeqCst));
    }

    #[test]
    fn multi_registry_caps_at_four_and_demuxes_by_host() {
        let r = Registry::new();
        r.set_multi(true);
        for id in ["111111111", "222222222", "333333333", "444444444"] {
            assert!(matches!(r.open(id, 1, 0, 2), Open::Opened(_)));
        }
        assert!(matches!(r.open("555555555", 1, 0, 2), Open::Full));
        assert!(r.single().is_none(), "untagged messages never reach a multi-session tab");
        let frame =
            FrameHeader { kind: KIND_DELTA, flags: 0, seq: 1, width: 4, height: 4, moves: vec![], patches: vec![] }
                .encode(&[]);
        let tagged = binary::tag("333333333", &frame).unwrap();
        let (host, payload) = binary::untag(&tagged).unwrap();
        r.get(host).unwrap().inbox.push(payload.to_vec());
        assert_eq!(r.get("333333333").unwrap().inbox.drain(), vec![frame]);
        assert!(r.get("111111111").unwrap().inbox.is_empty());
        let gone = r.remove("222222222").unwrap();
        assert!(!gone.alive.load(Ordering::SeqCst));
        assert!(r.get("111111111").unwrap().alive.load(Ordering::SeqCst));
        assert!(matches!(r.open("555555555", 1, 0, 2), Open::Opened(_)));
        assert_eq!(r.clear().len(), 4);
        assert!(r.is_empty());
    }

    #[test]
    fn focus_drives_visibility_audio_and_survives_removal() {
        let r = Registry::new();
        r.set_multi(true);
        for id in ["111111111", "222222222", "333333333"] {
            r.open(id, 1, 0, 2);
        }
        assert_eq!(r.set_focus("111111111"), Some(String::new()));
        assert_eq!(r.set_focus("111111111"), None, "unchanged focus is not a change");
        assert_eq!(r.set_focus("999999999"), None, "unknown session");
        let vis = |id: &str| r.get(id).unwrap().visible.load(Ordering::SeqCst);
        let audio = |id: &str| r.get(id).unwrap().audio_on.load(Ordering::SeqCst);
        assert!(vis("111111111") && !vis("222222222") && !vis("333333333"), "tabs: only the focused tab is decoded");
        assert_eq!(r.set_focus("222222222"), Some("111111111".into()));
        assert!(!vis("111111111") && vis("222222222") && audio("222222222") && !audio("111111111"));
        let shown = r.set_layout(1);
        assert_eq!(shown.len(), 2);
        assert!(vis("111111111") && vis("333333333") && !audio("333333333"));
        r.set_layout(0);
        r.remove("222222222");
        assert_eq!(r.focused_id(), "333333333");
        assert!(vis("333333333") && audio("333333333"));
        r.remove("333333333");
        assert_eq!(r.focused_id(), "111111111");
        r.remove("111111111");
        assert_eq!(r.focused_id(), "");
    }

    #[test]
    fn clipboard_push_only_when_the_host_has_not_seen_it() {
        assert!(clipboard_push_needed(42, 41));
        assert!(!clipboard_push_needed(42, 42), "already synced with this host");
        assert!(!clipboard_push_needed(0, 41), "clipboard unavailable");
    }
}
