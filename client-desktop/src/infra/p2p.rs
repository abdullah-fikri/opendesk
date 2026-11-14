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
use crate::infra::send_gate::{SendGate, VideoLink};
use crate::infra::udp_socket::{self, PathSocket};
use crate::protocol::{ClientMessage, InputEvent, Outgoing};

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

fn parse_token(hex: &str) -> Option<[u8; 16]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
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

/// Sliding anti-replay window over explicit nonces (bit = nonce % window).
struct Replay {
    top: Option<u64>,
    bits: [u64; (REPLAY_WINDOW / 64) as usize],
}

impl Replay {
    fn new() -> Self {
        Self { top: None, bits: [0; (REPLAY_WINDOW / 64) as usize] }
    }
    fn bit(n: u64) -> (usize, u64) {
        let i = n % REPLAY_WINDOW;
        ((i / 64) as usize, 1u64 << (i % 64))
    }
    fn check(&self, n: u64) -> bool {
        match self.top {
            None => true,
            Some(top) if n > top => true,
            Some(top) if top - n >= REPLAY_WINDOW => false,
            Some(_) => {
                let (w, m) = Self::bit(n);
                self.bits[w] & m == 0
            }
        }
    }
    /// Call only after the packet authenticated.
    fn mark(&mut self, n: u64) {
        let top = self.top.unwrap_or(n);
        if self.top.is_none() || n > top {
            if self.top.is_none() || n - top >= REPLAY_WINDOW {
                self.bits = [0; (REPLAY_WINDOW / 64) as usize];
            } else {
                for k in top + 1..n {
                    let (w, m) = Self::bit(k);
                    self.bits[w] &= !m;
                }
                let (w, m) = Self::bit(n);
                self.bits[w] &= !m;
            }
            self.top = Some(n);
        }
        let (w, m) = Self::bit(n);
        self.bits[w] |= m;
    }
}

fn seal(t: &snow::StatelessTransportState, nonce: u64, plain: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![0u8; HEADER + plain.len() + TAG];
    out[0] = MAGIC;
    out[1..HEADER].copy_from_slice(&nonce.to_be_bytes());
    let n = t.write_message(nonce, plain, &mut out[HEADER..]).ok()?;
    out.truncate(HEADER + n);
    Some(out)
}

fn open(t: &snow::StatelessTransportState, replay: &mut Replay, pkt: &[u8]) -> Option<Vec<u8>> {
    if pkt.len() < HEADER + TAG || pkt.len() > MAX_DATAGRAM || pkt[0] != MAGIC {
        return None;
    }
    let nonce = u64::from_be_bytes(pkt[1..HEADER].try_into().ok()?);
    // Nonces above the P2P domain belong to relay-path streams sealed with the same keys (`infra::e2e`).
    if nonce >= P2P_NONCE_LIMIT || !replay.check(nonce) {
        return None;
    }
    let mut plain = vec![0u8; pkt.len() - HEADER];
    let n = t.read_message(nonce, &pkt[HEADER..], &mut plain).ok()?;
    replay.mark(nonce);
    plain.truncate(n);
    Some(plain)
}

/// Only public or private IPv4 unicast and global IPv6 on non-system ports, so a hostile candidate list cannot aim
/// probes at local services.
fn valid_candidate(addr: &SocketAddr) -> bool {
    let v4 = match addr {
        SocketAddr::V4(v4) => v4,
        SocketAddr::V6(v6) => return v6.port() >= 1024 && udp_socket::is_global_v6(v6.ip()),
    };
    let ip = v4.ip();
    v4.port() >= 1024
        && !(ip.is_unspecified()
            || ip.is_loopback()
            || ip.is_multicast()
            || ip.is_broadcast()
            || ip.is_documentation()
            || ip.is_link_local())
}

/// The candidates of a signal (`"c"`), filtered and capped.
fn parse_candidates(v: &Value) -> Vec<SocketAddr> {
    v.get("c")
        .and_then(|c| c.as_array())
        .map(|list| {
            list.iter().filter_map(|s| s.as_str()?.parse().ok()).filter(valid_candidate).take(MAX_CANDIDATES).collect()
        })
        .unwrap_or_default()
}

/// A `p2p-signal` payload: v1 carries this path's `Noise_NN` message, v2 (with e2e) only candidates. A refresh
/// (`"r": 1`, after a session resume) only replaces the candidates of a path whose handshake is done.
fn signal_json(e2e: bool, hs_msg: &[u8], cands: &[SocketAddr], refresh: bool) -> String {
    let c: Vec<String> = cands.iter().map(|a| a.to_string()).collect();
    let v = if refresh {
        json!({ "v": if e2e { 2 } else { 1 }, "r": 1, "c": c })
    } else if e2e {
        json!({ "v": 2, "c": c })
    } else {
        json!({ "v": 1, "hs": to_base64(hs_msg), "c": c })
    };
    v.to_string()
}

/// The new candidates of a refresh signal; None for any other signal.
fn refresh_candidates(candidate: &str) -> Option<Vec<SocketAddr>> {
    let v = serde_json::from_str::<Value>(candidate).ok()?;
    (v.get("r").and_then(Value::as_u64) == Some(1)).then(|| parse_candidates(&v))
}

/// Builds a STUN Binding request (RFC 5389).
fn stun_request(txid: &[u8; 12]) -> [u8; 20] {
    let mut req = [0u8; 20];
    req[0..2].copy_from_slice(&0x0001u16.to_be_bytes());
    req[4..8].copy_from_slice(&0x2112_A442u32.to_be_bytes());
    req[8..20].copy_from_slice(txid);
    req
}

/// Extracts the IPv4 XOR-MAPPED-ADDRESS from a Binding success response matching `txid`.
fn parse_stun_response(buf: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if buf.len() < 20 || buf[0..2] != [0x01, 0x01] || buf[4..8] != 0x2112_A442u32.to_be_bytes() || &buf[8..20] != txid {
        return None;
    }
    let end = (20 + u16::from_be_bytes([buf[2], buf[3]]) as usize).min(buf.len());
    let mut i = 20;
    while i + 4 <= end {
        let kind = u16::from_be_bytes([buf[i], buf[i + 1]]);
        let len = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
        let v = buf.get(i + 4..i + 4 + len)?;
        if kind == 0x0020 && len == 8 && v[1] == 0x01 {
            let port = u16::from_be_bytes([v[2], v[3]]) ^ 0x2112;
            let ip = u32::from_be_bytes([v[4], v[5], v[6], v[7]]) ^ 0x2112_A442;
            return Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), port)));
        }
        i += 4 + ((len + 3) & !3);
    }
    None
}

