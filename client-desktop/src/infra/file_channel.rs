//! Direct file channel (`file-channel`): TCP connections of their own between the two peers of a session, only for
//! file transfers. The P2P path carries video and input in small paced datagrams and declares itself lost after a
//! few silent seconds; files on it had to crawl behind the video (a delay-based window aiming at 8 ms of queue)
//! and were cancelled whenever the path dropped. On this channel the operating system's TCP moves the bytes with
//! its own congestion control and buffers, as AnyDesk's file transfers do, and a busy transfer can never take the
//! session's path down.
//!
//! Setup: once the session's end-to-end handshake is done, each side listens on a fresh TCP port and sends its
//! candidates (LAN, global IPv6, public IPv4 with the same port) in a `p2p-signal`. Both sides connect to every
//! candidate of the other, from their listening port (a TCP simultaneous open gets through NATs that keep the
//! port) and accept. Each connection runs `Noise_XX` with the device keys; it is kept only if the partner's static
//! key is the one the session's end-to-end handshake authenticated. The viewer then picks the first such
//! connection (`KIND_SELECT`); every other one is dropped. Nothing is trusted before that: a stranger reaching the
//! port cannot complete the handshake with the right key.
//!
//! Striping (see `domain::file_channel`): when both sides can, the viewer then opens up to
//! [`wire::DEFAULT_STREAMS`] - 1 more connections to the address the picked one runs to (`KIND_JOIN`, each with
//! its own `Noise_XX` and key check). Messages go out on whichever stream is free first and are put back in order
//! on arrival. An extra stream that cannot connect (a NAT that only let the simultaneous open through) just leaves
//! the channel with fewer streams.
//!
//! Messages are the file stream messages of `domain::bulk` (see `domain::file_channel` for the framing), handed to
//! the same handler as those of the P2P path. A lost stream ends the whole channel and reports
//! [`super::p2p::BULK_LOST`], which cuts (and lets the sender resume) whatever was moving on it, exactly like a
//! lost P2P path. The relay connection then sets the channel up again ([`FileChannel::redial_due`]).
//!
//! `OPENDESK_FILE_CHANNEL=0` turns it off (not advertised, not used); `OPENDESK_FILE_STREAMS=N` (1..8) sets how
//! many streams this side offers, 1 turning striping off.

use std::collections::VecDeque;
use std::io::{self, BufWriter, Read, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};

use crate::domain::bulk::OnDelivered;
use crate::domain::file_channel::{
    self as wire, KIND_HELLO, KIND_JOIN, KIND_PING, KIND_SELECT, KIND_SEQ, Pick, Reader, Redial, Reorder,
};
use crate::infra::{logger, udp_socket};

const NOISE_XX: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Connect attempts per candidate (a simultaneous open may need a few to line up), each this long at most.
const CONNECT_TRIES: u32 = 6;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_RETRY: Duration = Duration::from_millis(700);
/// Connect attempts per extra stream: the address is known to work, so it either answers or never will.
const STREAM_TRIES: u32 = 2;
/// The whole handshake on one connection, and how long the host waits for the viewer's pick after it.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections being set up at once; more are refused (a port scan must not spawn threads without end).
const MAX_PENDING: usize = 16;
/// Bytes received but not yet handled by the relay connection thread; the readers stop reading above this, and
/// TCP then slows the sender down (a slow disk throttles the partner instead of filling memory).
const RECEIVE_WINDOW: usize = 32 << 20;
/// Bytes of a striped direction held back for an earlier message still on another stream. With the
/// [`RECEIVE_WINDOW`] this bounds what one channel holds in memory; a stream over it stops reading.
const REORDER_LIMIT: usize = 16 << 20;
/// Socket buffers: room for a long fat path (100 MB/s at 80 ms) on each stream.
const SOCKET_BUFFER: usize = 8 << 20;
/// Frames are gathered up to this much before a write: a 128 KiB chunk becomes one system call, not three.
const WRITE_BUFFER: usize = 256 << 10;
/// A keep-alive goes out after this much quiet; a connection silent for [`SILENCE`] is dead.
const PING_EVERY: Duration = Duration::from_secs(5);
const SILENCE: Duration = Duration::from_secs(25);

/// Client-side kill switch: `OPENDESK_FILE_CHANNEL=0` neither advertises nor uses the channel.
pub fn enabled() -> bool {
    std::env::var("OPENDESK_FILE_CHANNEL").map(|v| v != "0").unwrap_or(true)
}

/// Streams this side offers (`OPENDESK_FILE_STREAMS`, default 4).
pub fn streams() -> u8 {
    wire::streams_setting(std::env::var("OPENDESK_FILE_STREAMS").ok().as_deref())
}

/// Where received messages go (the relay connection thread), as for the P2P path's file stream.
pub type Sink = Arc<dyn Fn(u8, Vec<u8>) + Send + Sync>;

type Transport = Arc<snow::StatelessTransportState>;

enum Phase {
    /// Looking for a connection.
    Connecting,
    /// Connected: messages go to these streams.
    Up(Arc<Link>),
    /// Lost or closed. This instance is not used again; the relay connection starts a new one to redial.
    Down,
}

/// What the first message on a connection the host accepted says.
enum First {
    /// We are the viewer: we pick, nobody picks for us.
    Viewer,
    /// The viewer picked this connection.
    Pick(Pick),
    /// The viewer adds this connection as a stream of the channel its pick with this token opened.
    Join(u64),
}

/// One queued message and what to call once it is in the socket (or could not be).
struct Queued {
    /// Its place in this direction's order; None while not striping.
    seq: Option<u64>,
    kind: u8,
    payload: Vec<u8>,
    done: Option<OnDelivered>,
}

