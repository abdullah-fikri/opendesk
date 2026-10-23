//! Viewer-side sessions: one entry per host this machine controls, keyed by the host's clean desk ID
//! (docs/multi-session.md). Against a relay without the `multi-session` capability there is at most one
//! entry and every message keeps its legacy shape. The host role is not tracked here: it stays single.
//!
//! Threads: the registry is shared by the UI thread and the network thread. Each connected session owns one
//! decoder thread (created at `session-connected`, stopped when the session ends), so one session costs what
//! the single viewer cost before and nothing runs while no session is open.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::protocol::OutgoingMsg;
use crate::session::{decode_b64_to_pixel_buffer, VideoInbox, ViewerDecoder, ViewerOutput};

/// Capability sent in `register` and expected in the relay's `registered-ack`.
pub const CAP: &str = "multi-session";
/// Sessions a multi-session viewer may hold (the relay enforces the same limit).
pub const MAX_SESSIONS: usize = 4;
/// First byte of a session-tagged binary message: `[0x10][9 ASCII digits host ID][legacy payload]`.
pub const BIN_TAG: u8 = 0x10;
pub const BIN_TAG_LEN: usize = 10;

pub const MSG_FULL_MULTI: &str = "Maksimal 4 sesi bersamaan. Tutup salah satu sesi terlebih dahulu.";
pub const MSG_FULL_LEGACY: &str = "Server relay ini belum mendukung multi-sesi. Tutup sesi yang berjalan untuk membuka sesi baru.";

fn valid_host_id(id: &str) -> bool {
    id.len() == BIN_TAG_LEN - 1 && id.bytes().all(|b| b.is_ascii_digit())
}

/// Prefixes a viewer -> host binary message (voice) with the session tag. None if `host` is not a clean ID.
pub fn tag_binary(host: &str, payload: &[u8]) -> Option<Vec<u8>> {
    if !valid_host_id(host) {
        return None;
    }
    let mut out = Vec::with_capacity(BIN_TAG_LEN + payload.len());
    out.push(BIN_TAG);
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(payload);
    Some(out)
}

/// Splits a host -> viewer binary message into (host ID, legacy payload). Legacy messages (0x02 audio,
/// 0xFF 0xD8 video) never start with the tag byte, so None means "untagged, use the legacy path".
pub fn untag_binary(bin: &[u8]) -> Option<(&str, &[u8])> {
    if bin.len() <= BIN_TAG_LEN || bin[0] != BIN_TAG {
        return None;
    }
    let id = std::str::from_utf8(&bin[1..BIN_TAG_LEN]).ok()?;
    valid_host_id(id).then(|| (id, &bin[BIN_TAG_LEN..]))
}

/// One host monitor from a `monitors` input event.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub index: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}

/// Parses `{action:"monitors", list:[{index,width,height,primary}], active}`. Bounded, never trusts sizes.
pub fn parse_monitors(event: &Value) -> Option<(Vec<Monitor>, i32)> {
    let list = event.get("list")?.as_array()?;
    let monitors = list
        .iter()
        .take(16)
        .filter_map(|m| {
            Some(Monitor {
                index: m.get("index")?.as_u64()?.min(63) as i32,
                width: m.get("width")?.as_i64()?.clamp(0, 65_535) as i32,
                height: m.get("height")?.as_i64()?.clamp(0, 65_535) as i32,
                primary: m.get("primary").and_then(|v| v.as_bool()).unwrap_or(false),
            })
        })
        .collect();
    let active = event.get("active").and_then(|v| v.as_u64()).unwrap_or(0).min(63) as i32;
    Some((monitors, active))
}

/// Clipboard follows focus: push the viewer's clipboard to a newly focused host only if it changed since that
/// host last saw it (pushed, synced from it, or present when its session started).
pub fn clipboard_push_needed(current_seq: u32, session_seq: u32) -> bool {
    current_seq != 0 && current_seq != session_seq
}