fn stun_query(sock: &PathSocket, server: &str) -> Option<SocketAddr> {
    let addr = server.to_socket_addrs().ok()?.find(|a| a.is_ipv4())?;
    let mut txid = [0u8; 12];
    getrandom::fill(&mut txid).ok()?;
    let request = stun_request(&txid);
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut buf = [0u8; 512];
    // One lost datagram must not cost the public candidate: the request goes out again every 250 ms.
    let mut resend = Instant::now();
    while Instant::now() < deadline {
        if Instant::now() >= resend {
            sock.send_to(&request, addr).ok()?;
            resend = Instant::now() + Duration::from_millis(250);
        }
        let wait = resend.min(deadline).saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
        let _ = sock.set_read_timeout(Some(wait));
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if from == addr => {
                if let Some(mapped) = parse_stun_response(&buf[..n], &txid) {
                    return Some(mapped);
                }
            }
            _ => {}
        }
    }
    None
}

fn gather_candidates(sock: &PathSocket) -> Vec<SocketAddr> {
    let port = sock.port();
    let mut cands = Vec::new();
    // IPv6 first: most home ISPs hand out global addresses without NAT, so the peers reach each other directly.
    if sock.is_dual()
        && let Some(ip) = udp_socket::global_ipv6()
    {
        cands.push(SocketAddr::from((ip, port)));
    }
    // shortcut: only the primary interface (route to a public address); no packet is sent by connect().
    if let Ok(probe) = UdpSocket::bind("0.0.0.0:0")
        && probe.connect("8.8.8.8:80").is_ok()
        && let Ok(SocketAddr::V4(local)) = probe.local_addr()
    {
        cands.push(SocketAddr::V4(SocketAddrV4::new(*local.ip(), port)));
    }
    let server = std::env::var("OPENDESK_STUN").unwrap_or_else(|_| "stun.l.google.com:19302".into());
    if server.is_empty() || server == "off" {
        return cands;
    }
    // A second server tells the NAT type: a different public port per destination (symmetric NAT, common
    // behind carrier-grade NAT) means the peer's probes cannot reach the address the first server saw. It also
    // stands in when the first one does not answer.
    match (stun_query(sock, &server), stun_query(sock, STUN_CHECK)) {
        (Some(mapped), check) => {
            let nat = match check {
                Some(other) if other == mapped => "endpoint-independent (hole punching can work)".to_string(),
                Some(other) => format!("symmetric: {STUN_CHECK} saw {other} (hole punching will likely fail)"),
                None => "unknown (second STUN server did not answer)".to_string(),
            };
            logger::info("anynet.p2p", &format!("STUN mapped {mapped}; NAT {nat}."));
            cands.push(mapped);
        }
        (None, Some(mapped)) => {
            logger::warn("anynet.p2p", &format!("STUN via {server} failed; {STUN_CHECK} mapped {mapped}."));
            cands.push(mapped);
        }
        (None, None) => logger::warn("anynet.p2p", "STUN failed on both servers; only the LAN address is offered."),
    }
    cands.dedup();
    cands.retain(valid_candidate);
    cands
}

/// Latest-frame-wins reassembly: a newer frame id discards the incomplete older one.
struct Reasm {
    id: u32,
    parts: Vec<Option<Vec<u8>>>,
    got: usize,
}

impl Reasm {
    fn push(slot: &mut Option<Reasm>, id: u32, idx: usize, count: usize, data: &[u8]) -> Option<Vec<u8>> {
        if count == 0 || count > MAX_FRAGS || idx >= count {
            return None;
        }
        let newer = slot.as_ref().is_none_or(|r| (id.wrapping_sub(r.id) as i32) > 0);
        if newer {
            *slot = Some(Reasm { id, parts: vec![None; count], got: 0 });
        }
        let r = slot.as_mut()?;
        if r.id != id || r.parts.len() != count {
            return None;
        }
        if r.parts[idx].is_none() {
            r.parts[idx] = Some(data.to_vec());
            r.got += 1;
        }
        if r.got < count {
            return None;
        }
        let frame = slot.take()?.parts.into_iter().flatten().flatten().collect();
        Some(frame)
    }
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

    /// Handles a `p2p-signal` payload from the session partner (the relay already checked the pairing).
    pub fn on_signal(&self, candidate: &str) {
        let mut st = self.state.lock();
        if let Some(cands) = refresh_candidates(candidate) {
            if matches!(st.hs, Some(Handshake::Done(_))) {
                logger::info("anynet.p2p", &format!("Peer refreshed its candidates ({}); probing.", cands.len()));
                st.peer_cands = cands;
                st.probe_started = Some(Instant::now());
            }
            return;
        }
        if st.signal_handled {
            return;
        }
        if st.hs.is_none() {
            st.pending_signal = Some(candidate.to_string());
            return;
        }
        st.signal_handled = true;
        self.handle_signal(&mut st, candidate);
    }

    fn handle_signal(&self, st: &mut State, candidate: &str) {
        let Ok(v) = serde_json::from_str::<Value>(candidate) else { return };
        let hs_msg = v.get("hs").and_then(|h| h.as_str()).and_then(from_base64);
        // v1: Noise_NN message and candidates; v2 (with e2e): candidates only.
        let version = if self.e2e.is_some() { 2 } else { 1 };
        if v.get("v").and_then(|x| x.as_u64()) != Some(version)
            || (version == 1 && hs_msg.as_ref().is_none_or(|m| m.len() > 256))
        {
            return;
        }
        st.peer_cands = parse_candidates(&v);
        let list: Vec<String> = st.peer_cands.iter().map(|a| a.to_string()).collect();
        logger::info("anynet.p2p", &format!("Peer offered {} candidates: [{}].", list.len(), list.join(", ")));
        if version == 2 {
            return; // probing starts once the shared handshake is done (`tick`)
        }
        let Some(Handshake::Pending(mut hs)) = st.hs.take() else { return };
        let mut buf = [0u8; 256];
        let msg = hs_msg.unwrap();
        let reply = if self.is_host {
            hs.read_message(&msg, &mut buf).ok().and_then(|_| hs.write_message(&[], &mut buf).ok())
        } else {
            hs.read_message(&msg, &mut buf).ok().map(|_| 0)
        };
        let reply_bytes = reply.map(|n| buf[..n].to_vec());
        st.hs = match (reply_bytes, hs.into_stateless_transport_mode()) {
            (Some(r), Ok(t)) => {
                if self.is_host {
                    self.send_signal(&r, &st.local_cands, false);
                }
                st.probe_started = Some(Instant::now());
                logger::info("anynet.p2p", &format!("Handshake complete; probing {} candidates.", st.peer_cands.len()));
                Some(Handshake::Done(Arc::new(t)))
            }
            _ => {
                logger::warn("anynet.p2p", "Handshake failed; staying on relay.");
                Some(Handshake::Failed)
            }
        };
    }