impl Queued {
    /// The message as it goes on the stream: its sequence number first when striping.
    fn bytes(&self) -> Vec<u8> {
        let body = wire::encode(self.kind, &self.payload);
        match self.seq {
            Some(seq) => {
                let mut out = wire::encode(KIND_SEQ, &wire::u64_payload(seq));
                out.extend_from_slice(&body);
                out
            }
            None => body,
        }
    }
}

/// What the writers take next.
enum Next {
    Send(Queued),
    Ping,
    Stop,
}

/// The outbound queue every stream's writer takes from.
struct Out {
    items: VecDeque<Queued>,
    /// The next sequence number; None while this side does not stripe toward the partner.
    seq: Option<u64>,
}

/// The streams of a channel that is up and what they share: one outbound queue (a free stream takes the next
/// message, so a stream slowed by a loss simply carries less) and the reordering of what arrives.
struct Link {
    /// Where the picked connection runs to; extra streams connect there.
    peer: SocketAddr,
    /// The viewer's pick token; an extra stream must present it.
    token: u64,
    /// Most streams: 1 until both sides said more (the host at the pick, the viewer at the host's hello).
    max_streams: AtomicU8,
    raws: Mutex<Vec<TcpStream>>,
    out: Mutex<Out>,
    out_ready: Condvar,
    reorder: Mutex<Reorder>,
    reordered: Condvar,
    dead: AtomicBool,
}

impl Link {
    fn new(peer: SocketAddr, token: u64, raw: TcpStream) -> Self {
        Self {
            peer,
            token,
            max_streams: AtomicU8::new(1),
            raws: Mutex::new(vec![raw]),
            out: Mutex::new(Out { items: VecDeque::new(), seq: None }),
            out_ready: Condvar::new(),
            reorder: Mutex::new(Reorder::new(REORDER_LIMIT)),
            reordered: Condvar::new(),
            dead: AtomicBool::new(false),
        }
    }

    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Queues a message; false (and `done(false)`) once the link is dead.
    fn push(&self, kind: u8, payload: Vec<u8>, done: Option<OnDelivered>) -> bool {
        let mut q = self.out.lock();
        if self.is_dead() {
            drop(q);
            if let Some(done) = done {
                done(false);
            }
            return false;
        }
        let seq = q.seq;
        if let Some(s) = seq {
            q.seq = Some(s + 1);
        }
        q.items.push_back(Queued { seq, kind, payload, done });
        drop(q);
        self.out_ready.notify_all();
        true
    }

    /// From now on messages to the partner carry sequence numbers and may take any stream.
    fn stripe(&self) {
        let mut q = self.out.lock();
        if q.seq.is_none() {
            q.seq = Some(0);
        }
    }

    /// The next message for a writer. Only the primary stream (the picked connection) takes messages without a
    /// sequence number and the first striped one: everything before striping began, and the message that begins
    /// it, then leave in queue order on one stream, so the receiver needs no sequence numbers to order them.
    fn next_for(&self, primary: bool) -> Next {
        let mut q = self.out.lock();
        let deadline = Instant::now() + PING_EVERY;
        loop {
            if self.is_dead() {
                return Next::Stop;
            }
            let mine = q.items.front().is_some_and(|m| primary || m.seq.is_some_and(|s| s > 0));
            if mine && let Some(m) = q.items.pop_front() {
                return Next::Send(m);
            }
            if self.out_ready.wait_until(&mut q, deadline).timed_out() {
                return Next::Ping;
            }
        }
    }

    /// Adds a stream unless the link is dead or has all it may have.
    fn add_stream(&self, raw: TcpStream) -> bool {
        let mut raws = self.raws.lock();
        if self.is_dead() || raws.len() >= self.max_streams.load(Ordering::SeqCst) as usize {
            return false;
        }
        raws.push(raw);
        true
    }

    fn streams(&self) -> usize {
        self.raws.lock().len()
    }

    /// Ends every stream; whatever is still queued will never leave.
    fn kill(&self) {
        self.dead.store(true, Ordering::SeqCst);
        for raw in self.raws.lock().iter() {
            let _ = raw.shutdown(std::net::Shutdown::Both);
        }
        let queued: Vec<Queued> = self.out.lock().items.drain(..).collect();
        for m in queued {
            if let Some(done) = m.done {
                done(false);
            }
        }
        self.out_ready.notify_all();
        self.reordered.notify_all();
    }
}

/// The file channel of one session.
pub struct FileChannel {
    pub partner: String,
    is_host: bool,
    secret: Vec<u8>,
    peer_key: Vec<u8>,
    prologue: String,
    listener: Option<TcpListener>,
    port: u16,
    dual: bool,
    /// Streams this side offers.
    streams: u8,
    phase: Mutex<Phase>,
    pending: Mutex<usize>,
    /// Received bytes not yet consumed (see [`RECEIVE_WINDOW`]).
    window: Mutex<usize>,
    window_freed: Condvar,
    sink: Sink,
    started: Instant,
    closed: AtomicBool,
    redial: Mutex<Redial>,
    /// The partner's latest candidates, so a channel set up again can reach a partner that already did.
    partner_peers: Mutex<Vec<SocketAddr>>,
}

