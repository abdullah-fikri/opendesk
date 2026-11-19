//! Direct UDP path between session partners. The relay stays the control plane: it authenticates
//! devices, approves the session and carries the Noise handshake; this module only moves media/input.
//! Wire format and security decisions are documented in `docs/features/p2p.md`.

use std::collections::{BTreeMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use core_engine::codec::{from_base64, to_base64};
use parking_lot::{Condvar, Mutex, MutexGuard};
use serde_json::{Value, json};

use crate::domain::bulk::{self, BulkReceiver, BulkSender, OnDelivered};
use crate::domain::link_quality::LossMeter;
use crate::domain::nack::{self, Pushed, Reassembly};
use crate::domain::pacing::{self, Pacing};
use crate::domain::udp_window::{self, MIN_FRAMES_IN_FLIGHT, RttEstimator};
use crate::infra::audio::VoiceEngine;
use crate::infra::e2e::{E2e, P2P_NONCE_LIMIT};
use crate::infra::logger;
use crate::infra::port_map;
use crate::infra::send_gate::{SendGate, VideoLink};
use crate::infra::tcp_path;
use crate::infra::udp_socket::{self, PathSocket};
use crate::protocol::{ClientMessage, InputEvent, Outgoing};

mod bulk_stream;
mod candidates;
mod receive;
mod send;
mod signal;
#[cfg(test)]
pub(crate) mod tests;
mod wire;

use candidates::*;
use wire::*;

const NOISE: &str = "Noise_NN_25519_ChaChaPoly_BLAKE2s";
const MAGIC: u8 = 0x4F;
const MAX_DATAGRAM: usize = 1200;
const HEADER: usize = 9; // magic + nonce
const TAG: usize = 16;
const MAX_PLAIN: usize = MAX_DATAGRAM - HEADER - TAG;
const FRAG_DATA: usize = MAX_PLAIN - 9; // kind + frame_id + idx + count
const MAX_FRAGS: usize = 4096;
const MAX_CANDIDATES: usize = 8;
const REPLAY_WINDOW: u64 = 1024;
/// UDP probes this long without an answer: TCP connections are tried too.
const TCP_AFTER: Duration = Duration::from_secs(2);
const PATH_TIMEOUT: Duration = Duration::from_secs(3);
const FRAME_ACK_TIMEOUT: Duration = Duration::from_millis(200);
const INPUT_RETRANSMIT: Duration = Duration::from_millis(80);
const MAX_UNACKED: usize = 256;
/// The relay may drop a signal (rate limit) and says nothing, so with `e2e` the candidates go out again this many
/// times, every [`SIGNAL_RESEND_EVERY`] while probing; the partner ignores copies after the first.
const SIGNAL_RESENDS: u32 = 4;
const SIGNAL_RESEND_EVERY: Duration = Duration::from_secs(2);
/// UDP relay control datagrams (see `server-relay/internal/udprelay`): `[0x52][0x01][token]` asks, the relay
/// answers `[0x52][0x03][cookie]`, `[0x52][0x01][token][cookie]` binds and `[0x52][0x02]` confirms. The cookie
/// proves this address receives what the relay sends. Everything else to and from the relay is an ordinary path
/// datagram it forwards unread.
const RELAY_TAG: u8 = 0x52;
const RELAY_BIND: u8 = 0x01;
const RELAY_BOUND: u8 = 0x02;
const RELAY_CHALLENGE: u8 = 0x03;
const RELAY_COOKIE: usize = 8;
/// Bind again this often once bound (keeps the NAT mapping and the relay's pair alive) and while unbound.
const RELAY_REBIND: Duration = Duration::from_secs(15);
const RELAY_BIND_RETRY: Duration = Duration::from_secs(1);
/// Probes to a bound relay while no path is up, outside the bounded direct probing.
const RELAY_PROBE: Duration = Duration::from_secs(1);
/// Second STUN server, only used to tell the NAT type (see `gather_candidates`).
const STUN_CHECK: &str = "stun1.l.google.com:19302";
/// A port mapping is offered once the handshake is done; given up when that takes longer than this.
const PORT_MAP_OFFER_WAIT: Duration = Duration::from_secs(30);

const K_PROBE: u8 = 1;
const K_PROBE_ACK: u8 = 2;
const K_VIDEO: u8 = 3;
const K_AUDIO: u8 = 4;
const K_INPUT: u8 = 5;
const K_INPUT_ACK: u8 = 6;
const K_FRAME_ACK: u8 = 7;
const K_KEEPALIVE: u8 = 8;
/// Viewer to host: resend these fragments of a video frame (`video-nack`, see `domain::nack`).
const K_NACK: u8 = 9;
/// Frames whose fragments the host keeps for retransmits.
const NACK_HISTORY: usize = 4;
/// File stream segment (`file-p2p`, see `domain::bulk`): `[seq u32][flags u8][data]`.
const K_BULK: u8 = 10;
/// File stream acknowledgement: `[cum u32][rwnd u32][sack bitmap]`.
const K_BULK_ACK: u8 = 11;
const BULK_SEG: usize = MAX_PLAIN - 6;
/// Received file stream bytes the connection may hold unprocessed; the advertised window shrinks with it.
const BULK_BUFFER: usize = 16 << 20;
/// Kind the bulk sink receives once when the path drops with the file stream unfinished.
pub const BULK_LOST: u8 = 0;

/// Receives file stream messages: `(kind, payload)`.
pub type BulkSink = Arc<dyn Fn(u8, Vec<u8>) + Send + Sync>;

/// `OPENDESK_FILE_P2P=0` keeps file transfers on the relay.
pub fn file_p2p_enabled() -> bool {
    std::env::var("OPENDESK_FILE_P2P").map_or(true, |v| v != "0")
}

/// `OPENDESK_VIDEO_NACK=0` turns lost-fragment retransmits off (1.3.x behavior: a lost fragment drops the frame).
pub fn nack_enabled() -> bool {
    std::env::var("OPENDESK_VIDEO_NACK").map_or(true, |v| v != "0")
}

/// A complete video message that restarts the decoder chain (key frame, IDR, legacy JPEG): nothing older needs
/// to be waited for.
fn restarts_chain(frame: &[u8]) -> bool {
    use core_engine::codec::frame::{FLAG_IDR, KIND_H264, KIND_KEY, is_framed};
    !is_framed(frame) || frame[5] == KIND_KEY || (frame[5] == KIND_H264 && frame[6] & FLAG_IDR != 0)
}

/// Client-side kill switch: `OPENDESK_P2P=0` keeps every session on the relay.
pub fn enabled() -> bool {
    std::env::var("OPENDESK_P2P").map(|v| v != "0").unwrap_or(true)
}

/// `OPENDESK_UDP_RELAY=0` keeps the path direct-only (no `udp-relay` capability).
pub fn relay_enabled() -> bool {
    enabled() && std::env::var("OPENDESK_UDP_RELAY").map(|v| v != "0").unwrap_or(true)
}

/// Which way the path's datagrams currently go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Direct(SocketAddr),
    /// Through the relay server's UDP forwarder.
    Relayed(SocketAddr),
}