    fn send_signal(&self, hs_msg: &[u8], cands: &[SocketAddr], refresh: bool) {
        let candidate = signal_json(self.e2e.is_some(), hs_msg, cands, refresh);
        let msg = ClientMessage::P2pSignal { target_clean: self.partner.clone(), candidate };
        let _ = self.tx_out.send(Outgoing::Message(msg));
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

    /// Gathers the candidates again (the network may have changed while the relay connection was down) and
    /// sends them as a refresh. The path is down meanwhile, so the datagrams STUN skips cost nothing.
    fn regather_candidates(&self) {
        let cands = gather_candidates(&self.sock);
        let _ = self.sock.set_read_timeout(Some(Duration::from_millis(50)));
        let mut st = self.state.lock();
        if !matches!(st.hs, Some(Handshake::Done(_))) {
            return;
        }
        logger::info("anynet.p2p", &format!("Session resumed: sending {} fresh candidates.", cands.len()));
        self.send_signal(&[], &cands, true);
        st.local_cands = cands;
        st.refreshed = true;
        st.signal_resends = 0;
        st.probe_started = Some(Instant::now());
    }

    /// Starts this path's own `Noise_NN` handshake (partners without `e2e`). False if it cannot start.
    fn start_nn(&self, cands: &[SocketAddr]) -> bool {
        let builder = || snow::Builder::new(NOISE.parse().unwrap()).prologue(&self.prologue);
        let hs = builder().and_then(|b| if self.is_host { b.build_responder() } else { b.build_initiator() });
        let mut st = self.state.lock();
        st.local_cands = cands.to_vec();
        match hs {
            Ok(mut hs) => {
                if !self.is_host {
                    let mut buf = [0u8; 256];
                    match hs.write_message(&[], &mut buf) {
                        Ok(n) => self.send_signal(&buf[..n], cands, false),
                        Err(_) => return false,
                    }
                }
                st.hs = Some(Handshake::Pending(hs));
            }
            Err(_) => return false,
        }
        if let Some(sig) = st.pending_signal.take() {
            st.signal_handled = true;
            self.handle_signal(&mut st, &sig);
        }
        true
    }

    fn send_plain(&self, st: &mut State, to: SocketAddr, plain: &[u8]) -> bool {
        let nonce = st.tx_nonce;
        st.tx_nonce += 1;
        let Some(Handshake::Done(t)) = &st.hs else { return false };
        match seal(t, nonce, plain) {
            Some(pkt) => self.sock.send_to(&pkt, to).is_ok(),
            None => false,
        }
    }

    fn send_to_peer(&self, st: &mut State, plain: &[u8]) -> bool {
        match Self::route_of(st) {
            Some(route) => self.send_plain(st, route.addr(), plain),
            None => false,
        }
    }

    fn on_datagram(&self, pkt: &[u8], from: SocketAddr) {
        let mut st = self.state.lock();
        let via_relay = st.relay.as_ref().is_some_and(|r| r.addr == from);
        if via_relay && pkt.first() == Some(&RELAY_TAG) {
            let Some(r) = st.relay.as_mut() else { return };
            if pkt == [RELAY_TAG, RELAY_BOUND] && !r.bound {
                r.bound = true;
                logger::info("anynet.p2p", &format!("Bound to the UDP relay at {from}."));
            } else if pkt.len() == 2 + RELAY_COOKIE && pkt[1] == RELAY_CHALLENGE {
                r.cookie = pkt[2..].try_into().ok();
                r.last_bind = None; // answer on the next tick
            }
            return;
        }
        st.rx_datagrams += 1;
        st.first_from.get_or_insert(from);
        let plain = {
            let State { hs, replay, .. } = &mut *st;
            let opened = match hs {
                Some(Handshake::Done(t)) => open(t, replay, pkt),
                _ => None,
            };
            match opened {
                Some(p) => p,
                None => {
                    // unauthenticated, replayed, malformed or early: never affects state
                    st.rx_rejected += 1;
                    return;
                }
            }
        };
        let Some((&kind, body)) = plain.split_first() else { return };
        if via_relay {
            st.relay_rx = Some(Instant::now());
        } else {
            st.peer_addr = Some(from);
            st.direct_rx = Some(Instant::now());
        }
        match kind {
            K_PROBE => {
                self.send_plain(&mut st, from, &[K_PROBE_ACK]);
            }
            K_VIDEO if !self.is_host && body.len() > 8 => {
                let id = u32::from_be_bytes(body[0..4].try_into().unwrap());
                let idx = u16::from_be_bytes([body[4], body[5]]) as usize;
                let count = u16::from_be_bytes([body[6], body[7]]) as usize;
                if let Some(loss) = &self.sinks.loss
                    && count > 0
                    && count <= MAX_FRAGS
                    && idx < count
                {
                    loss.lock().fragment(id, count);
                }
                if self.sinks.nack {
                    let pushed = st.nack_reasm.push(Instant::now(), id, idx, count, &body[8..], restarts_chain);
                    self.deliver(&mut st, pushed);
                } else if let Some(frame) = Reasm::push(&mut st.reasm, id, idx, count, &body[8..]) {
                    (self.sinks.video)(frame);
                    let mut ack = vec![K_FRAME_ACK];
                    ack.extend_from_slice(&id.to_be_bytes());
                    self.send_to_peer(&mut st, &ack);
                }
            }
            K_NACK if self.is_host => {
                let Some((id, missing)) = nack::decode_nack(body) else { return };
                let frame =
                    st.sent_frames.iter().find(|(f, _)| *f == id).map(|(_, pkts)| {
                        missing.iter().filter_map(|&i| pkts.get(i as usize).cloned()).collect::<Vec<_>>()
                    });
                if let Some(pkts) = frame {
                    self.video_link.stats.lock().resent += pkts.len() as u64;
                    for pkt in pkts {
                        self.send_to_peer(&mut st, &pkt);
                    }
                }
            }
            K_FRAME_ACK if self.is_host && body.len() == 4 => {
                let id = u32::from_be_bytes(body.try_into().unwrap());
                // An acknowledgement also covers every older frame (their acks may have been lost).
                if let Some(&(_, sent)) = st.frame_wait.iter().find(|(w, _)| *w == id) {
                    let rtt = sent.elapsed();
                    self.video_link.stats.lock().ack_rtt.record(rtt);
                    st.ack_rtt.sample(rtt);
                    st.frame_wait.retain(|(w, _)| (w.wrapping_sub(id) as i32) > 0);
                    if st.frame_wait.len() < st.window {
                        self.can_send_frame.open();
                    }
                }
            }
            K_AUDIO => {
                if self.sinks.audio_gate.as_ref().is_none_or(|g| g.load(Ordering::Relaxed)) {
                    self.sinks.voice.play_binary_packet(body);
                }
            }
            K_INPUT if self.is_host && body.len() > 4 => {
                let seq = u32::from_be_bytes(body[0..4].try_into().unwrap());
                if seq >= st.rx_next && seq - st.rx_next < MAX_UNACKED as u32 {
                    st.rx_buf.insert(seq, body[4..].to_vec());
                }
                while let Some(ev) = {
                    let next = st.rx_next;
                    st.rx_buf.remove(&next)
                } {
                    st.rx_next += 1;
                    // Decoded locally, so a peer can only ever produce input events.
                    if let Some(event) = serde_json::from_slice::<Value>(&ev).ok().as_ref().and_then(InputEvent::parse)
                    {
                        let _ = self.sinks.input.send(event);
                    }
                }
                let mut ack = vec![K_INPUT_ACK];
                ack.extend_from_slice(&st.rx_next.to_be_bytes());
                self.send_to_peer(&mut st, &ack);
            }
            K_BULK if body.len() > 5 => {
                let Some(sink) = self.sinks.bulk.as_ref() else { return };
                let seq = u32::from_be_bytes(body[0..4].try_into().unwrap());
                let got = st.bulk_rx.on_segment(seq, body[4], &body[5..]);
                for (kind, payload) in got.messages {
                    self.bulk_pending.fetch_add(payload.len(), Ordering::SeqCst);
                    sink(kind, payload);
                }
                if got.ack {
                    let room = BULK_BUFFER.saturating_sub(self.bulk_pending.load(Ordering::SeqCst)) / BULK_SEG;
                    let mut ack = vec![K_BULK_ACK];
                    ack.extend_from_slice(&st.bulk_rx.ack(room.min(bulk::MAX_REORDER as usize) as u32));
                    self.send_to_peer(&mut st, &ack);
                }
            }
            K_BULK_ACK => {
                if let Some((cum, rwnd, sack)) = bulk::parse_ack(body) {
                    st.bulk_tx.on_ack(Instant::now(), cum, rwnd, sack);
                    self.bulk_wake.notify_all();
                }
            }
            K_INPUT_ACK if !self.is_host && body.len() == 4 => {
                let next = u32::from_be_bytes(body.try_into().unwrap());
                while st.unacked.front().is_some_and(|(s, _, _)| *s < next) {
                    st.unacked.pop_front();
                }
            }
            _ => {} // probe-ack, keepalive, or a kind not meant for this role
        }
    }

    /// Viewer: hands reassembled frames to the decoder in order, acknowledges the newest, and asks for lost
    /// fragments.
    fn deliver(&self, st: &mut State, pushed: Pushed) {
        for frame in pushed.frames {
            (self.sinks.video)(frame);
        }
        if let Some(id) = pushed.last_id {
            let mut ack = vec![K_FRAME_ACK];
            ack.extend_from_slice(&id.to_be_bytes());
            self.send_to_peer(st, &ack);
        }
        for (id, missing) in pushed.nacks {
            let mut pkt = vec![K_NACK];
            pkt.extend_from_slice(&nack::encode_nack(id, &missing));
            self.send_to_peer(st, &pkt);
        }
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

    /// Sends one JPEG over P2P. Returns false (caller uses the relay) if the path is down or the frame is too large.
    pub fn send_video(&self, jpeg: &[u8]) -> bool {
        let started = Instant::now();
        let mut st = self.state.lock();
        let count = jpeg.len().div_ceil(FRAG_DATA);
        if !Self::alive(&st) || count == 0 || count > MAX_FRAGS {
            return false;
        }
        // The viewer keeps only the newest frame: the previous frame's paced rest must arrive before this one.
        self.send_paced(&mut st, None);
        st.frame_id = st.frame_id.wrapping_add(1);
        let id = st.frame_id;
        let plan = if self.pacing {
            pacing::plan(count, MAX_DATAGRAM, self.video_link.target_fps(), self.video_link.kbps())
        } else {
            Pacing::burst(count)
        };
        let mut paced = VecDeque::new();
        let mut kept = Vec::with_capacity(count);
        let mut pkt = Vec::with_capacity(MAX_PLAIN);
        for (idx, chunk) in jpeg.chunks(FRAG_DATA).enumerate() {
            pkt.clear();
            pkt.push(K_VIDEO);
            pkt.extend_from_slice(&id.to_be_bytes());
            pkt.extend_from_slice(&(idx as u16).to_be_bytes());
            pkt.extend_from_slice(&(count as u16).to_be_bytes());
            pkt.extend_from_slice(chunk);
            kept.push(pkt.clone());
            if idx < plan.burst {
                self.send_to_peer(&mut st, &pkt);
            } else {
                paced.push_back((started + plan.offset(idx), pkt.clone()));
            }
        }
        // Up to `window` frames in flight; past that the viewer's frame-ack (or a timeout) releases the capture
        // loop. The window follows the stream's rate and the measured round trip, and the capture loop's
        // congestion level shrinks it back to 2.
        if self.dynamic_window {
            let link = &self.video_link;
            st.window = udp_window::frames_in_flight(st.ack_rtt.get(), link.target_fps(), link.congestion());
        }
        // Kept for retransmits; a viewer that does not ask never costs more than these few frames.
        st.sent_frames.push_back((id, kept));
        if st.sent_frames.len() > NACK_HISTORY {
            st.sent_frames.pop_front();
        }
        st.frame_wait.push_back((id, started));
        if st.frame_wait.len() < st.window {
            self.can_send_frame.open();
        }
        let mut link = self.video_link.stats.lock();
        link.window = Some(st.window);
        if paced.is_empty() {
            link.send.record(started.elapsed());
        } else {
            st.paced = Some(Paced { started, pending: paced });
            self.pace_wake.notify_one();
        }
        true
    }

    /// Sends the paced fragments that are due by `until` (all with None); records the frame's send time once
    /// its last fragment left.
    fn send_paced(&self, st: &mut State, until: Option<Instant>) {
        let Some(mut p) = st.paced.take() else { return };
        while let Some((at, _)) = p.pending.front() {
            if until.is_some_and(|u| *at > u) {
                break;
            }
            if let Some((_, pkt)) = p.pending.pop_front() {
                self.send_to_peer(st, &pkt);
            }
        }
        if p.pending.is_empty() {
            self.video_link.stats.lock().send.record(p.started.elapsed());
        } else {
            st.paced = Some(p);
        }
    }

    /// The pacer thread (host side, pacing on): sends each paced fragment when it is due. It sleeps with the
    /// state unlocked, so input, audio and acknowledgements are not held up; `std` sleeps on a high-resolution
    /// timer, and fragments that came due meanwhile leave together.
    fn pace(&self) {
        let mut st = self.state.lock();
        while !self.closed.load(Ordering::SeqCst) {
            let Some(due) = st.paced.as_ref().and_then(|p| p.pending.front()).map(|f| f.0) else {
                // The timeout notices a closed path.
                self.pace_wake.wait_for(&mut st, Duration::from_millis(100));
                continue;
            };
            let now = Instant::now();
            if due > now {
                MutexGuard::unlocked(&mut st, || std::thread::sleep(due - now));
                continue;
            }
            self.send_paced(&mut st, Some(now));
        }
    }

    /// True while the path goes straight to the partner (not through the UDP relay): new file transfers take it.
    pub fn is_direct(&self) -> bool {
        matches!(self.route(), Some(Route::Direct(_)))
    }

    /// Queues one file stream message (`file-p2p`); `done` runs once the partner acknowledged it, or with false
    /// if the path dropped first. False when the path is down (`done` then ran with false already).
    pub fn send_bulk(&self, kind: u8, payload: &[u8], done: Option<OnDelivered>) -> bool {
        let mut st = self.state.lock();
        if self.closed.load(Ordering::SeqCst) || Self::route_of(&st).is_none() {
            if let Some(done) = done {
                done(false);
            }
            return false;
        }
        st.bulk_tx.push(kind, payload, done);
        self.bulk_wake.notify_all();
        true
    }

    /// The connection processed `bytes` of received file stream payload: the advertised window grows back.
    pub fn bulk_consumed(&self, bytes: usize) {
        let _ = self.bulk_pending.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |p| Some(p.saturating_sub(bytes)));
    }

    /// Ends both directions of the file stream after the path dropped: unacknowledged messages fail and the
    /// connection learns that what it was receiving is cut off.
    fn drop_bulk(&self, st: &mut State) {
        let sending = !st.bulk_tx.is_idle();
        st.bulk_tx.abort();
        st.bulk_tx = BulkSender::new(BULK_SEG);
        st.bulk_rx = BulkReceiver::default();
        if sending {
            logger::warn("anynet.p2p", "P2P path lost during a file transfer; the transfer is cancelled.");
        }
        if let Some(sink) = &self.sinks.bulk {
            sink(BULK_LOST, Vec::new());
        }
    }

    /// File stream sender: segments leave as the window allows, paced at the window's rate so a burst never
    /// queues ahead of video; overdue ones are resent.
    fn run_bulk(&self) {
        let mut st = self.state.lock();
        while !self.closed.load(Ordering::SeqCst) {
            if st.bulk_tx.is_idle() || Self::route_of(&st).is_none() {
                self.bulk_wake.wait_for(&mut st, Duration::from_millis(100));
                continue;
            }
            let now = Instant::now();
            st.bulk_tx.on_timer(now);
            if st.bulk_due > now {
                let wait = st.bulk_due - now;
                MutexGuard::unlocked(&mut st, || std::thread::sleep(wait));
                continue;
            }
            // Up to 1 ms of the window's rate per wake-up; one segment at a time before a round trip is known.
            let burst = st.bulk_tx.rate().map_or(1, |r| ((r * 0.001) / BULK_SEG as f64).ceil().max(1.0) as usize);
            let mut sent = 0;
            while sent < burst {
                let Some((seq, flags, data)) = st.bulk_tx.next(now) else { break };
                let mut pkt = Vec::with_capacity(6 + data.len());
                pkt.push(K_BULK);
                pkt.extend_from_slice(&seq.to_be_bytes());
                pkt.push(flags);
                pkt.extend_from_slice(&data);
                self.send_to_peer(&mut st, &pkt);
                sent += 1;
            }
            if sent == 0 {
                // Window full: an acknowledgement wakes this thread; the timeout catches losses.
                self.bulk_wake.wait_for(&mut st, Duration::from_millis(5));
                continue;
            }
            let rate = st.bulk_tx.rate().unwrap_or(f64::MAX);
            let spacing =
                Duration::from_secs_f64((sent * BULK_SEG) as f64 / (rate * 1.25)).min(Duration::from_millis(20));
            st.bulk_due = now + spacing;
        }
    }

    /// Sends a `[0x02, rate_khz, pcm...]` packet, split at even byte boundaries to fit one datagram each; an Opus
    /// packet (voice or host sound) goes whole in one datagram.
    pub fn send_audio(&self, pkt: &[u8]) -> bool {
        let mut st = self.state.lock();
        if !Self::alive(&st) {
            return false;
        }
        use crate::infra::audio::opus;
        if opus::parse(pkt).is_some() || opus::parse_host_sound(pkt).is_some() {
            if pkt.len() + 1 > MAX_PLAIN {
                return false;
            }
            let mut plain = Vec::with_capacity(1 + pkt.len());
            plain.push(K_AUDIO);
            plain.extend_from_slice(pkt);
            self.send_to_peer(&mut st, &plain);
            return true;
        }
        if pkt.len() < 4 || !pkt.len().is_multiple_of(2) || pkt[0] != 0x02 {
            return false;
        }
        for chunk in pkt[2..].chunks((MAX_PLAIN - 3) & !1) {
            let mut plain = Vec::with_capacity(3 + chunk.len());
            plain.extend_from_slice(&[K_AUDIO, 0x02, pkt[1]]);
            plain.extend_from_slice(chunk);
            self.send_to_peer(&mut st, &plain);
        }
        true
    }

    /// Queues one input event on the reliable, ordered channel. Returns false if it must go over the relay instead.
    pub fn send_input(&self, event: &InputEvent) -> bool {
        // The input channel only runs viewer -> host (the viewer drops K_INPUT); host events such as
        // cursor-shape or monitors must take the relay.
        if self.is_host {
            return false;
        }
        let Ok(body) = serde_json::to_vec(event) else { return false };
        let mut st = self.state.lock();
        if !Self::alive(&st) || body.len() + 5 > MAX_PLAIN || st.unacked.len() >= MAX_UNACKED {
            return false;
        }
        let seq = st.next_seq;
        st.next_seq += 1;
        st.unacked.push_back((seq, body.clone(), Instant::now()));
        self.send_input_packet(&mut st, seq, &body);
        true
    }

    fn send_input_packet(&self, st: &mut State, seq: u32, body: &[u8]) {
        let mut plain = Vec::with_capacity(5 + body.len());
        plain.push(K_INPUT);
        plain.extend_from_slice(&seq.to_be_bytes());
        plain.extend_from_slice(body);
        self.send_to_peer(st, &plain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn pair(prologue: &[u8]) -> (snow::StatelessTransportState, snow::StatelessTransportState) {
        let b = || snow::Builder::new(NOISE.parse().unwrap()).prologue(prologue).unwrap();
        let mut i = b().build_initiator().unwrap();
        let mut r = b().build_responder().unwrap();
        let (mut m, mut p) = ([0u8; 256], [0u8; 256]);
        let n = i.write_message(&[], &mut m).unwrap();
        r.read_message(&m[..n], &mut p).unwrap();
        let n = r.write_message(&[], &mut m).unwrap();
        i.read_message(&m[..n], &mut p).unwrap();
        (i.into_stateless_transport_mode().unwrap(), r.into_stateless_transport_mode().unwrap())
    }

    #[test]
    fn refresh_signal_carries_only_candidates() {
        let cands: Vec<SocketAddr> = vec!["203.0.113.9:40000".parse().unwrap(), "192.168.1.5:40000".parse().unwrap()];
        for e2e in [false, true] {
            let sig = signal_json(e2e, b"hs", &cands, true);
            let v: Value = serde_json::from_str(&sig).unwrap();
            assert!(v.get("hs").is_none() && !crate::infra::e2e::is_signal(&v));
            // 203.0.113.0/24 is a documentation range, never probed.
            assert_eq!(refresh_candidates(&sig), Some(vec![cands[1]]));
            assert_eq!(refresh_candidates(&signal_json(e2e, b"hs", &cands, false)), None);
        }
        assert_eq!(refresh_candidates("not json"), None);
    }

    #[test]
    fn transport_rejects_replay_tampering_and_foreign_keys() {
        let (viewer, host) = pair(b"opendesk-p2p-v1|1|2");
        let (stranger, _) = pair(b"opendesk-p2p-v1|1|2");
        let mut replay = Replay::new();
        let pkt = seal(&viewer, 7, b"\x05hello").unwrap();
        assert!(pkt.len() <= MAX_DATAGRAM);
        assert_eq!(open(&host, &mut replay, &pkt).unwrap(), b"\x05hello");
        assert!(open(&host, &mut replay, &pkt).is_none(), "replay accepted");
        let mut bad = seal(&viewer, 8, b"\x05hello").unwrap();
        *bad.last_mut().unwrap() ^= 1;
        assert!(open(&host, &mut replay, &bad).is_none(), "tampered packet accepted");
        assert!(open(&host, &mut replay, &seal(&stranger, 9, b"\x05x").unwrap()).is_none(), "foreign key accepted");
        assert!(
            open(&host, &mut replay, &seal(&viewer, 5, b"\x05late").unwrap()).is_some(),
            "in-window reorder rejected"
        );
    }

    #[test]
    fn relay_path_nonces_never_open_as_datagrams() {
        let (viewer, host) = pair(b"opendesk-p2p-v1|1|2");
        let mut replay = Replay::new();
        // Same keys, relay-path domain: a relay forwarding a sealed relay message as a datagram gets nowhere.
        assert!(open(&host, &mut replay, &seal(&viewer, P2P_NONCE_LIMIT + 1, b"\x05x").unwrap()).is_none());
        assert!(open(&host, &mut replay, &seal(&viewer, 1, b"\x05x").unwrap()).is_some());
    }

    #[test]
    fn replay_window_tracks_order_and_age() {
        let mut r = Replay::new();
        for n in [10, 12, 11] {
            assert!(r.check(n));
            r.mark(n);
        }
        assert!(!r.check(11) && !r.check(12));
        r.mark(10 + REPLAY_WINDOW + 5);
        assert!(!r.check(10), "too old");
        assert!(r.check(10 + REPLAY_WINDOW + 4));
    }

    #[test]
    fn candidates_reject_local_services_and_special_addresses() {
        for bad in [
            "127.0.0.1:5000",
            "0.0.0.0:5000",
            "224.0.0.1:5000",
            "255.255.255.255:5000",
            "192.168.1.2:80",
            "169.254.1.1:5000",
            "[::1]:5000",
            "[fe80::1]:5000",
            "[2a02:1810::1]:80",
            "[::ffff:8.8.8.8]:5000",
        ] {
            assert!(!valid_candidate(&bad.parse().unwrap()), "{bad}");
        }
        assert!(valid_candidate(&"192.168.1.2:50000".parse().unwrap()));
        assert!(valid_candidate(&"8.8.4.4:50000".parse().unwrap()));
        assert!(valid_candidate(&"[2a02:1810::1]:50000".parse().unwrap()));
    }

    #[test]
    fn stun_parses_xor_mapped_address_for_matching_transaction_only() {
        let txid = [7u8; 12];
        let mut resp = vec![0x01, 0x01, 0, 12];
        resp.extend_from_slice(&0x2112_A442u32.to_be_bytes());
        resp.extend_from_slice(&txid);
        resp.extend_from_slice(&[0x00, 0x20, 0, 8, 0, 0x01]);
        resp.extend_from_slice(&(50000u16 ^ 0x2112).to_be_bytes());
        resp.extend_from_slice(&(u32::from(Ipv4Addr::new(203, 0, 113, 7)) ^ 0x2112_A442).to_be_bytes());
        assert_eq!(parse_stun_response(&resp, &txid), Some("203.0.113.7:50000".parse().unwrap()));
        assert_eq!(parse_stun_response(&resp, &[8u8; 12]), None);
    }

    /// Real UDP over this machine's LAN address; ignored by default because it needs a network route.
    #[test]
    #[ignore]
    fn end_to_end_video_and_input_over_lan_udp() {
        // A 60 FPS stream of unknown bitrate: the 150 KB frame below is paced over ~8 ms.
        let link = Arc::new(VideoLink::default());
        link.set_rate(60, 0.0);
        // SAFETY: set before any thread in this test reads the environment.
        unsafe { std::env::set_var("OPENDESK_STUN", "off") };
        let (tx_v, rx_v) = std::sync::mpsc::channel();
        let (tx_h, rx_h) = std::sync::mpsc::channel();
        let (in_tx, in_rx) = std::sync::mpsc::channel();
        let voice = VoiceEngine::new(tx_v.clone());
        let inbox: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let sink = Arc::clone(&inbox);
        let mk = |input: Sender<InputEvent>, video: Arc<dyn Fn(Vec<u8>) + Send + Sync>| Sinks {
            video,
            voice: Arc::clone(&voice),
            input,
            audio_gate: None,
            loss: None,
            nack: true,
            bulk: None,
        };
        let (dummy, _keep) = std::sync::mpsc::channel();
        let viewer = P2p::start(
            false,
            "111111111",
            "222222222".into(),
            Arc::new(SendGate::new(true)),
            Arc::default(),
            tx_v,
            mk(dummy, Arc::new(move |f| sink.lock().push(f))),
            None,
        )
        .unwrap();
        let host = P2p::start(
            true,
            "222222222",
            "111111111".into(),
            Arc::new(SendGate::new(true)),
            link,
            tx_h,
            mk(in_tx, Arc::new(|_| {})),
            None,
        )
        .unwrap();
        let signal = |rx: &std::sync::mpsc::Receiver<Outgoing>| loop {
            if let Ok(Outgoing::Message(ClientMessage::P2pSignal { candidate, .. })) =
                rx.recv_timeout(Duration::from_secs(5))
            {
                break candidate;
            }
        };
        host.on_signal(&signal(&rx_v));
        viewer.on_signal(&signal(&rx_h));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(viewer.is_active() && host.is_active()) {
            assert!(Instant::now() < deadline, "path never became active");
            thread::sleep(Duration::from_millis(20));
        }
        let jpeg: Vec<u8> = (0..150_000u32).map(|i| i as u8).collect();
        assert!(host.send_video(&jpeg));
        let deadline = Instant::now() + Duration::from_secs(2);
        while inbox.lock().is_empty() {
            assert!(Instant::now() < deadline, "frame not delivered");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(inbox.lock().pop().unwrap(), jpeg);
        assert!(viewer.send_input(&InputEvent::KeyDown { key_code: 65 }));
        assert_eq!(in_rx.recv_timeout(Duration::from_secs(2)).unwrap(), InputEvent::KeyDown { key_code: 65 });
        viewer.close();
        host.close();
    }

    /// A stand-in for the relay server's UDP forwarder (`server-relay/internal/udprelay`) on loopback. With
    /// `drop_every` > 0 it loses every n-th full-size datagram (video fragments) it forwards.
    fn fake_udp_relay(tokens: [[u8; 16]; 2], stop: Arc<AtomicBool>, drop_every: u32) -> SocketAddr {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        thread::spawn(move || {
            let mut legs: [Option<SocketAddr>; 2] = [None, None];
            let mut big = 0u32;
            let mut buf = [0u8; 2048];
            while !stop.load(Ordering::SeqCst) {
                let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
                let pkt = &buf[..n];
                // The cookie stands for "sent to this address": the port, repeated.
                let cookie = [from.port().to_be_bytes(); 4].concat();
                let token = |t: &[u8]| tokens.iter().position(|k| k[..] == *t);
                if n == 18 && pkt[..2] == [RELAY_TAG, RELAY_BIND] && token(&pkt[2..]).is_some() {
                    let _ = sock.send_to(&[&[RELAY_TAG, RELAY_CHALLENGE][..], &cookie].concat(), from);
                } else if n == 26
                    && pkt[..2] == [RELAY_TAG, RELAY_BIND]
                    && pkt[18..] == cookie[..]
                    && let Some(i) = token(&pkt[2..18])
                {
                    legs[i] = Some(from);
                    let _ = sock.send_to(&[RELAY_TAG, RELAY_BOUND], from);
                } else if pkt[0] == MAGIC
                    && let Some(i) = legs.iter().position(|l| *l == Some(from))
                    && let Some(to) = legs[1 - i]
                {
                    if n >= 1000 && drop_every > 0 {
                        big += 1;
                        if big.is_multiple_of(drop_every) {
                            continue;
                        }
                    }
                    let _ = sock.send_to(pkt, to);
                }
            }
        });
        addr
    }

    /// Two partners that cannot reach each other (no candidates) meet through the UDP relay.
    #[test]
    fn path_runs_through_the_udp_relay_without_candidates() {
        relayed_session(None);
    }

    /// The same against the real Go forwarder: `OPENDESK_TEST_UDP_RELAY=<addr>,<viewer token>,<host token>` from
    /// an allocation of `server-relay/internal/udprelay`.
    #[test]
    #[ignore]
    fn path_runs_through_the_go_udp_relay() {
        let raw = std::env::var("OPENDESK_TEST_UDP_RELAY").expect("OPENDESK_TEST_UDP_RELAY");
        let mut parts = raw.split(',').map(str::to_string);
        let mut next = || parts.next().expect("addr,viewer token,host token");
        relayed_session(Some((next(), next(), next())));
    }

    /// Runs a session with no direct candidates through `external` (address, viewer token, host token), or through
    /// [`fake_udp_relay`], which it then stops to check the fallback.
    fn relayed_session(external: Option<(String, String, String)>) {
        // SAFETY: set before any thread in this test reads the environment.
        unsafe { std::env::set_var("OPENDESK_STUN", "off") };
        let (tx_v, rx_v) = std::sync::mpsc::channel();
        let (tx_h, rx_h) = std::sync::mpsc::channel();
        let (in_tx, in_rx) = std::sync::mpsc::channel();
        let voice = VoiceEngine::new(tx_v.clone());
        let inbox: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let sink = Arc::clone(&inbox);
        let mk = |input: Sender<InputEvent>, video: Arc<dyn Fn(Vec<u8>) + Send + Sync>| Sinks {
            video,
            voice: Arc::clone(&voice),
            input,
            audio_gate: None,
            loss: None,
            nack: true,
            bulk: None,
        };
        let (dummy, _keep) = std::sync::mpsc::channel();
        let viewer = P2p::start(
            false,
            "111111111",
            "222222222".into(),
            Arc::new(SendGate::new(true)),
            Arc::default(),
            tx_v,
            mk(dummy, Arc::new(move |f| sink.lock().push(f))),
            None,
        )
        .unwrap();
        let host = P2p::start(
            true,
            "222222222",
            "111111111".into(),
            Arc::new(SendGate::new(true)),
            Arc::default(),
            tx_h,
            mk(in_tx, Arc::new(|_| {})),
            None,
        )
        .unwrap();
        // The handshake goes through, the candidates do not: no direct path is possible.
        let signal = |rx: &std::sync::mpsc::Receiver<Outgoing>| loop {
            if let Ok(Outgoing::Message(ClientMessage::P2pSignal { candidate, .. })) =
                rx.recv_timeout(Duration::from_secs(5))
            {
                let mut v: Value = serde_json::from_str(&candidate).unwrap();
                v["c"] = json!([]);
                break v.to_string();
            }
        };
        host.on_signal(&signal(&rx_v));
        viewer.on_signal(&signal(&rx_h));

        let stop = Arc::new(AtomicBool::new(false));
        let (relay, token_v, token_h) = external.clone().unwrap_or_else(|| {
            let addr = fake_udp_relay([[1u8; 16], [2u8; 16]], Arc::clone(&stop), 0);
            (addr.to_string(), "01".repeat(16), "02".repeat(16))
        });
        viewer.add_relay(&relay, &token_v);
        host.add_relay(&relay, &token_h);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(viewer.is_active() && host.is_active()) {
            assert!(Instant::now() < deadline, "path never came up through the relay");
            thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(viewer.route(), Some(Route::Relayed(a)) if a.to_string() == relay));
        assert!(matches!(host.route(), Some(Route::Relayed(_))));

        let jpeg: Vec<u8> = (0..150_000u32).map(|i| i as u8).collect();
        assert!(host.send_video(&jpeg));
        let deadline = Instant::now() + Duration::from_secs(2);
        while inbox.lock().is_empty() {
            assert!(Instant::now() < deadline, "frame not delivered");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(inbox.lock().pop().unwrap(), jpeg);
        assert!(viewer.send_input(&InputEvent::KeyDown { key_code: 65 }));
        assert_eq!(in_rx.recv_timeout(Duration::from_secs(2)).unwrap(), InputEvent::KeyDown { key_code: 65 });

        if external.is_some() {
            viewer.close();
            host.close();
            return;
        }
        // The relay goes away: the path is lost and the session falls back to the WebSocket relay.
        stop.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + PATH_TIMEOUT + Duration::from_secs(2);
        while viewer.is_active() {
            assert!(Instant::now() < deadline, "path stayed up without the relay");
            thread::sleep(Duration::from_millis(50));
        }
        viewer.close();
        host.close();
    }

    type Bulk = Option<BulkSink>;

    /// A viewer and a host whose path runs through a fake UDP relay that loses every `drop_every`-th full-size
    /// datagram; the returned flag stops the relay.
    fn relayed_pair(
        nack: bool,
        drop_every: u32,
        video: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
        viewer_bulk: Bulk,
        host_bulk: Bulk,
    ) -> (Arc<P2p>, Arc<P2p>, Arc<AtomicBool>) {
        // SAFETY: set before any thread in this test reads the environment.
        unsafe { std::env::set_var("OPENDESK_STUN", "off") };
        let (tx_v, rx_v) = std::sync::mpsc::channel();
        let (tx_h, rx_h) = std::sync::mpsc::channel();
        let (in_tx, _in_rx) = std::sync::mpsc::channel();
        let voice = VoiceEngine::new(tx_v.clone());
        let mk = |input: Sender<InputEvent>, video: Arc<dyn Fn(Vec<u8>) + Send + Sync>, nack: bool, bulk: Bulk| Sinks {
            video,
            voice: Arc::clone(&voice),
            input,
            audio_gate: None,
            loss: None,
            nack,
            bulk,
        };
        let (dummy, _keep) = std::sync::mpsc::channel();
        let viewer = P2p::start(
            false,
            "111111111",
            "222222222".into(),
            Arc::new(SendGate::new(true)),
            Arc::default(),
            tx_v,
            mk(dummy, video, nack, viewer_bulk),
            None,
        )
        .unwrap();
        let host = P2p::start(
            true,
            "222222222",
            "111111111".into(),
            Arc::new(SendGate::new(true)),
            Arc::default(),
            tx_h,
            mk(in_tx, Arc::new(|_| {}), false, host_bulk),
            None,
        )
        .unwrap();
        let signal = |rx: &std::sync::mpsc::Receiver<Outgoing>| loop {
            if let Ok(Outgoing::Message(ClientMessage::P2pSignal { candidate, .. })) =
                rx.recv_timeout(Duration::from_secs(5))
            {
                let mut v: Value = serde_json::from_str(&candidate).unwrap();
                v["c"] = json!([]);
                break v.to_string();
            }
        };
        host.on_signal(&signal(&rx_v));
        viewer.on_signal(&signal(&rx_h));
        let stop = Arc::new(AtomicBool::new(false));
        let relay = fake_udp_relay([[1u8; 16], [2u8; 16]], Arc::clone(&stop), drop_every).to_string();
        viewer.add_relay(&relay, &"01".repeat(16));
        host.add_relay(&relay, &"02".repeat(16));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(viewer.is_active() && host.is_active()) {
            assert!(Instant::now() < deadline, "path never came up through the relay");
            thread::sleep(Duration::from_millis(20));
        }
        (viewer, host, stop)
    }

    /// Frames through a path that loses every `drop_every`-th video fragment; returns how many of `frames`
    /// arrived intact, and whether they arrived in sending order.
    fn lossy_stream(nack: bool, drop_every: u32, frames: usize) -> (usize, bool) {
        let inbox: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let sink = Arc::clone(&inbox);
        let (viewer, host, stop) = relayed_pair(nack, drop_every, Arc::new(move |f| sink.lock().push(f)), None, None);
        // Each frame starts with its number, so order and integrity can be checked.
        let frame = |n: usize| -> Vec<u8> { (0..150_000u32).map(|i| (i as usize * 7 + n) as u8).collect() };
        for n in 0..frames {
            assert!(host.send_video(&frame(n)));
            thread::sleep(Duration::from_millis(30));
        }
        thread::sleep(Duration::from_millis(500));
        let got = inbox.lock().clone();
        let ids: Vec<usize> = got.iter().filter_map(|f| (0..frames).find(|&n| *f == frame(n))).collect();
        stop.store(true, Ordering::SeqCst);
        viewer.close();
        host.close();
        (ids.len(), ids.windows(2).all(|w| w[0] < w[1]) && ids.len() == got.len())
    }

    type Message = (u8, Vec<u8>);

    /// Sends `count` messages of `size` bytes from the viewer to the host over a path losing every
    /// `drop_every`-th full datagram; returns whether all arrived whole and in order, how many were acknowledged
    /// and the throughput in MB/s.
    fn file_stream(drop_every: u32, count: usize, size: usize) -> (bool, usize, f64) {
        let got: Arc<Mutex<Vec<Message>>> = Arc::default();
        let sink = Arc::clone(&got);
        let host_bulk: Bulk = Some(Arc::new(move |kind, payload| sink.lock().push((kind, payload))));
        let (viewer, host, stop) = relayed_pair(false, drop_every, Arc::new(|_| {}), None, host_bulk);
        let msg = |n: usize| -> Vec<u8> { (0..size).map(|i| (i * 13 + n) as u8).collect() };
        let acked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let started = Instant::now();
        for n in 0..count {
            let a = Arc::clone(&acked);
            let done: OnDelivered = Box::new(move |ok| {
                if ok {
                    a.fetch_add(1, Ordering::SeqCst);
                }
            });
            assert!(viewer.send_bulk(bulk::MSG_BINARY, &msg(n), Some(done)));
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while acked.load(Ordering::SeqCst) < count && Instant::now() < deadline {
            let n: usize = got.lock().iter().map(|(_, p)| p.len()).sum();
            host.bulk_consumed(n);
            thread::sleep(Duration::from_millis(2));
        }
        let secs = started.elapsed().as_secs_f64();
        let got = got.lock().clone();
        stop.store(true, Ordering::SeqCst);
        viewer.close();
        host.close();
        let whole =
            got.len() == count && got.iter().enumerate().all(|(n, (k, p))| *k == bulk::MSG_BINARY && *p == msg(n));
        (whole, acked.load(Ordering::SeqCst), (count * size) as f64 / secs / 1e6)
    }

    /// FN4: file stream messages arrive whole, in order and acknowledged, also with ~5% loss.
    #[test]
    fn file_stream_is_reliable_over_a_lossy_path() {
        let (whole, acked, mbps) = file_stream(0, 40, 128 * 1024);
        println!("loopback file stream: {mbps:.0} MB/s");
        assert!(whole && acked == 40, "clean path: whole={whole} acked={acked}");
        let (whole, acked, mbps) = file_stream(20, 16, 128 * 1024);
        println!("~5% loss: {mbps:.0} MB/s");
        assert!(whole && acked == 16, "lossy path: whole={whole} acked={acked}");
    }

    /// B12: with ~7% fragment loss a 150 KB frame (127 fragments) almost never arrives whole; with retransmit
    /// requests every frame does, in order.
    #[test]
    fn lost_fragments_are_resent_and_frames_stay_in_order() {
        let (without, _) = lossy_stream(false, 14, 20);
        let (with, ordered) = lossy_stream(true, 14, 20);
        println!("~7% fragment loss: {without}/20 frames without retransmits, {with}/20 with");
        assert!(without <= 2, "{without} frames survived without retransmits");
        assert!(with >= 19, "only {with} of 20 frames arrived with retransmits");
        assert!(ordered, "frames reached the decoder out of order or corrupt");
    }

    #[test]
    fn relay_tokens_parse_from_hex_only() {
        assert_eq!(parse_token(&"0a".repeat(16)), Some([10u8; 16]));
        assert_eq!(parse_token(&"0a".repeat(15)), None);
        assert_eq!(parse_token(&"zz".repeat(16)), None);
    }

    #[test]
    fn reassembly_keeps_latest_frame_only() {
        let mut slot = None;
        assert!(Reasm::push(&mut slot, 1, 0, 2, b"ab").is_none());
        assert!(Reasm::push(&mut slot, 2, 1, 2, b"cd").is_none()); // newer frame drops frame 1
        assert!(Reasm::push(&mut slot, 1, 1, 2, b"xx").is_none()); // stale fragment ignored
        assert_eq!(Reasm::push(&mut slot, 2, 0, 2, b"ab").unwrap(), b"abcd");
        assert!(Reasm::push(&mut slot, 3, 5, 2, b"ab").is_none()); // bad index
    }
}