impl FileChannel {
    /// Starts listening for `partner`. `secret` is this device's Noise key, `peer_key` the partner's static key the
    /// end-to-end handshake authenticated; `redial` is [`Redial::default`] for the session's first channel, else
    /// the lost one's [`FileChannel::next_redial`]. None when no port could be opened.
    pub fn start(
        is_host: bool,
        local_clean: &str,
        partner: &str,
        secret: &[u8],
        peer_key: &[u8],
        sink: Sink,
        redial: Redial,
    ) -> Option<Arc<Self>> {
        Self::open(is_host, local_clean, partner, secret, peer_key, sink, redial, streams())
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        is_host: bool,
        local_clean: &str,
        partner: &str,
        secret: &[u8],
        peer_key: &[u8],
        sink: Sink,
        redial: Redial,
        streams: u8,
    ) -> Option<Arc<Self>> {
        let local: String = local_clean.chars().filter(|c| c.is_ascii_digit()).collect();
        let (viewer, host) = if is_host { (partner, local.as_str()) } else { (local.as_str(), partner) };
        let (listener, port, dual) = listen()?;
        let me = Arc::new(Self {
            partner: partner.to_string(),
            is_host,
            secret: secret.to_vec(),
            peer_key: peer_key.to_vec(),
            prologue: wire::prologue(viewer, host),
            listener: listener.try_clone().ok(),
            port,
            dual,
            streams: streams.clamp(1, wire::MAX_STREAMS),
            phase: Mutex::new(Phase::Connecting),
            pending: Mutex::new(0),
            window: Mutex::new(0),
            window_freed: Condvar::new(),
            sink,
            started: Instant::now(),
            closed: AtomicBool::new(false),
            redial: Mutex::new(redial),
            partner_peers: Mutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&me);
        crate::infra::thread::spawn_named("file-channel-accept", move || accept_loop(listener, weak));
        Some(me)
    }

    /// This side's candidates: the listening port on the LAN address, the global IPv6 address and every public
    /// IPv4 address the P2P path learned (`public_ips`, from STUN), with the same port.
    pub fn candidates(&self, public_ips: &[std::net::IpAddr]) -> Vec<SocketAddr> {
        let mut out = Vec::new();
        if self.dual
            && let Some(ip) = udp_socket::global_ipv6()
        {
            out.push(SocketAddr::from((ip, self.port)));
        }
        // The interface that routes to the internet (no packet is sent by connect()).
        if let Ok(probe) = UdpSocket::bind("0.0.0.0:0")
            && probe.connect("8.8.8.8:80").is_ok()
            && let Ok(local) = probe.local_addr()
        {
            out.push(SocketAddr::new(local.ip(), self.port));
        }
        for ip in public_ips {
            out.push(SocketAddr::new(*ip, self.port));
        }
        out.dedup();
        out
    }

    /// The partner's candidates arrived: remember them and connect to each while still looking.
    pub fn connect_to(self: &Arc<Self>, peers: &[SocketAddr]) {
        peers.clone_into(&mut self.partner_peers.lock());
        if !self.connecting() {
            return;
        }
        let list: Vec<String> = peers.iter().map(|a| a.to_string()).collect();
        logger::info("file_channel", &format!("Partner {} offered [{}]; connecting.", self.partner, list.join(", ")));
        for &peer in peers {
            if peer.is_ipv6() && !self.dual {
                continue;
            }
            let me = Arc::clone(self);
            crate::infra::thread::spawn_named("file-channel-connect", move || {
                for attempt in 0..CONNECT_TRIES {
                    if !me.connecting() {
                        return;
                    }
                    match me.connect(peer) {
                        Ok(stream) => {
                            me.secure(stream, peer);
                            return;
                        }
                        Err(_) if attempt + 1 < CONNECT_TRIES => std::thread::sleep(CONNECT_RETRY),
                        Err(e) => logger::info("file_channel", &format!("No connection to {peer}: {e}.")),
                    }
                }
            });
        }
    }

    /// The partner's latest candidates (empty before its first signal).
    pub fn partner_peers(&self) -> Vec<SocketAddr> {
        self.partner_peers.lock().clone()
    }

    /// True while connected.
    pub fn is_up(&self) -> bool {
        matches!(*self.phase.lock(), Phase::Up(_))
    }

    /// Streams of a channel that is up (0 otherwise).
    #[cfg(test)]
    pub fn streams_up(&self) -> usize {
        match &*self.phase.lock() {
            Phase::Up(link) => link.streams(),
            _ => 0,
        }
    }

    /// Time to replace this lost channel by a new one. A channel set up again that did not come up within
    /// [`wire::REDIAL_CONNECT`] counts as lost here, so it is tried again later rather than waited for forever.
    pub fn redial_due(&self, now: Instant) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let mut phase = self.phase.lock();
        let mut redial = self.redial.lock();
        if matches!(*phase, Phase::Connecting)
            && redial.again
            && now.duration_since(self.started) >= wire::REDIAL_CONNECT
        {
            *phase = Phase::Down;
            redial.lost(now);
            logger::info("file_channel", &format!("File channel with {} did not come back up in time.", self.partner));
        }
        matches!(*phase, Phase::Down) && redial.due(now)
    }