fn caps_has(caps: &Value, cap: &str) -> bool {
    caps.as_array().is_some_and(|list| list.iter().any(|c| c.as_str() == Some(cap)))
}

/// State of one viewer session, shared by the UI, network and decoder threads.
pub struct Session {
    pub id: String,
    /// False while the connect is pending (Android approval); true after `session-connected`.
    pub connected: AtomicBool,
    pub inbox: Arc<VideoInbox>,
    /// Legacy JSON/base64 frame (`frame` message), newest only.
    pub b64: Mutex<Option<String>>,
    /// The decoder thread runs while true.
    pub alive: AtomicBool,
    decoder_started: AtomicBool,
    /// Shown on screen (focused tab, or any tile in split view). Hidden sessions are not decoded.
    pub visible: AtomicBool,
    /// Focused: its audio plays (shared with the P2P sink).
    pub audio_on: Arc<AtomicBool>,
    pub p2p_active: AtomicBool,
    pub res_mode: AtomicUsize,
    /// Canvas size in physical pixels (session::pack_box), 0 = unknown.
    pub viewport: AtomicU64,
    /// Box last sent to the host in automatic mode, 0 = none.
    pub last_sent_box: AtomicU64,
    pub mic_muted: AtomicBool,
    pub audio_quality: AtomicUsize,
    /// Clipboard sequence number this host last saw (see `clipboard_push_needed`).
    pub clip_seq: AtomicU32,
    /// The host announced its monitors (newer hosts); otherwise monitor changes use `switch-monitor`.
    pub has_monitors: AtomicBool,
    pub file_offer: AtomicBool,
}