impl Route {
    fn addr(self) -> SocketAddr {
        match self {
            Route::Direct(a) | Route::Relayed(a) => a,
        }
    }
}

/// The relay server's UDP forwarder for this session (`udp-relay-offer`).
struct RelayLeg {
    addr: SocketAddr,
    token: [u8; 16],
    /// The relay's latest challenge cookie for this address.
    cookie: Option<[u8; RELAY_COOKIE]>,
    bound: bool,
    last_bind: Option<Instant>,
    last_probe: Option<Instant>,
}

/// Where received P2P traffic goes; the same sinks the relay path feeds.
pub struct Sinks {
    /// Receives each reassembled video message.
    pub video: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
    pub voice: Arc<VoiceEngine>,
    /// Input events from the viewer (host side only).
    pub input: Sender<InputEvent>,
    /// Viewer of several hosts: audio plays only while this is true (its tab has focus). None = always.
    pub audio_gate: Option<Arc<AtomicBool>>,
    /// Viewer side: counts the video fragments received and lost, for the stats overlay.
    pub loss: Option<Arc<Mutex<LossMeter>>>,
    /// Viewer side: the host resends lost fragments (`video-nack`), so ask for them and keep frames in order.
    pub nack: bool,
    /// File stream messages from the partner (`file-p2p`): `(kind, payload)`, and [`BULK_LOST`] when the path
    /// dropped mid-stream. None: the partner does not send files this way.
    pub bulk: Option<BulkSink>,
}

/// The fragments of a frame still to be sent by the pacer (see `domain::pacing`), each with when it may leave.
/// They are kept in plain text and sealed as they leave, so nonces stay in sending order.
struct Paced {
    started: Instant,
    pending: VecDeque<(Instant, Vec<u8>)>,
}