    /// True while the channel was lost and is coming back: a try is scheduled or a new instance is connecting.
    pub fn redialing(&self) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        // Phase before redial, as everywhere.
        let phase = self.phase.lock();
        let redial = self.redial.lock();
        match *phase {
            Phase::Down => redial.pending(),
            Phase::Connecting => redial.again,
            Phase::Up(_) => false,
        }
    }

    /// This channel is lost and may be set up again at once: the partner set its side up again.
    pub fn may_answer(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
            && matches!(*self.phase.lock(), Phase::Down)
            && self.redial.lock().may_answer()
    }

    /// The redial state the channel replacing this one starts with.
    pub fn next_redial(&self) -> Redial {
        self.redial.lock().next_try()
    }

    /// Queues one file stream message; `done` runs once it is in the socket (true) or the channel is gone (false).
    /// False when the channel is not up (`done` then already ran with false). Never blocks: the senders bound what
    /// is in flight (`file_transfer::in_flight_limit`), and this runs on the relay connection thread.
    pub fn send(&self, kind: u8, payload: &[u8], done: Option<OnDelivered>) -> bool {
        let link = match &*self.phase.lock() {
            Phase::Up(link) => Some(Arc::clone(link)),
            _ => None,
        };
        match link {
            Some(link) => link.push(kind, payload.to_vec(), done),
            None => {
                if let Some(done) = done {
                    done(false);
                }
                false
            }
        }
    }

    /// The relay connection handled `bytes` of received payload: the readers may read on.
    pub fn consumed(&self, bytes: usize) {
        let mut w = self.window.lock();
        *w = w.saturating_sub(bytes);
        self.window_freed.notify_all();
    }

    /// Ends the channel (the session ended).
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let old = std::mem::replace(&mut *self.phase.lock(), Phase::Down);
        if let Phase::Up(link) = old {
            link.kill();
        }
        // Wake the accept loop so it sees `closed` and ends.
        if self.listener.is_some() {
            let to = if self.dual {
                SocketAddr::from((Ipv6Addr::LOCALHOST, self.port))
            } else {
                SocketAddr::from(([127, 0, 0, 1], self.port))
            };
            let _ = TcpStream::connect_timeout(&to, Duration::from_millis(200));
        }
        self.window_freed.notify_all();
    }

    fn connecting(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && matches!(*self.phase.lock(), Phase::Connecting)
    }

    /// Whether an incoming connection may be useful: while looking for the channel, or on the host while the
    /// viewer may still add streams.
    fn accepting(&self) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        match &*self.phase.lock() {
            Phase::Connecting => true,
            Phase::Up(link) => self.is_host && link.streams() < link.max_streams.load(Ordering::SeqCst) as usize,
            Phase::Down => false,
        }
    }

    fn socket(&self) -> io::Result<Socket> {
        let raw = Socket::new(if self.dual { Domain::IPV6 } else { Domain::IPV4 }, Type::STREAM, Some(Protocol::TCP))?;
        if self.dual {
            raw.set_only_v6(false)?;
        }
        raw.set_reuse_address(true)?;
        #[cfg(unix)]
        raw.set_reuse_port(true)?;
        let any: SocketAddr = if self.dual {
            SocketAddr::from((Ipv6Addr::UNSPECIFIED, self.port))
        } else {
            SocketAddr::from(([0, 0, 0, 0], self.port))
        };
        raw.bind(&any.into())?;
        Ok(raw)
    }

    /// Connects to `peer` from the listening port, so a partner doing the same meets us (simultaneous open).
    fn connect(&self, peer: SocketAddr) -> io::Result<TcpStream> {
        let raw = self.socket()?;
        let wire = match (peer, self.dual) {
            (SocketAddr::V4(v4), true) => SocketAddr::from((v4.ip().to_ipv6_mapped(), v4.port())),
            _ => peer,
        };
        raw.connect_timeout(&wire.into(), CONNECT_TIMEOUT)?;
        Ok(raw.into())
    }

    /// Runs the handshake on a fresh connection in its own thread; adopts it if it proves to be the partner and
    /// is the one picked (or, on the host, a stream the viewer adds).
    fn secure(self: &Arc<Self>, stream: TcpStream, peer: SocketAddr) {
        {
            let mut pending = self.pending.lock();
            if *pending >= MAX_PENDING || !self.accepting() {
                return;
            }
            *pending += 1;
        }
        let me = Arc::clone(self);
        crate::infra::thread::spawn_named("file-channel-handshake", move || {
            let result = me.handshake(&stream);
            *me.pending.lock() -= 1;
            match result {
                Ok((transport, First::Join(token))) => me.join(stream, transport, token),
                Ok((transport, First::Pick(pick))) => me.adopt(stream, peer, transport, pick),
                Ok((transport, First::Viewer)) => me.adopt(stream, peer, transport, Pick { streams: 1, token: 0 }),
                Err(e) => {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    if me.connecting() {
                        logger::info("file_channel", &format!("Connection with {peer} not used: {e}."));
                    }
                }
            }
        });
    }

    /// `Noise_XX` with the device keys; the viewer initiates. The partner must show the key the session's
    /// end-to-end handshake authenticated. The host then waits for the viewer's first message on this connection:
    /// its pick, or the join of an extra stream.
    fn handshake(&self, stream: &TcpStream) -> io::Result<(Transport, First)> {
        let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let builder = snow::Builder::new(NOISE_XX.parse().map_err(|_| bad("pattern"))?)
            .prologue(self.prologue.as_bytes())
            .and_then(|b| b.local_private_key(&self.secret))
            .map_err(|_| bad("keys"))?;
        let mut hs = if self.is_host { builder.build_responder() } else { builder.build_initiator() }
            .map_err(|_| bad("noise"))?;
        let (mut r, mut w) = (stream.try_clone()?, stream.try_clone()?);
        let mut buf = vec![0u8; 1024];
        // XX: -> e; <- e, ee, s, es; -> s, se.
        for step in 0..3 {
            let ours = (step % 2 == 0) != self.is_host;
            if ours {
                let n = hs.write_message(&[], &mut buf).map_err(|_| bad("handshake"))?;
                write_frame(&mut w, &buf[..n])?;
            } else {
                let msg = read_frame(&mut r)?;
                hs.read_message(&msg, &mut buf).map_err(|_| bad("handshake"))?;
            }
        }
        if hs.get_remote_static() != Some(self.peer_key.as_slice()) {
            return Err(bad("the partner's key differs from the session's"));
        }
        let transport = Arc::new(hs.into_stateless_transport_mode().map_err(|_| bad("noise"))?);
        if !self.is_host {
            return Ok((transport, First::Viewer));
        }
        // The viewer's first message (nonce 0): the pick, or the join of an extra stream.
        let frame = read_frame(&mut r)?;
        let mut plain = vec![0u8; frame.len()];
        let n = transport.read_message(0, &frame, &mut plain).map_err(|_| bad("pick"))?;
        let first = Reader::default().push(&plain[..n]).and_then(|m| m.into_iter().next());
        match first {
            Some((KIND_SELECT, payload)) => Ok((transport, First::Pick(Pick::parse(&payload)))),
            Some((KIND_JOIN, payload)) => {
                Ok((transport, First::Join(wire::u64_be(&payload).ok_or_else(|| bad("join"))?)))
            }
            _ => Err(bad("not picked")),
        }
    }

    /// Makes `stream` the channel unless another connection already is; the viewer announces its pick, a host
    /// that can stripe answers a viewer that can with its hello.
    fn adopt(self: &Arc<Self>, stream: TcpStream, peer: SocketAddr, transport: Transport, pick: Pick) {
        let Ok(raw) = stream.try_clone() else { return };
        let token = if self.is_host { pick.token } else { random_token() };
        let link = Arc::new(Link::new(peer, token, raw));
        {
            let mut phase = self.phase.lock();
            if !matches!(*phase, Phase::Connecting) || self.closed.load(Ordering::SeqCst) {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return;
            }
            *phase = Phase::Up(Arc::clone(&link));
        }
        self.redial.lock().up(Instant::now());
        tune(&stream);
        let _ = stream.set_read_timeout(Some(SILENCE));
        let _ = stream.set_write_timeout(Some(SILENCE));
        logger::info(
            "file_channel",
            &format!(
                "File channel with {} up: direct TCP via {peer} ({} ms after start).",
                self.partner,
                self.started.elapsed().as_millis()
            ),
        );
        let (Ok(read_half), Ok(mut write_half)) = (stream.try_clone(), stream.try_clone()) else { return self.lost() };
        // Nonces: the host already read the pick with 0, the viewer writes it now with 0.
        let (first_tx, first_rx) = if self.is_host { (0, 1) } else { (1, 0) };
        if self.is_host {
            let streams = self.streams.min(pick.streams);
            link.max_streams.store(streams, Ordering::SeqCst);
            // A viewer from before striping picks with an empty payload and gets neither.
            if pick.streams > 1 && self.streams > 1 {
                link.push(KIND_HELLO, vec![self.streams], None);
            }
            if streams > 1 {
                link.stripe();
            }
        } else {
            // A single-stream viewer picks like one from before striping: empty.
            let payload = if self.streams > 1 { Pick { streams: self.streams, token }.encode() } else { Vec::new() };
            if write_one(&mut write_half, &transport, 0, KIND_SELECT, &payload).is_err() {
                return self.lost();
            }
        }
        self.run_stream(&link, read_half, write_half, transport, first_tx, first_rx, true);
    }

    /// Host: the viewer adds `stream` to the channel its pick with `token` opened.
    fn join(self: &Arc<Self>, stream: TcpStream, transport: Transport, token: u64) {
        let link = match &*self.phase.lock() {
            Phase::Up(link) if link.token == token => Arc::clone(link),
            _ => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return;
            }
        };
        self.attach(&link, stream, transport, 0, 1);
    }

    /// Viewer: the host's hello says how many streams it takes; open the extra ones and stripe from now on.
    fn on_hello(self: &Arc<Self>, link: &Arc<Link>, payload: &[u8]) {
        let theirs = payload.first().copied().unwrap_or(1).clamp(1, wire::MAX_STREAMS);
        let streams = self.streams.min(theirs);
        link.max_streams.store(streams, Ordering::SeqCst);
        if streams < 2 {
            return;
        }
        link.stripe();
        logger::info(
            "file_channel",
            &format!("File channel with {}: opening {} more streams.", self.partner, streams - 1),
        );
        for _ in 1..streams {
            let (me, link) = (Arc::clone(self), Arc::clone(link));
            crate::infra::thread::spawn_named("file-channel-stream", move || me.open_stream(&link));
        }
    }

    /// Viewer: one extra stream to where the picked connection runs, from a port of its own.
    fn open_stream(self: &Arc<Self>, link: &Arc<Link>) {
        for attempt in 0..STREAM_TRIES {
            if link.is_dead() || self.closed.load(Ordering::SeqCst) {
                return;
            }
            let stream = match TcpStream::connect_timeout(&link.peer, CONNECT_TIMEOUT) {
                Ok(s) => s,
                Err(e) if attempt + 1 == STREAM_TRIES => {
                    logger::info("file_channel", &format!("No extra stream to {}: {e}.", link.peer));
                    return;
                }
                Err(_) => {
                    std::thread::sleep(CONNECT_RETRY);
                    continue;
                }
            };
            match self.handshake(&stream) {
                Ok((transport, _)) => {
                    let Ok(mut w) = stream.try_clone() else { return };
                    if write_one(&mut w, &transport, 0, KIND_JOIN, &wire::u64_payload(link.token)).is_ok() {
                        self.attach(link, stream, transport, 1, 0);
                    }
                }
                Err(e) => logger::info("file_channel", &format!("Extra stream to {} not used: {e}.", link.peer)),
            }
            return;
        }
    }

    /// Runs `stream` as one more stream of `link`.
    fn attach(self: &Arc<Self>, link: &Arc<Link>, stream: TcpStream, transport: Transport, tx: u64, rx: u64) {
        let (Ok(raw), Ok(r), Ok(w)) = (stream.try_clone(), stream.try_clone(), stream.try_clone()) else { return };
        if !link.add_stream(raw) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
        tune(&stream);
        let _ = stream.set_read_timeout(Some(SILENCE));
        let _ = stream.set_write_timeout(Some(SILENCE));
        logger::info("file_channel", &format!("File channel with {}: stream {} up.", self.partner, link.streams()));
        self.run_stream(link, r, w, transport, tx, rx, false);
    }

    /// Starts the writer and the reader of one stream.
    #[allow(clippy::too_many_arguments)]
    fn run_stream(
        self: &Arc<Self>,
        link: &Arc<Link>,
        read_half: TcpStream,
        write_half: TcpStream,
        transport: Transport,
        first_tx: u64,
        first_rx: u64,
        primary: bool,
    ) {
        let (me, l, t) = (Arc::clone(self), Arc::clone(link), Arc::clone(&transport));
        crate::infra::thread::spawn_named("file-channel-write", move || {
            me.write_loop(&l, write_half, &t, first_tx, primary)
        });
        let (me, l) = (Arc::clone(self), Arc::clone(link));
        crate::infra::thread::spawn_named("file-channel-read", move || {
            me.read_loop(&l, read_half, &transport, first_rx, primary)
        });
    }

    fn write_loop(&self, link: &Link, stream: TcpStream, t: &Transport, mut nonce: u64, primary: bool) {
        let mut out = BufWriter::with_capacity(WRITE_BUFFER, stream);
        let mut frame = vec![0u8; 2 + wire::SEGMENT + 16];
        loop {
            let (msg, done) = match link.next_for(primary) {
                Next::Stop => break,
                Next::Ping => (wire::encode(KIND_PING, &[]), None),
                Next::Send(m) => (m.bytes(), m.done),
            };
            let ok = write_segments(&mut out, t, &mut nonce, &mut frame, &msg).and_then(|()| out.flush()).is_ok();
            if let Some(done) = done {
                done(ok);
            }
            if !ok || self.closed.load(Ordering::SeqCst) {
                break;
            }
        }
        // A stream that cannot write ends the channel: its messages are gone, and the order with them.
        self.lost();
    }

    fn read_loop(self: &Arc<Self>, link: &Arc<Link>, mut stream: TcpStream, t: &Transport, first: u64, primary: bool) {
        let mut nonce = first;
        let mut reader = Reader::default();
        let mut plain = vec![0u8; wire::SEGMENT + 16];
        let mut seq = None;
        'read: while !self.closed.load(Ordering::SeqCst) && !link.is_dead() {
            let Ok(frame) = read_frame(&mut stream) else { break };
            let Ok(n) = t.read_message(nonce, &frame, &mut plain) else { break };
            nonce += 1;
            let Some(messages) = reader.push(&plain[..n]) else { break };
            for (kind, payload) in messages {
                match kind {
                    KIND_SEQ => match wire::u64_be(&payload) {
                        Some(s) => seq = Some(s),
                        None => break 'read,
                    },
                    KIND_HELLO if primary && !self.is_host => self.on_hello(link, &payload),
                    k if wire::is_control(k) => {}
                    _ => {
                        if !self.deliver(link, seq.take(), kind, payload) {
                            break 'read;
                        }
                    }
                }
            }
        }
        self.lost();
    }

    /// Hands one received message on: at once when the partner does not stripe, else once every earlier one is
    /// through. False when the channel ended or the stream broke the order.
    fn deliver(&self, link: &Link, seq: Option<u64>, kind: u8, payload: Vec<u8>) -> bool {
        let Some(seq) = seq else { return self.to_sink(link, kind, payload) };
        let mut r = link.reorder.lock();
        while !r.admits(seq, payload.len()) {
            if link.is_dead() || self.closed.load(Ordering::SeqCst) {
                return false;
            }
            link.reordered.wait_for(&mut r, Duration::from_millis(200));
        }
        let Some(ready) = r.push(seq, kind, payload) else {
            logger::warn("file_channel", &format!("File channel with {}: message {seq} out of place.", self.partner));
            return false;
        };
        let moved = !ready.is_empty();
        // Under the reorder lock: two streams must not hand on their runs interleaved.
        for (kind, payload) in ready {
            if !self.to_sink(link, kind, payload) {
                return false;
            }
        }
        drop(r);
        if moved {
            link.reordered.notify_all();
        }
        true
    }

    /// Waits for the relay connection to catch up before handing on more (see [`RECEIVE_WINDOW`]).
    fn to_sink(&self, link: &Link, kind: u8, payload: Vec<u8>) -> bool {
        let mut w = self.window.lock();
        while *w > RECEIVE_WINDOW {
            if self.closed.load(Ordering::SeqCst) || link.is_dead() {
                return false;
            }
            self.window_freed.wait_for(&mut w, Duration::from_millis(200));
        }
        *w += payload.len();
        drop(w);
        (self.sink)(kind, payload);
        true
    }

    /// The connection ended: stop using it and tell the relay connection, as a lost P2P path does.
    fn lost(&self) {
        let old = std::mem::replace(&mut *self.phase.lock(), Phase::Down);
        if let Phase::Up(link) = old {
            link.kill();
            if !self.closed.load(Ordering::SeqCst) {
                self.redial.lock().lost(Instant::now());
                logger::info("file_channel", &format!("File channel with {} via {} lost.", self.partner, link.peer));
                (self.sink)(super::p2p::BULK_LOST, Vec::new());
            }
        }
    }
}