impl Session {
    fn new(id: &str, res_mode: usize, viewport: u64, audio_quality: usize) -> Self {
        Session {
            id: id.to_string(),
            connected: AtomicBool::new(false),
            inbox: Arc::new(VideoInbox::default()),
            b64: Mutex::new(None),
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
            clip_seq: AtomicU32::new(0),
            has_monitors: AtomicBool::new(false),
            file_offer: AtomicBool::new(false),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Records the host's caps from `session-connected.peerCaps` (file caps also go to file_transfer).
    pub fn set_peer_caps(&self, peer_caps: &Value, relay_offer: bool) {
        let ack = caps_has(peer_caps, "file-ack");
        let offer = caps_has(peer_caps, "file-offer") && relay_offer;
        let folder = caps_has(peer_caps, crate::file_transfer::CAP_FILE_FOLDER);
        self.file_offer.store(offer, Ordering::SeqCst);
        crate::file_transfer::set_session_caps(&self.id, ack, offer, folder);
    }
}

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
    /// Visibility and audio follow focus and layout.
    fn refresh(&self) -> Vec<Arc<Session>> {
        let mut shown = Vec::new();
        for s in &self.sessions {
            let focused = s.id == self.focused;
            let visible = focused || self.layout == 1;
            if visible && !s.visible.swap(true, Ordering::SeqCst) {
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
}

pub static VIEWERS: LazyLock<Registry> = LazyLock::new(Registry::new);

impl Registry {
    pub fn new() -> Self {
        Registry { multi: AtomicBool::new(false), inner: Mutex::new(Inner::default()) }
    }

    /// The relay advertised `multi-session` on this connection.
    pub fn multi(&self) -> bool {
        self.multi.load(Ordering::SeqCst)
    }

    pub fn set_multi(&self, on: bool) {
        self.multi.store(on, Ordering::SeqCst);
    }

    pub fn limit(&self) -> usize {
        if self.multi() { MAX_SESSIONS } else { 1 }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().sessions.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        if id.is_empty() {
            return None;
        }
        self.inner.lock().unwrap().sessions.iter().find(|s| s.id == id).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Session>> {
        self.inner.lock().unwrap().sessions.clone()
    }

    pub fn focused(&self) -> Option<Arc<Session>> {
        let inner = self.inner.lock().unwrap();
        inner.sessions.iter().find(|s| s.id == inner.focused).cloned()
    }

    pub fn focused_id(&self) -> String {
        self.inner.lock().unwrap().focused.clone()
    }

    pub fn is_focused(&self, id: &str) -> bool {
        !id.is_empty() && self.inner.lock().unwrap().focused == id
    }

    pub fn layout(&self) -> i32 {
        self.inner.lock().unwrap().layout
    }

    /// The session an untagged message belongs to: the single session of a legacy connection.
    pub fn legacy(&self) -> Option<Arc<Session>> {
        if self.multi() {
            return None;
        }
        self.inner.lock().unwrap().sessions.first().cloned()
    }

    /// Session for a message: by `fromClean` when present, else the legacy single session.
    pub fn route(&self, from: Option<&str>) -> Option<Arc<Session>> {
        match from {
            Some(id) => self.get(id),
            None => self.legacy(),
        }
    }

    /// Adds a (pending) session unless it exists or the limit is reached. Focus is left to the caller.
    pub fn open(&self, id: &str, res_mode: usize, viewport: u64, audio_quality: usize) -> Open {
        let limit = self.limit();
        let mut inner = self.inner.lock().unwrap();
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
        let mut inner = self.inner.lock().unwrap();
        let pos = inner.sessions.iter().position(|s| s.id == id)?;
        let s = inner.sessions.remove(pos);
        s.alive.store(false, Ordering::SeqCst);
        s.visible.store(false, Ordering::SeqCst);
        s.audio_on.store(false, Ordering::SeqCst);
        if inner.focused == id {
            let next = inner.sessions.get(pos).or_else(|| inner.sessions.last()).map(|n| n.id.clone());
            inner.focused = next.unwrap_or_default();
        }
        inner.refresh();
        Some(s)
    }

    /// Moves focus; returns the previous focus, or None if `id` is unknown or already focused.
    pub fn set_focus(&self, id: &str) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.focused == id || !inner.sessions.iter().any(|s| s.id == id) {
            return None;
        }
        let prev = std::mem::replace(&mut inner.focused, id.to_string());
        inner.refresh();
        Some(prev)
    }

    /// 0 = tabs, 1 = split. Returns sessions that just became visible.
    pub fn set_layout(&self, layout: i32) -> Vec<Arc<Session>> {
        let mut inner = self.inner.lock().unwrap();
        inner.layout = if layout == 1 { 1 } else { 0 };
        inner.refresh()
    }

    /// Ends every session (relay connection lost).
    pub fn clear(&self) -> Vec<Arc<Session>> {
        let mut inner = self.inner.lock().unwrap();
        inner.focused.clear();
        let all: Vec<Arc<Session>> = inner.sessions.drain(..).collect();
        for s in &all {
            s.alive.store(false, Ordering::SeqCst);
            s.visible.store(false, Ordering::SeqCst);
            s.audio_on.store(false, Ordering::SeqCst);
        }
        all
    }
}

/// What the decoder hands the UI for one shown frame.
pub struct FrameOut {
    pub buf: slint::SharedPixelBuffer<slint::Rgba8Pixel>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub p2p: bool,
}

fn request_key(tx: &Sender<OutgoingMsg>, id: &str) {
    let _ = tx.send(OutgoingMsg::Json(json!({
        "type": "relay-input",
        "targetClean": id,
        "event": { "action": "video-keyframe" }
    })));
}

/// Starts the session's decoder thread once. `show` runs for every decoded frame (it posts to the UI).
/// Hidden sessions (background tabs) are not decoded at all: their messages are dropped together with the
/// decoder state, and showing the session again asks the host for a key frame.
pub fn spawn_decoder<F>(sess: Arc<Session>, tx: Sender<OutgoingMsg>, show: F)
where
    F: Fn(&Arc<Session>, FrameOut) + Send + 'static,
{
    if sess.decoder_started.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::spawn(move || {
        let mut frame_count = 0u32;
        let mut fps_timer = Instant::now();
        let mut current_fps = 0u32;
        let mut viewer: Option<ViewerDecoder> = None;
        let mut last_key_request: Option<Instant> = None;
        let mut was_visible = sess.visible.load(Ordering::SeqCst);

        while sess.alive.load(Ordering::SeqCst) {
            let visible = sess.visible.load(Ordering::SeqCst);
            if visible && !was_visible {
                // Everything sent while hidden was dropped: resync from a key frame.
                last_key_request = Some(Instant::now());
                request_key(&tx, &sess.id);
            }
            was_visible = visible;

            let batch = sess.inbox.drain();
            let b64_opt = if batch.is_empty() { sess.b64.lock().unwrap().take() } else { None };
            if batch.is_empty() && b64_opt.is_none() {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            if !visible {
                viewer = None; // frees the framebuffer and the H.264 decoder while the tab is in the background
                continue;
            }

            let decoded = if !batch.is_empty() {
                match viewer.get_or_insert_with(ViewerDecoder::new).process(batch) {
                    ViewerOutput::Frame(buf, w, h) => Some((buf, w, h)),
                    ViewerOutput::Unchanged => None,
                    ViewerOutput::NeedKey => {
                        // Lost or undecodable delta: ask the host for a key frame, at most every 300 ms.
                        if last_key_request.is_none_or(|t| t.elapsed() >= Duration::from_millis(300)) {
                            last_key_request = Some(Instant::now());
                            crate::logger::info("desk_rt.video", "Delta chain broken; requesting key frame.");
                            request_key(&tx, &sess.id);
                        }
                        None
                    }
                }
            } else {
                b64_opt.and_then(|b64| decode_b64_to_pixel_buffer(&b64))
            };

            if let Some((buf, width, height)) = decoded {
                frame_count += 1;
                let elapsed = fps_timer.elapsed();
                if elapsed >= Duration::from_secs(1) {
                    current_fps = ((frame_count as f64 / elapsed.as_secs_f64()).round() as u32).max(1);
                    frame_count = 0;
                    fps_timer = Instant::now();
                }
                let p2p = sess.p2p_active.load(Ordering::Relaxed);
                show(&sess, FrameOut { buf, width, height, fps: current_fps, p2p });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_engine::codec::frame::{FrameHeader, KIND_DELTA};

    #[test]
    fn binary_tag_round_trip_and_legacy_payloads_stay_untagged() {
        let audio = [0x02u8, 48, 1, 2, 3, 4];
        let tagged = tag_binary("222222222", &audio).unwrap();
        assert_eq!(tagged.len(), BIN_TAG_LEN + audio.len());
        assert_eq!(&tagged[..10], b"\x10222222222");
        assert_eq!(untag_binary(&tagged), Some(("222222222", &audio[..])));
        // Legacy binaries are never parsed as tagged.
        assert_eq!(untag_binary(&audio), None);
        assert_eq!(untag_binary(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0, 0, 0, 0, 0]), None);
        // Malformed tags: short, empty payload, non-digit ID.
        assert_eq!(untag_binary(b"\x10222222222"), None);
        assert_eq!(untag_binary(b"\x1022222222x\x02\x00"), None);
        assert!(tag_binary("22222222", &audio).is_none());
        assert!(tag_binary("", &audio).is_none());
    }

    #[test]
    fn monitors_event_round_trips_and_rejects_garbage() {
        let mons = [
            core_engine::capture::MonitorBounds { left: 0, top: 0, width: 1920, height: 1080 },
            core_engine::capture::MonitorBounds { left: 1920, top: 0, width: 2560, height: 1440 },
        ];
        let ev = crate::session::monitors_event(&mons, 1);
        assert_eq!(ev["action"], "monitors");
        let (list, active) = parse_monitors(&ev).unwrap();
        assert_eq!(active, 1);
        assert_eq!(list, vec![
            Monitor { index: 0, width: 1920, height: 1080, primary: true },
            Monitor { index: 1, width: 2560, height: 1440, primary: false },
        ]);
        assert!(parse_monitors(&json!({"action": "monitors"})).is_none());
        let (list, active) = parse_monitors(&json!({"list": [{"index": 0}, {"index": -1, "width": 1, "height": 1}, {"index": 999, "width": 100000, "height": 5}]})).unwrap();
        assert_eq!(list, vec![Monitor { index: 63, width: 65_535, height: 5, primary: false }]);
        assert_eq!(active, 0);
    }

    #[test]
    fn legacy_registry_holds_one_session_and_routes_untagged_messages() {
        let r = Registry::new();
        assert!(!r.multi());
        assert!(matches!(r.open("111111111", 1, 0, 2), Open::Opened(_)));
        assert!(matches!(r.open("222222222", 1, 0, 2), Open::Full), "old relay: one session only");
        assert!(matches!(r.open("111111111", 1, 0, 2), Open::Exists(_)));
        assert_eq!(r.legacy().unwrap().id, "111111111");
        assert_eq!(r.route(None).unwrap().id, "111111111");
        assert!(r.route(Some("333333333")).is_none());
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
        assert!(r.legacy().is_none(), "untagged messages never reach a multi-session tab");
        // Demux: a tagged frame lands in its own host's inbox only.
        let frame = FrameHeader { kind: KIND_DELTA, flags: 0, seq: 1, width: 4, height: 4, moves: vec![], patches: vec![] }.encode(&[]);
        let tagged = tag_binary("333333333", &frame).unwrap();
        let (host, payload) = untag_binary(&tagged).unwrap();
        r.get(host).unwrap().inbox.push(payload.to_vec());
        assert_eq!(r.get("333333333").unwrap().inbox.drain(), vec![frame]);
        assert!(r.get("111111111").unwrap().inbox.is_empty());
        // Removing one frees a slot and stops only that decoder.
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
        // Split view shows every tile; audio stays with focus.
        let shown = r.set_layout(1);
        assert_eq!(shown.len(), 2);
        assert!(vis("111111111") && vis("333333333") && !audio("333333333"));
        r.set_layout(0);
        // Closing the focused tab focuses its right neighbour, then the last one.
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

    #[test]
    fn hidden_session_skips_decoding_and_resyncs_with_a_key_frame_when_shown() {
        let r = Registry::new();
        r.set_multi(true);
        let Open::Opened(a) = r.open("111111111", 1, 0, 2) else { panic!() };
        let Open::Opened(b) = r.open("222222222", 1, 0, 2) else { panic!() };
        r.set_focus("111111111");
        let (tx, rx) = std::sync::mpsc::channel();
        let shown = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&shown);
        spawn_decoder(Arc::clone(&b), tx, move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let key = core_engine::codec::delta::key_header(4, 4, 4).encode(&[]);
        b.inbox.push(key.clone());
        thread::sleep(Duration::from_millis(50));
        assert!(b.inbox.is_empty(), "hidden tab drains its inbox");
        assert_eq!(shown.load(Ordering::SeqCst), 0);
        assert!(rx.try_recv().is_err());
        r.set_focus("222222222");
        let msg = rx.recv_timeout(Duration::from_secs(2)).expect("key frame request");
        let OutgoingMsg::Json(v) = msg else { panic!() };
        assert_eq!(v["targetClean"], "222222222");
        assert_eq!(v["event"]["action"], "video-keyframe");
        r.remove("222222222");
        let _ = a;
    }
}

/// End to end against a running relay: `cargo test --release relay_e2e -- --ignored --nocapture`
/// (OPENDESK_E2E_RELAY, default ws://127.0.0.1:8088). Two simulated hosts stream real key frames to one
/// multi-session viewer, which demultiplexes and decodes them per tab; a legacy viewer stays untagged.
#[cfg(test)]
mod relay_e2e {
    use super::*;
    use core_engine::stream::{FrameInput, StreamEncoder};
    use std::net::TcpStream;
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{Message, WebSocket};

    type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

    fn ids() -> [String; 4] {
        // 9 digits, no leading zero, unlikely to collide with real devices on the same relay.
        let base = 700_000_000 + (std::process::id() % 50_000) * 1000;
        [0, 1, 2, 3].map(|i| (base + i).to_string())
    }

    fn connect(id: &str, caps: &[&str]) -> (Ws, Value) {
        let url = std::env::var("OPENDESK_E2E_RELAY").unwrap_or_else(|_| "ws://127.0.0.1:8088".into());
        let (mut ws, _) = tungstenite::connect(url.as_str()).expect("relay reachable");
        if let MaybeTlsStream::Plain(s) = ws.get_mut() {
            s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        }
        let reg = json!({ "type": "register", "deskId": id, "platform": "windows-pc", "deviceName": format!("e2e-{id}"), "deviceToken": "", "isAdmin": false, "caps": caps });
        ws.send(Message::Text(reg.to_string().into())).unwrap();
        let ack = wait_json(&mut ws, |v| v["type"] == "registered-ack" || v["type"] == "error");
        assert_eq!(ack["type"], "registered-ack", "register {id}: {ack}");
        (ws, ack)
    }

    fn read(ws: &mut Ws) -> Option<Message> {
        match ws.read() {
            Ok(m) => Some(m),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => None,
            Err(e) => panic!("socket: {e}"),
        }
    }

    fn wait_json(ws: &mut Ws, want: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(Message::Text(t)) = read(ws) {
                let v: Value = serde_json::from_str(&t).unwrap();
                if want(&v) {
                    return v;
                }
            }
        }
        panic!("timed out waiting for a message");
    }

    fn wait_binary(ws: &mut Ws) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(Message::Binary(b)) = read(ws) {
                return b.to_vec();
            }
        }
        panic!("timed out waiting for a binary message");
    }

    /// A solid-colour key frame, as a host's capture loop would send it.
    fn key_frame(rgb: u32) -> Vec<u8> {
        let (w, h) = (64usize, 48usize);
        let px = vec![rgb; w * h];
        StreamEncoder::new(w as i32, h as i32, 85, true).encode(&FrameInput { pixels: &px, stride: w, dirty_hint: None, moves: &[] }).unwrap()
    }

    #[test]
    #[ignore]
    fn relay_e2e_two_hosts_one_multi_session_viewer() {
        let [viewer_id, a_id, b_id, legacy_id] = ids();
        let host_caps = ["file-ack", "file-offer", "cursor-shape", "video-delta"];
        let (mut host_a, _) = connect(&a_id, &host_caps);
        let (mut host_b, _) = connect(&b_id, &host_caps);
        let (mut viewer, ack) = connect(&viewer_id, &["file-ack", "video-delta", CAP]);
        if !caps_has(&ack["caps"], CAP) {
            eprintln!("relay lacks multi-session; skipping (legacy behaviour is the single-session path)");
            return;
        }
        let reg = Registry::new();
        reg.set_multi(true);

        for host in [&a_id, &b_id] {
            viewer.send(Message::Text(json!({ "type": "connect-request", "targetId": host }).to_string().into())).unwrap();
            let sc = wait_json(&mut viewer, |v| v["type"] == "session-connected");
            assert_eq!(sc["role"], "viewer");
            let Open::Opened(s) = reg.open(sc["targetClean"].as_str().unwrap(), 1, 0, 2) else { panic!() };
            s.connected.store(true, Ordering::SeqCst);
        }
        for host in [&mut host_a, &mut host_b] {
            let sc = wait_json(host, |v| v["type"] == "session-connected");
            assert_eq!(sc["role"], "host");
            assert_eq!(sc["targetClean"], viewer_id.as_str());
        }
        reg.set_focus(&a_id);
        reg.set_layout(1); // split view: both decoded

        // Both hosts stream; the viewer demultiplexes by tag and decodes each tab separately.
        host_a.send(Message::Binary(key_frame(0x00FF_0000).into())).unwrap();
        host_b.send(Message::Binary(key_frame(0x0000_00FF).into())).unwrap();
        let mut decoders = std::collections::HashMap::new();
        for _ in 0..2 {
            let bin = wait_binary(&mut viewer);
            let (host, payload) = untag_binary(&bin).expect("multi-session frames are tagged");
            let s = reg.route(Some(host)).expect("frame for an open tab");
            s.inbox.push(payload.to_vec());
            let out = decoders.entry(host.to_string()).or_insert_with(ViewerDecoder::new).process(s.inbox.drain());
            let ViewerOutput::Frame(buf, w, h) = out else { panic!("frame from {host} did not decode") };
            assert_eq!((w, h), (64, 48));
            let px = buf.as_bytes();
            let (r, b) = (px[0], px[2]);
            if host == a_id {
                assert!(r > 200 && b < 60, "A is red: {r},{b}");
            } else {
                assert!(b > 200 && r < 60, "B is blue: {r},{b}");
            }
        }
        assert_eq!(decoders.len(), 2, "one frame per host");

        // Viewer -> host: text needs targetClean, binary (voice) needs the tag; the host sees legacy bytes.
        viewer.send(Message::Text(json!({ "type": "relay-input", "targetClean": b_id, "event": { "action": "video-keyframe" } }).to_string().into())).unwrap();
        let cmd = wait_json(&mut host_b, |v| v["type"] == "input-command");
        assert_eq!(cmd["event"]["action"], "video-keyframe");
        let voice = [0x02u8, 48, 1, 0, 2, 0];
        viewer.send(Message::Binary(tag_binary(&a_id, &voice).unwrap().into())).unwrap();
        assert_eq!(wait_binary(&mut host_a), voice.to_vec(), "relay strips the tag for the host");

        // Host -> multi-session viewer text carries fromClean; the monitors announcement parses.
        let mons = [core_engine::capture::MonitorBounds { left: 0, top: 0, width: 1920, height: 1080 }];
        host_b.send(Message::Text(json!({ "type": "relay-input", "targetClean": viewer_id, "event": crate::session::monitors_event(&mons, 0) }).to_string().into())).unwrap();
        let ev = wait_json(&mut viewer, |v| v["type"] == "input-command");
        assert_eq!(ev["fromClean"], b_id.as_str());
        assert_eq!(parse_monitors(&ev["event"]).unwrap().0.len(), 1);

        // Closing one tab leaves the other streaming.
        viewer.send(Message::Text(json!({ "type": "session-disconnect", "targetClean": a_id }).to_string().into())).unwrap();
        wait_json(&mut host_a, |v| v["type"] == "session-closed");
        reg.remove(&a_id);
        host_b.send(Message::Binary(key_frame(0x0000_00FF).into())).unwrap();
        let bin = wait_binary(&mut viewer);
        assert_eq!(untag_binary(&bin).unwrap().0, b_id);

        // A host ending its side names itself in session-closed.
        host_b.send(Message::Text(json!({ "type": "session-disconnect", "targetClean": viewer_id }).to_string().into())).unwrap();
        let closed = wait_json(&mut viewer, |v| v["type"] == "session-closed");
        assert_eq!(closed["targetClean"], b_id.as_str());

        // Legacy viewer (no cap) on the same relay: untagged frames, no fromClean.
        let (mut legacy, _) = connect(&legacy_id, &["file-ack", "video-delta"]);
        legacy.send(Message::Text(json!({ "type": "connect-request", "targetId": a_id }).to_string().into())).unwrap();
        wait_json(&mut legacy, |v| v["type"] == "session-connected");
        wait_json(&mut host_a, |v| v["type"] == "session-connected");
        let frame = key_frame(0x0000_FF00);
        host_a.send(Message::Binary(frame.clone().into())).unwrap();
        let bin = wait_binary(&mut legacy);
        assert!(untag_binary(&bin).is_none());
        assert_eq!(bin, frame, "legacy viewers get the exact host bytes");
        host_a.send(Message::Text(json!({ "type": "relay-input", "targetClean": legacy_id, "event": { "action": "cursor-shape", "shape": 2 } }).to_string().into())).unwrap();
        assert!(wait_json(&mut legacy, |v| v["type"] == "input-command").get("fromClean").is_none());
    }
}