// One handshake per path, replaced once on completion: the size difference between variants costs nothing.
#[allow(clippy::large_enum_variant)]
enum Handshake {
    Pending(snow::HandshakeState),
    /// Waiting for the session's end-to-end handshake (`infra::e2e`), which keys this path too.
    Shared,
    Done(Arc<snow::StatelessTransportState>),
    Failed,
}

struct State {
    hs: Option<Handshake>,
    local_cands: Vec<SocketAddr>,
    pending_signal: Option<String>,
    signal_handled: bool,
    tx_nonce: u64,
    replay: Replay,
    peer_cands: Vec<SocketAddr>,
    /// The partner's direct address (the last one an authentic datagram came from) and when that was.
    peer_addr: Option<SocketAddr>,
    direct_rx: Option<Instant>,
    relay: Option<RelayLeg>,
    /// Last authentic datagram through the relay.
    relay_rx: Option<Instant>,
    route_was: Option<Route>,
    probe_started: Option<Instant>,
    last_probe: Instant,
    /// Times the candidates were sent again (see [`SIGNAL_RESENDS`]).
    signal_resends: u32,
    /// The candidates were gathered again after a session resume: resends go out as refresh signals.
    refreshed: bool,
    /// Datagrams that reached the socket, those that did not open, and the first sender: tells "nothing got
    /// through the NATs" apart from "packets arrive but are rejected".
    rx_datagrams: u64,
    rx_rejected: u64,
    first_from: Option<SocketAddr>,
    last_keepalive: Instant,
    frame_id: u32,
    /// Frames awaiting their acknowledgement, oldest first (at most `window`).
    frame_wait: VecDeque<(u32, Instant)>,
    /// Smoothed round trip of frame acknowledgements.
    ack_rtt: RttEstimator,
    /// Video frames that may be in flight (`domain::udp_window`).
    window: usize,
    /// The paced rest of the last frame, sent by the pacer thread.
    paced: Option<Paced>,
    reasm: Option<Reasm>,
    /// Reassembly with retransmit requests (viewer, `video-nack` hosts).
    nack_reasm: Reassembly,
    /// Host: plain fragments of the last frames, for retransmits.
    sent_frames: VecDeque<(u32, Vec<Vec<u8>>)>,
    next_seq: u32,
    unacked: VecDeque<(u32, Vec<u8>, Instant)>,
    rx_next: u32,
    rx_buf: BTreeMap<u32, Vec<u8>>,
    /// File stream to the partner and from it (`file-p2p`).
    bulk_tx: BulkSender,
    bulk_rx: BulkReceiver,
    /// Pacing of the file stream: when the next segment may leave.
    bulk_due: Instant,
}

/// A direct encrypted UDP path to one session partner (see `docs/features/p2p.md`).
pub struct P2p {
    pub partner: String,
    is_host: bool,
    prologue: Vec<u8>,
    sock: PathSocket,
    state: Mutex<State>,
    closed: AtomicBool,
    /// Gather the candidates again and send them as a refresh (see `refresh`).
    regather: AtomicBool,
    can_send_frame: Arc<SendGate>,
    /// Host side: send time, acknowledgement round trips and the window, for `host-stats`.
    video_link: Arc<VideoLink>,
    tx_out: Sender<Outgoing>,
    sinks: Sinks,
    /// Both peers list `e2e`: the path uses the session's `Noise_XX` keys instead of its own `Noise_NN`.
    e2e: Option<Arc<E2e>>,
    /// Host side: the window follows the acknowledgement round trip (else fixed at 2, `OPENDESK_DYNAMIC_WINDOW=0`).
    dynamic_window: bool,
    /// Host side: a frame's fragments are spread over up to half a frame interval (else one burst,
    /// `OPENDESK_UDP_PACING=0`).
    pacing: bool,
    /// Wakes the pacer thread when a frame has fragments to pace.
    pace_wake: Condvar,
    /// Wakes the file stream thread (new data, an acknowledgement).
    bulk_wake: Condvar,
    /// File stream bytes handed to the bulk sink and not yet processed (see [`P2p::bulk_consumed`]).
    bulk_pending: std::sync::atomic::AtomicUsize,
}