/// A pick token: binds extra streams to the connection the viewer picked.
fn random_token() -> u64 {
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_err() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        b = (now.as_nanos() as u64).to_be_bytes();
    }
    u64::from_be_bytes(b)
}

/// A listener on a fresh port, dual-stack when the system allows.
fn listen() -> Option<(TcpListener, u16, bool)> {
    let open = |dual: bool| -> io::Result<(TcpListener, u16)> {
        let raw = Socket::new(if dual { Domain::IPV6 } else { Domain::IPV4 }, Type::STREAM, Some(Protocol::TCP))?;
        if dual {
            raw.set_only_v6(false)?;
        }
        raw.set_reuse_address(true)?;
        #[cfg(unix)]
        raw.set_reuse_port(true)?;
        let any: SocketAddr =
            if dual { SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)) } else { SocketAddr::from(([0, 0, 0, 0], 0)) };
        raw.bind(&any.into())?;
        raw.listen(8)?;
        let listener: TcpListener = raw.into();
        let port = listener.local_addr()?.port();
        Ok((listener, port))
    };
    match open(true) {
        Ok((l, p)) => Some((l, p, true)),
        Err(_) => open(false).ok().map(|(l, p)| (l, p, false)),
    }
}

fn accept_loop(listener: TcpListener, weak: Weak<FileChannel>) {
    for conn in listener.incoming() {
        let Some(me) = weak.upgrade() else { return };
        if me.closed.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = conn else { continue };
        let _ = stream.set_nonblocking(false);
        let Ok(from) = stream.peer_addr() else { continue };
        if !me.accepting() {
            continue;
        }
        me.secure(stream, udp_socket::canonical(from));
    }
}

