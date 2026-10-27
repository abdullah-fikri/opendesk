//! Direct UDP path between session partners. The relay stays the control plane: it authenticates
//! devices, approves the session and carries the Noise handshake; this module only moves media/input.
//! Wire format and security decisions are documented in `docs/features/p2p.md`.

use std::collections::{BTreeMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use core_engine::codec::{from_base64, to_base64};
use serde_json::{Value, json};

use crate::infra::audio::VoiceEngine;
use crate::infra::logger;
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
/// Video frames sent but not acknowledged yet. Two keep 60 FPS through a round trip of up to about one frame
/// interval; one would halve the rate as soon as the round trip plus encoding exceeds 16 ms.
const MAX_FRAMES_IN_FLIGHT: usize = 2;
const INPUT_RETRANSMIT: Duration = Duration::from_millis(80);
const MAX_UNACKED: usize = 256;

const K_PROBE: u8 = 1;
const K_PROBE_ACK: u8 = 2;
const K_VIDEO: u8 = 3;
const K_AUDIO: u8 = 4;
const K_INPUT: u8 = 5;
const K_INPUT_ACK: u8 = 6;
const K_FRAME_ACK: u8 = 7;
const K_KEEPALIVE: u8 = 8;

/// Client-side kill switch: `OPENDESK_P2P=0` keeps every session on the relay.
pub fn enabled() -> bool {
    std::env::var("OPENDESK_P2P").map(|v| v != "0").unwrap_or(true)
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
    if !replay.check(nonce) {
        return None;
    }
    let mut plain = vec![0u8; pkt.len() - HEADER];
    let n = t.read_message(nonce, &pkt[HEADER..], &mut plain).ok()?;
    replay.mark(nonce);
    plain.truncate(n);
    Some(plain)
}

/// Only public or private IPv4 unicast on non-system ports, so a hostile candidate list cannot aim probes at local services.
fn valid_candidate(addr: &SocketAddr) -> bool {
    let SocketAddr::V4(v4) = addr else { return false };
    let ip = v4.ip();
    v4.port() >= 1024
        && !(ip.is_unspecified()
            || ip.is_loopback()
            || ip.is_multicast()
            || ip.is_broadcast()
            || ip.is_documentation()
            || ip.is_link_local())
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

fn stun_query(sock: &UdpSocket, server: &str) -> Option<SocketAddr> {
    let addr = server.to_socket_addrs().ok()?.find(|a| a.is_ipv4())?;
    let mut txid = [0u8; 12];
    getrandom::fill(&mut txid).ok()?;
    sock.send_to(&stun_request(&txid), addr).ok()?;
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut buf = [0u8; 512];
    while Instant::now() < deadline {
        let _ = sock
            .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))));
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if from == addr => {
                if let Some(mapped) = parse_stun_response(&buf[..n], &txid) {
                    return Some(mapped);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    None
}

fn gather_candidates(sock: &UdpSocket) -> Vec<SocketAddr> {
    let port = sock.local_addr().map(|a| a.port()).unwrap_or(0);
    let mut cands = Vec::new();
    // shortcut: only the primary interface (route to a public address); no packet is sent by connect().
    if let Ok(probe) = UdpSocket::bind("0.0.0.0:0")
        && probe.connect("8.8.8.8:80").is_ok()
        && let Ok(SocketAddr::V4(local)) = probe.local_addr()
    {
        cands.push(SocketAddr::V4(SocketAddrV4::new(*local.ip(), port)));
    }
    let server = std::env::var("OPENDESK_STUN").unwrap_or_else(|_| "stun.l.google.com:19302".into());
    if !server.is_empty()
        && server != "off"
        && let Some(mapped) = stun_query(sock, &server)
        && !cands.contains(&mapped)
    {
        cands.push(mapped);
    }
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

// One handshake per path, replaced once on completion: the size difference between variants costs nothing.
#[allow(clippy::large_enum_variant)]
enum Handshake {
    Pending(snow::HandshakeState),
    Done(snow::StatelessTransportState),
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
    peer_addr: Option<SocketAddr>,
    last_rx: Option<Instant>,
    was_alive: bool,
    probe_started: Option<Instant>,
    last_probe: Instant,
    last_keepalive: Instant,
    frame_id: u32,
    /// Frames awaiting their acknowledgement, oldest first (at most `MAX_FRAMES_IN_FLIGHT`).
    frame_wait: VecDeque<(u32, Instant)>,
    reasm: Option<Reasm>,
    next_seq: u32,
    unacked: VecDeque<(u32, Vec<u8>, Instant)>,
    rx_next: u32,
    rx_buf: BTreeMap<u32, Vec<u8>>,
}

/// A direct encrypted UDP path to one session partner (see `docs/features/p2p.md`).
pub struct P2p {
    pub partner: String,
    is_host: bool,
    prologue: Vec<u8>,
    sock: UdpSocket,
    state: Mutex<State>,
    closed: AtomicBool,
    can_send_frame: Arc<AtomicBool>,
    tx_out: Sender<Outgoing>,
    sinks: Sinks,
}

impl P2p {
    /// Opens the UDP socket and starts candidate gathering in the background. Returns None if no socket is available.
    pub fn start(
        is_host: bool,
        local_clean: &str,
        partner: String,
        can_send_frame: Arc<AtomicBool>,
        tx_out: Sender<Outgoing>,
        sinks: Sinks,
    ) -> Option<Arc<P2p>> {
        let raw =
            socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP)).ok()?;
        // A 1080p JPEG is ~100-200 fragments sent back to back; the default 64 KB buffer would drop most of them.
        let _ = raw.set_recv_buffer_size(4 << 20);
        let _ = raw.set_send_buffer_size(4 << 20);
        raw.bind(&SocketAddr::from(([0, 0, 0, 0], 0)).into()).ok()?;
        let sock: UdpSocket = raw.into();
        let local: String = local_clean.chars().filter(|c| c.is_ascii_digit()).collect();
        let (viewer, host) = if is_host { (partner.clone(), local) } else { (local, partner.clone()) };
        let now = Instant::now();
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
                last_rx: None,
                was_alive: false,
                probe_started: None,
                last_probe: now,
                last_keepalive: now,
                frame_id: 0,
                frame_wait: VecDeque::new(),
                reasm: None,
                next_seq: 0,
                unacked: VecDeque::new(),
                rx_next: 0,
                rx_buf: BTreeMap::new(),
            }),
            closed: AtomicBool::new(false),
            can_send_frame,
            tx_out,
            sinks,
        });
        let worker = Arc::clone(&p);
        crate::infra::thread::spawn_named("p2p-worker", move || worker.run());
        Some(p)
    }

    /// Closes the path; the worker thread exits and traffic falls back to the relay.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.can_send_frame.store(true, Ordering::Release);
        }
    }

    /// True for the link this machine opened as the host of the session.
    pub fn is_host(&self) -> bool {
        self.is_host
    }

    /// True once the Noise handshake has completed and the link is not closed.
    pub fn is_active(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && Self::alive(&self.state.lock().unwrap())
    }

    fn alive(st: &State) -> bool {
        matches!(st.hs, Some(Handshake::Done(_)))
            && st.peer_addr.is_some()
            && st.last_rx.is_some_and(|t| t.elapsed() < PATH_TIMEOUT)
    }

    /// Handles a `p2p-signal` payload from the session partner (the relay already checked the pairing).
    pub fn on_signal(&self, candidate: &str) {
        let mut st = self.state.lock().unwrap();
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
        if v.get("v").and_then(|x| x.as_u64()) != Some(1) || hs_msg.as_ref().is_none_or(|m| m.len() > 256) {
            return;
        }
        st.peer_cands = v
            .get("c")
            .and_then(|c| c.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|s| s.as_str()?.parse().ok())
                    .filter(valid_candidate)
                    .take(MAX_CANDIDATES)
                    .collect()
            })
            .unwrap_or_default();
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
                    self.send_signal(&r, &st.local_cands);
                }
                st.probe_started = Some(Instant::now());
                logger::info("anynet.p2p", &format!("Handshake complete; probing {} candidates.", st.peer_cands.len()));
                Some(Handshake::Done(t))
            }
            _ => {
                logger::warn("anynet.p2p", "Handshake failed; staying on relay.");
                Some(Handshake::Failed)
            }
        };
    }

    fn send_signal(&self, hs_msg: &[u8], cands: &[SocketAddr]) {
        let c: Vec<String> = cands.iter().map(|a| a.to_string()).collect();
        let candidate = json!({ "v": 1, "hs": to_base64(hs_msg), "c": c }).to_string();
        let msg = ClientMessage::P2pSignal { target_clean: self.partner.clone(), candidate };
        let _ = self.tx_out.send(Outgoing::Message(msg));
    }

    fn run(self: Arc<Self>) {
        let cands = gather_candidates(&self.sock);
        let builder = || snow::Builder::new(NOISE.parse().unwrap()).prologue(&self.prologue);
        let hs = builder().and_then(|b| if self.is_host { b.build_responder() } else { b.build_initiator() });
        {
            let mut st = self.state.lock().unwrap();
            st.local_cands = cands.clone();
            match hs {
                Ok(mut hs) => {
                    if !self.is_host {
                        let mut buf = [0u8; 256];
                        match hs.write_message(&[], &mut buf) {
                            Ok(n) => self.send_signal(&buf[..n], &cands),
                            Err(_) => return,
                        }
                    }
                    st.hs = Some(Handshake::Pending(hs));
                }
                Err(_) => return,
            }
            if let Some(sig) = st.pending_signal.take() {
                st.signal_handled = true;
                self.handle_signal(&mut st, &sig);
            }
        }
        logger::info("anynet.p2p", &format!("UDP ready with {} local candidates.", cands.len()));

        let _ = self.sock.set_read_timeout(Some(Duration::from_millis(50)));
        let mut buf = [0u8; 2048];
        while !self.closed.load(Ordering::SeqCst) {
            if let Ok((n, from)) = self.sock.recv_from(&mut buf) {
                self.on_datagram(&buf[..n], from);
            }
            self.tick();
        }
        logger::info("anynet.p2p", "P2P path closed.");
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
        match st.peer_addr {
            Some(addr) => self.send_plain(st, addr, plain),
            None => false,
        }
    }

    fn on_datagram(&self, pkt: &[u8], from: SocketAddr) {
        let mut st = self.state.lock().unwrap();
        let plain = {
            let State { hs, replay, .. } = &mut *st;
            let Some(Handshake::Done(t)) = hs else { return };
            match open(t, replay, pkt) {
                Some(p) => p,
                None => return, // unauthenticated, replayed or malformed: never affects state
            }
        };
        let Some((&kind, body)) = plain.split_first() else { return };
        st.peer_addr = Some(from);
        st.last_rx = Some(Instant::now());
        match kind {
            K_PROBE => {
                self.send_plain(&mut st, from, &[K_PROBE_ACK]);
            }
            K_VIDEO if !self.is_host && body.len() > 8 => {
                let id = u32::from_be_bytes(body[0..4].try_into().unwrap());
                let idx = u16::from_be_bytes([body[4], body[5]]) as usize;
                let count = u16::from_be_bytes([body[6], body[7]]) as usize;
                if let Some(frame) = Reasm::push(&mut st.reasm, id, idx, count, &body[8..]) {
                    (self.sinks.video)(frame);
                    let mut ack = vec![K_FRAME_ACK];
                    ack.extend_from_slice(&id.to_be_bytes());
                    self.send_to_peer(&mut st, &ack);
                }
            }
            K_FRAME_ACK if self.is_host && body.len() == 4 => {
                let id = u32::from_be_bytes(body.try_into().unwrap());
                // An acknowledgement also covers every older frame (their acks may have been lost).
                if st.frame_wait.iter().any(|(w, _)| *w == id) {
                    st.frame_wait.retain(|(w, _)| (w.wrapping_sub(id) as i32) > 0);
                    self.can_send_frame.store(true, Ordering::Release);
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
            K_INPUT_ACK if !self.is_host && body.len() == 4 => {
                let next = u32::from_be_bytes(body.try_into().unwrap());
                while st.unacked.front().is_some_and(|(s, _, _)| *s < next) {
                    st.unacked.pop_front();
                }
            }
            _ => {} // probe-ack, keepalive, or a kind not meant for this role
        }
    }

    fn tick(&self) {
        let mut st = self.state.lock().unwrap();
        let now = Instant::now();
        let alive = Self::alive(&st);
        if alive != st.was_alive {
            st.was_alive = alive;
            logger::info(
                "anynet.p2p",
                if alive { "P2P path active." } else { "P2P path lost; falling back to relay." },
            );
            if !alive {
                st.probe_started = Some(now);
                st.frame_wait.clear();
                // shortcut: unacked input is dropped, not replayed on the relay, to avoid double clicks.
                st.unacked.clear();
                self.can_send_frame.store(true, Ordering::Release);
            }
        }
        if alive {
            if now.duration_since(st.last_keepalive) >= Duration::from_secs(1) {
                st.last_keepalive = now;
                self.send_to_peer(&mut st, &[K_KEEPALIVE]);
            }
            let waiting = st.frame_wait.len();
            st.frame_wait.retain(|(_, t)| now.duration_since(*t) < FRAME_ACK_TIMEOUT);
            if st.frame_wait.len() < waiting {
                self.can_send_frame.store(true, Ordering::Release);
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
            return;
        }
        // Bounded probing: 200 ms for 10 s, then 3 s until 60 s, then stop until the path is lost again.
        let Some(started) = st.probe_started else { return };
        let age = now.duration_since(started);
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

    /// Sends one JPEG over P2P. Returns false (caller uses the relay) if the path is down or the frame is too large.
    pub fn send_video(&self, jpeg: &[u8]) -> bool {
        let mut st = self.state.lock().unwrap();
        let count = jpeg.len().div_ceil(FRAG_DATA);
        if !Self::alive(&st) || count == 0 || count > MAX_FRAGS {
            return false;
        }
        st.frame_id = st.frame_id.wrapping_add(1);
        let id = st.frame_id;
        let mut pkt = Vec::with_capacity(MAX_PLAIN);
        for (idx, chunk) in jpeg.chunks(FRAG_DATA).enumerate() {
            pkt.clear();
            pkt.push(K_VIDEO);
            pkt.extend_from_slice(&id.to_be_bytes());
            pkt.extend_from_slice(&(idx as u16).to_be_bytes());
            pkt.extend_from_slice(&(count as u16).to_be_bytes());
            pkt.extend_from_slice(chunk);
            self.send_to_peer(&mut st, &pkt);
        }
        // Up to MAX_FRAMES_IN_FLIGHT frames in flight; past that the viewer's frame-ack (or a timeout) releases the
        // capture loop.
        st.frame_wait.push_back((id, Instant::now()));
        if st.frame_wait.len() < MAX_FRAMES_IN_FLIGHT {
            self.can_send_frame.store(true, Ordering::Release);
        }
        true
    }

    /// Sends a `[0x02, rate_khz, pcm...]` packet, split at even byte boundaries to fit one datagram each; an Opus
    /// packet goes whole in one datagram.
    pub fn send_audio(&self, pkt: &[u8]) -> bool {
        let mut st = self.state.lock().unwrap();
        if !Self::alive(&st) {
            return false;
        }
        if crate::infra::audio::opus::parse(pkt).is_some() {
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
        let mut st = self.state.lock().unwrap();
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
        ] {
            assert!(!valid_candidate(&bad.parse().unwrap()), "{bad}");
        }
        assert!(valid_candidate(&"192.168.1.2:50000".parse().unwrap()));
        assert!(valid_candidate(&"8.8.4.4:50000".parse().unwrap()));
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
        };
        let (dummy, _keep) = std::sync::mpsc::channel();
        let viewer = P2p::start(
            false,
            "111111111",
            "222222222".into(),
            Arc::new(AtomicBool::new(true)),
            tx_v,
            mk(dummy, Arc::new(move |f| sink.lock().unwrap().push(f))),
        )
        .unwrap();
        let host = P2p::start(
            true,
            "222222222",
            "111111111".into(),
            Arc::new(AtomicBool::new(true)),
            tx_h,
            mk(in_tx, Arc::new(|_| {})),
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
        while inbox.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "frame not delivered");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(inbox.lock().unwrap().pop().unwrap(), jpeg);
        assert!(viewer.send_input(&InputEvent::KeyDown { key_code: 65 }));
        assert_eq!(in_rx.recv_timeout(Duration::from_secs(2)).unwrap(), InputEvent::KeyDown { key_code: 65 });
        viewer.close();
        host.close();
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