impl P2p {
    /// Opens the UDP socket and starts candidate gathering in the background. Returns None if no socket is available.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        is_host: bool,
        local_clean: &str,
        partner: String,
        can_send_frame: Arc<SendGate>,
        video_link: Arc<VideoLink>,
        tx_out: Sender<Outgoing>,
        sinks: Sinks,
        e2e: Option<Arc<E2e>>,
    ) -> Option<Arc<P2p>> {
        // A 1080p JPEG is ~100-200 fragments sent back to back; the default 64 KB buffer would drop most of them.
        let sock = PathSocket::bind(4 << 20).ok()?;
        let local: String = local_clean.chars().filter(|c| c.is_ascii_digit()).collect();
        let (viewer, host) = if is_host { (partner.clone(), local) } else { (local, partner.clone()) };
        let now = Instant::now();
        let dynamic_window = udp_window::dynamic_enabled(std::env::var(udp_window::DYNAMIC_WINDOW_ENV).ok().as_deref());
        let pacing = is_host && pacing::pacing_enabled(std::env::var(pacing::PACING_ENV).ok().as_deref());
        if is_host {
            logger::info(
                "anynet.p2p",
                if pacing {
                    "Video pacing: on; frames above the average size spread over at most half a frame interval."
                } else {
                    "Video pacing: off; every frame leaves in one burst (OPENDESK_UDP_PACING=0)."
                },
            );
            logger::info(
                "anynet.p2p",
                if dynamic_window {
                    "Video window: dynamic, 2..8 frames in flight from the acknowledgement round trip x target FPS."
                } else {
                    "Video window: fixed at 2 frames in flight (OPENDESK_DYNAMIC_WINDOW=0)."
                },
            );
        }
        let p = Arc::new(P2p {
            partner,
            is_host,
            prologue: format!("opendesk-p2p-v1|{viewer}|{host}").into_bytes(),
            sock,
            state: Mutex::new(State {
                hs: None,
                local_cands: Vec::new(),
                pending_signal: None,
                signal_handled: false,
                tx_nonce: 0,
                replay: Replay::new(),
                peer_cands: Vec::new(),
                peer_addr: None,
                direct_rx: None,
                relay: None,
                relay_rx: None,
                route_was: None,
                probe_started: None,
                last_probe: now,
                signal_resends: 0,
                refreshed: false,
                rx_datagrams: 0,
                rx_rejected: 0,
                first_from: None,
                last_keepalive: now,
                frame_id: 0,
                frame_wait: VecDeque::new(),
                ack_rtt: RttEstimator::default(),
                window: MIN_FRAMES_IN_FLIGHT,
                paced: None,
                reasm: None,
                nack_reasm: Reassembly::default(),
                sent_frames: VecDeque::new(),
                next_seq: 0,
                unacked: VecDeque::new(),
                rx_next: 0,
                rx_buf: BTreeMap::new(),
                bulk_tx: BulkSender::new(BULK_SEG),
                bulk_rx: BulkReceiver::default(),
                bulk_due: now,
            }),
            closed: AtomicBool::new(false),
            regather: AtomicBool::new(false),
            can_send_frame,
            video_link,
            tx_out,
            sinks,
            e2e,
            dynamic_window,
            pacing,
            pace_wake: Condvar::new(),
            bulk_wake: Condvar::new(),
            bulk_pending: std::sync::atomic::AtomicUsize::new(0),
        });
        let worker = Arc::clone(&p);
        crate::infra::thread::spawn_named("p2p-worker", move || {
            // Video and input arrive on this thread.
            let _mmcss = crate::infra::power::mmcss(crate::infra::power::Mmcss::Games);
            worker.run()
        });
        let bulk = Arc::clone(&p);
        crate::infra::thread::spawn_named("p2p-bulk", move || bulk.run_bulk());
        if pacing {
            let pacer = Arc::clone(&p);
            crate::infra::thread::spawn_named("p2p-pacer", move || {
                let _mmcss = crate::infra::power::mmcss(crate::infra::power::Mmcss::Games);
                pacer.pace()
            });
        }
        Some(p)
    }

    /// Closes the path; the worker thread exits and traffic falls back to the relay.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.can_send_frame.open();
            self.state.lock().bulk_tx.abort();
            self.bulk_wake.notify_all();
        }
    }

    /// The session resumed after a lost relay connection: a path that is down (a network change, say) gathers
    /// its candidates again and sends them to the partner, which probes them. A live path is left alone.
    pub fn refresh(&self) {
        if !self.closed.load(Ordering::SeqCst) && !self.is_active() {
            self.regather.store(true, Ordering::SeqCst);
        }
    }

    /// True for the link this machine opened as the host of the session.
    pub fn is_host(&self) -> bool {
        self.is_host
    }

    /// True once the Noise handshake has completed and the link is not closed.
    pub fn is_active(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && Self::alive(&self.state.lock())
    }

    /// The current route, if the path is up: direct whenever it works, else through the relay.
    pub fn route(&self) -> Option<Route> {
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        Self::route_of(&self.state.lock())
    }

    fn route_of(st: &State) -> Option<Route> {
        if !matches!(st.hs, Some(Handshake::Done(_))) {
            return None;
        }
        let fresh = |t: Option<Instant>| t.is_some_and(|t| t.elapsed() < PATH_TIMEOUT);
        match (st.peer_addr, &st.relay) {
            (Some(a), _) if fresh(st.direct_rx) => Some(Route::Direct(a)),
            (_, Some(r)) if fresh(st.relay_rx) => Some(Route::Relayed(r.addr)),
            _ => None,
        }
    }

    fn alive(st: &State) -> bool {
        Self::route_of(st).is_some()
    }

    /// Takes the relay server's UDP forwarder (`udp-relay-offer`) as a fallback route.
    pub fn add_relay(&self, addr: &str, token: &str) {
        let (Ok(SocketAddr::V4(v4)), Some(token)) = (addr.parse::<SocketAddr>(), parse_token(token)) else {
            logger::warn("anynet.p2p", &format!("Ignoring a malformed UDP relay offer ({addr})."));
            return;
        };
        if v4.ip().is_unspecified() || v4.port() == 0 {
            return;
        }
        let addr = SocketAddr::V4(v4);
        logger::info("anynet.p2p", &format!("UDP relay offered at {addr}."));
        self.state.lock().relay =
            Some(RelayLeg { addr, token, cookie: None, bound: false, last_bind: None, last_probe: None });
    }

    fn run(self: Arc<Self>) {
        let cands = gather_candidates(&self.sock);
        if self.e2e.is_some() {
            let mut st = self.state.lock();
            st.local_cands = cands.clone();
            self.send_signal(&[], &cands, false);
            st.hs = Some(Handshake::Shared);
            if let Some(sig) = st.pending_signal.take() {
                st.signal_handled = true;
                self.handle_signal(&mut st, &sig);
            }
        } else if !self.start_nn(&cands) {
            return;
        }
        logger::info("anynet.p2p", &format!("UDP ready with {} local candidates.", cands.len()));
        if let Some(local) = lan_ipv4(&cands).filter(|_| port_map::enabled()) {
            let p = Arc::clone(&self);
            crate::infra::thread::spawn_named("p2p-portmap", move || p.keep_port_mapping(local));
        }

        let _ = self.sock.set_read_timeout(Some(Duration::from_millis(50)));
        let mut buf = [0u8; 2048];
        while !self.closed.load(Ordering::SeqCst) {
            if self.regather.swap(false, Ordering::SeqCst) {
                self.regather_candidates();
            }
            if let Ok((n, from)) = self.sock.recv_from(&mut buf) {
                self.on_datagram(&buf[..n], from);
            }
            self.tick();
        }
        self.sock.tcp().close();
        let st = self.state.lock();
        let first = st.first_from.map_or("-".to_string(), |a| a.to_string());
        logger::info(
            "anynet.p2p",
            &format!(
                "P2P path closed; {} datagrams received ({} rejected), first from {first}.",
                st.rx_datagrams, st.rx_rejected
            ),
        );
    }

    fn tick(&self) {
        let mut st = self.state.lock();
        let now = Instant::now();
        if self.sinks.nack {
            let pushed = st.nack_reasm.tick(now, restarts_chain);
            self.deliver(&mut st, pushed);
        }
        if matches!(st.hs, Some(Handshake::Shared))
            && let Some(t) = self.e2e.as_ref().and_then(|e| e.transport())
        {
            st.hs = Some(Handshake::Done(t));
            st.probe_started = Some(now);
            logger::info("anynet.p2p", "End-to-end keys ready (Noise_XX); probing candidates.");
        }
        self.maintain_relay(&mut st, now);
        let route = Self::route_of(&st);
        let alive = route.is_some();
        if route != st.route_was {
            let was_alive = st.route_was.is_some();
            st.route_was = route;
            match route {
                Some(Route::Direct(a)) if self.sock.tcp().has(a) => {
                    logger::info("anynet.p2p", &format!("P2P path active: direct TCP via {a}."))
                }
                Some(Route::Direct(a)) => logger::info("anynet.p2p", &format!("P2P path active: direct via {a}.")),
                Some(Route::Relayed(a)) => logger::info("anynet.p2p", &format!("P2P path active: UDP relay via {a}.")),
                None => logger::info("anynet.p2p", "P2P path lost; falling back to relay."),
            }
            if was_alive && !alive {
                st.probe_started = Some(now);
                st.frame_wait.clear();
                st.paced = None;
                // shortcut: unacked input is dropped, not replayed on the relay, to avoid double clicks.
                st.unacked.clear();
                self.can_send_frame.open();
                self.drop_bulk(&mut st);
            }
        }
        if alive {
            if now.duration_since(st.last_keepalive) >= Duration::from_secs(1) {
                st.last_keepalive = now;
                self.send_to_peer(&mut st, &[K_KEEPALIVE]);
            }
            let waiting = st.frame_wait.len();
            st.frame_wait.retain(|(_, t)| now.duration_since(*t) < FRAME_ACK_TIMEOUT);
            if st.frame_wait.len() < waiting && st.frame_wait.len() < st.window {
                self.can_send_frame.open();
            }
            let resend: Vec<(u32, Vec<u8>)> = st
                .unacked
                .iter_mut()
                .filter(|(_, _, t)| now.duration_since(*t) >= INPUT_RETRANSMIT)
                .map(|(s, b, t)| {
                    *t = now;
                    (*s, b.clone())
                })
                .collect();
            for (seq, body) in resend {
                self.send_input_packet(&mut st, seq, &body);
            }
            if matches!(route, Some(Route::Direct(_))) {
                return;
            }
            // On the relay: keep probing the direct candidates below, so the path moves over once one works.
        }
        // Bounded probing: 200 ms for 10 s, then 3 s until 60 s, then stop until the path is lost again.
        let Some(started) = st.probe_started else { return };
        let age = now.duration_since(started);
        if (self.e2e.is_some() || st.refreshed)
            && st.signal_resends < SIGNAL_RESENDS
            && age >= SIGNAL_RESEND_EVERY * (st.signal_resends + 1)
        {
            st.signal_resends += 1;
            self.send_signal(&[], &st.local_cands, st.refreshed);
        }
        let interval = if age < Duration::from_secs(10) {
            Duration::from_millis(200)
        } else if age < Duration::from_secs(60) {
            Duration::from_secs(3)
        } else {
            return;
        };
        if now.duration_since(st.last_probe) < interval {
            return;
        }
        st.last_probe = now;
        let mut targets = st.peer_cands.clone();
        if let Some(a) = st.peer_addr.filter(|a| !targets.contains(a)) {
            targets.push(a);
        }
        // UDP found nobody yet: also try TCP to the same addresses (K3), which many office and hotel networks allow.
        if age >= TCP_AFTER && tcp_path::enabled() {
            self.sock.tcp().open(&targets);
        }
        for to in targets {
            self.send_plain(&mut st, to, &[K_PROBE]);
        }
    }

    /// Binds to the UDP relay (and keeps the binding), and probes the partner through it while no path is up.
    fn maintain_relay(&self, st: &mut State, now: Instant) {
        let done = matches!(st.hs, Some(Handshake::Done(_)));
        let alive = Self::alive(st);
        let Some(r) = st.relay.as_mut() else { return };
        let every = if r.bound { RELAY_REBIND } else { RELAY_BIND_RETRY };
        if r.last_bind.is_none_or(|t| now.duration_since(t) >= every) {
            r.last_bind = Some(now);
            let mut bind = vec![RELAY_TAG, RELAY_BIND];
            bind.extend_from_slice(&r.token);
            if let Some(cookie) = r.cookie {
                bind.extend_from_slice(&cookie);
            }
            let _ = self.sock.send_to(&bind, r.addr);
        }
        if done && r.bound && !alive && r.last_probe.is_none_or(|t| now.duration_since(t) >= RELAY_PROBE) {
            r.last_probe = Some(now);
            let to = r.addr;
            self.send_plain(st, to, &[K_PROBE]);
        }
    }

    /// True while the path goes straight to the partner (not through the UDP relay): new file transfers take it.
    pub fn is_direct(&self) -> bool {
        matches!(self.route(), Some(Route::Direct(_)))
    }
}