/// Large buffers, no Nagle delay, and TCP keep-alives so a vanished partner is noticed.
fn tune(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    let sock = socket2::SockRef::from(stream);
    let _ = sock.set_send_buffer_size(SOCKET_BUFFER);
    let _ = sock.set_recv_buffer_size(SOCKET_BUFFER);
    let _ = sock.set_tcp_keepalive(
        &TcpKeepalive::new().with_time(Duration::from_secs(10)).with_interval(Duration::from_secs(5)),
    );
}

/// Seals `msg` (any length) into frames from `nonce` on and writes them.
fn write_segments(w: &mut impl Write, t: &Transport, nonce: &mut u64, frame: &mut [u8], msg: &[u8]) -> io::Result<()> {
    for seg in wire::segments(msg) {
        let n = t.write_message(*nonce, seg, &mut frame[2..]).map_err(|_| io::Error::other("seal"))?;
        *nonce += 1;
        frame[..2].copy_from_slice(&(n as u16).to_be_bytes());
        w.write_all(&frame[..2 + n])?;
    }
    Ok(())
}

/// Writes one channel message with `nonce` (the pick or a join, before the stream's writer starts).
fn write_one(w: &mut impl Write, t: &Transport, nonce: u64, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut frame = vec![0u8; 2 + wire::SEGMENT + 16];
    let mut n = nonce;
    write_segments(w, t, &mut n, &mut frame, &wire::encode(kind, payload))
}

fn write_frame(w: &mut impl Write, msg: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(2 + msg.len());
    frame.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    frame.extend_from_slice(msg);
    w.write_all(&frame)
}

fn read_frame(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut head = [0u8; 2];
    r.read_exact(&mut head)?;
    let len = u16::from_be_bytes(head) as usize;
    if len == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty frame"));
    }
    let mut msg = vec![0u8; len];
    r.read_exact(&mut msg)?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::bulk;

    fn keypair() -> snow::Keypair {
        snow::Builder::new(NOISE_XX.parse().unwrap()).generate_keypair().unwrap()
    }

    type Inbox = Arc<Mutex<Vec<(u8, Vec<u8>)>>>;

    /// A viewer offering `vs` streams and a host offering `hs`; `viewer_expects` replaces the host key the viewer
    /// checks (a wrong key).
    fn pair_with(
        viewer_expects: Option<Vec<u8>>,
        vs: u8,
        hs: u8,
    ) -> (Arc<FileChannel>, Arc<FileChannel>, Inbox, Inbox) {
        let (kv, kh) = (keypair(), keypair());
        let (iv, ih): (Inbox, Inbox) = Default::default();
        let (sv, sh) = (Arc::clone(&iv), Arc::clone(&ih));
        let expect = viewer_expects.unwrap_or_else(|| kh.public.clone());
        let viewer = FileChannel::open(
            false,
            "111111111",
            "222222222",
            &kv.private,
            &expect,
            Arc::new(move |k, p| sv.lock().push((k, p))),
            Redial::default(),
            vs,
        )
        .unwrap();
        let host = FileChannel::open(
            true,
            "222222222",
            "111111111",
            &kh.private,
            &kv.public,
            Arc::new(move |k, p| sh.lock().push((k, p))),
            Redial::default(),
            hs,
        )
        .unwrap();
        (viewer, host, iv, ih)
    }

    fn pair(viewer_expects: Option<Vec<u8>>) -> (Arc<FileChannel>, Arc<FileChannel>, Inbox, Inbox) {
        pair_with(viewer_expects, 1, 1)
    }

    fn loopback(c: &FileChannel) -> Vec<SocketAddr> {
        vec![SocketAddr::from(([127, 0, 0, 1], c.port))]
    }

    fn meet(viewer: &Arc<FileChannel>, host: &Arc<FileChannel>) {
        viewer.connect_to(&loopback(host));
        host.connect_to(&loopback(viewer));
        assert!(wait(|| viewer.is_up() && host.is_up()), "no channel");
    }

    fn wait(what: impl Fn() -> bool) -> bool {
        let t = Instant::now();
        while !what() {
            if t.elapsed() > Duration::from_secs(10) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    #[test]
    fn two_sides_meet_and_move_large_messages_both_ways() {
        let (viewer, host, iv, ih) = pair(None);
        meet(&viewer, &host);
        let chunk = vec![0xABu8; 128 * 1024 + 30];
        let delivered = Arc::new(AtomicBool::new(false));
        let d = Arc::clone(&delivered);
        assert!(viewer.send(bulk::MSG_BINARY, &chunk, Some(Box::new(move |ok| d.store(ok, Ordering::SeqCst)))));
        assert!(viewer.send(bulk::MSG_JSON, b"{\"x\":1}", None));
        assert!(host.send(bulk::MSG_JSON, b"{\"ack\":true}", None));
        assert!(wait(|| ih.lock().len() == 2 && iv.lock().len() == 1));
        assert!(delivered.load(Ordering::SeqCst));
        assert_eq!(ih.lock()[0], (bulk::MSG_BINARY, chunk));
        assert_eq!(iv.lock()[0].1, b"{\"ack\":true}");
        assert_eq!((viewer.streams_up(), host.streams_up()), (1, 1));
        host.consumed(1 << 30);
        // Closing one side is a lost channel on the other, which then wants to be set up again.
        viewer.close();
        assert!(wait(|| ih.lock().iter().any(|(k, _)| *k == crate::infra::p2p::BULK_LOST)));
        assert!(!host.is_up());
        assert!(host.redialing() && host.may_answer() && !viewer.redialing());
        assert!(!host.redial_due(Instant::now()) && host.redial_due(Instant::now() + wire::REDIAL_FIRST));
        host.close();
    }

    #[test]
    fn a_partner_with_another_key_is_refused() {
        let stranger = keypair();
        let (viewer, host, _iv, _ih) = pair(Some(stranger.public));
        viewer.connect_to(&loopback(&host));
        host.connect_to(&loopback(&viewer));
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!viewer.is_up() && !host.is_up());
        assert!(!viewer.send(bulk::MSG_JSON, b"{}", None));
        viewer.close();
        host.close();
    }

    /// Sends `n` numbered messages each way and checks every one arrived once, in order.
    fn exchange_in_order(viewer: &FileChannel, host: &FileChannel, iv: &Inbox, ih: &Inbox, n: u32) {
        for i in 0..n {
            let mut msg = vec![(i % 251) as u8; 20_000 + (i as usize * 7919) % 120_000];
            msg[..4].copy_from_slice(&i.to_be_bytes());
            assert!(viewer.send(bulk::MSG_BINARY, &msg, None));
            assert!(host.send(bulk::MSG_BINARY, &msg[..64], None));
        }
        let all = |inbox: &Inbox, side: &FileChannel| {
            let got = inbox.lock().len();
            side.consumed(1 << 30);
            got == n as usize
        };
        assert!(wait(|| all(ih, host) && all(iv, viewer)), "messages lost");
        for inbox in [ih, iv] {
            for (i, (kind, msg)) in inbox.lock().iter().enumerate() {
                assert_eq!((*kind, u32::from_be_bytes(msg[..4].try_into().unwrap())), (bulk::MSG_BINARY, i as u32));
            }
        }
    }

    /// Both sides stripe: the viewer adds three streams and the messages of both directions, spread over them,
    /// still come out in the order they were sent.
    #[test]
    fn four_streams_carry_messages_in_order() {
        let (viewer, host, iv, ih) = pair_with(None, 4, 4);
        meet(&viewer, &host);
        assert!(wait(|| viewer.streams_up() == 4 && host.streams_up() == 4), "extra streams did not join");
        exchange_in_order(&viewer, &host, &iv, &ih, 400);
        // One stream lost is the whole channel lost: its messages took the order with them.
        viewer.close();
        assert!(wait(|| ih.lock().iter().any(|(k, _)| *k == crate::infra::p2p::BULK_LOST)));
        host.close();
    }

    /// A partner that cannot stripe (an empty pick, or no hello) keeps one stream without sequence numbers.
    #[test]
    fn a_single_stream_partner_keeps_one_stream() {
        for (vs, hs) in [(1, 4), (4, 1)] {
            let (viewer, host, iv, ih) = pair_with(None, vs, hs);
            meet(&viewer, &host);
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!((viewer.streams_up(), host.streams_up()), (1, 1), "viewer {vs}, host {hs}");
            exchange_in_order(&viewer, &host, &iv, &ih, 50);
            viewer.close();
            host.close();
        }
    }

    /// Raw speed of the channel on loopback (encryption, framing, threads):
    /// `cargo test --release throughput -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn throughput_on_loopback() {
        for streams in [1, 4] {
            let (viewer, host, _iv, ih) = pair_with(None, streams, streams);
            meet(&viewer, &host);
            assert!(wait(|| viewer.streams_up() == streams as usize));
            let chunk = vec![0x5Au8; 128 * 1024];
            let total = 2048; // 256 MiB
            let start = Instant::now();
            let (mut sent, mut received) = (0, 0);
            let drain = |received: &mut usize| {
                let got = {
                    let mut v = ih.lock();
                    let n = v.len();
                    v.clear();
                    n
                };
                *received += got;
                host.consumed(got * chunk.len());
            };
            while sent < total {
                // Like the senders: at most 16 MB in flight.
                if sent - received < 128 && viewer.send(bulk::MSG_BINARY, &chunk, None) {
                    sent += 1;
                }
                drain(&mut received);
            }
            let t = Instant::now();
            while received < total && t.elapsed() < Duration::from_secs(60) {
                drain(&mut received);
                std::thread::sleep(Duration::from_millis(1));
            }
            let secs = start.elapsed().as_secs_f64();
            assert_eq!(received, total, "lost messages");
            println!("loopback, {streams} streams: {:.0} MB/s", (total * chunk.len()) as f64 / secs / 1e6);
            viewer.close();
            host.close();
        }
    }
}
