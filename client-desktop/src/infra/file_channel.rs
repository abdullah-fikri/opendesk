//! Direct file channel (`file-channel`): a TCP connection of its own between the two peers of a session, only for
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
//! Messages are the file stream messages of `domain::bulk` (see `domain::file_channel` for the framing), handed to
//! the same handler as those of the P2P path. A lost connection reports [`super::p2p::BULK_LOST`], which cancels
//! (and lets the sender resume elsewhere) whatever was moving on it, exactly like a lost P2P path.
//!
//! `OPENDESK_FILE_CHANNEL=0` turns it off (not advertised, not used).

use std::io::{self, Read, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};

use crate::domain::bulk::OnDelivered;
use crate::domain::file_channel::{self as wire, KIND_PING, KIND_SELECT, Reader};
use crate::infra::{logger, udp_socket};

const NOISE_XX: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Connect attempts per candidate (a simultaneous open may need a few to line up), each this long at most.
const CONNECT_TRIES: u32 = 6;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_RETRY: Duration = Duration::from_millis(700);
/// The whole handshake on one connection, and how long the host waits for the viewer's pick after it.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections being set up at once; more are refused (a port scan must not spawn threads without end).
const MAX_PENDING: usize = 16;
/// Messages queued for the writer. A file chunk is at most 128 KiB, so this is a few MB.
const WRITE_QUEUE: usize = 64;
/// Bytes received but not yet handled by the relay connection thread; the reader stops reading above this, and
/// TCP then slows the sender down (a slow disk throttles the partner instead of filling memory).
const RECEIVE_WINDOW: usize = 32 << 20;
/// Socket buffers: room for a long fat path (100 MB/s at 80 ms).
const SOCKET_BUFFER: usize = 8 << 20;
/// A keep-alive goes out after this much quiet; a connection silent for [`SILENCE`] is dead.
const PING_EVERY: Duration = Duration::from_secs(5);
const SILENCE: Duration = Duration::from_secs(25);

/// Client-side kill switch: `OPENDESK_FILE_CHANNEL=0` neither advertises nor uses the channel.
pub fn enabled() -> bool {
    std::env::var("OPENDESK_FILE_CHANNEL").map(|v| v != "0").unwrap_or(true)
}

/// Where received messages go (the relay connection thread), as for the P2P path's file stream.
pub type Sink = Arc<dyn Fn(u8, Vec<u8>) + Send + Sync>;

/// One queued message and what to call once it is in the socket (or could not be).
type Outbound = (u8, Vec<u8>, Option<OnDelivered>);

enum Phase {
    /// Looking for a connection.
    Connecting,
    /// Connected: messages go to this writer.
    Up { tx: SyncSender<Outbound>, raw: TcpStream, peer: SocketAddr },
    /// Lost or closed; not tried again in this session.
    Down,
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
    phase: Mutex<Phase>,
    pending: Mutex<usize>,
    /// Received bytes not yet consumed (see [`RECEIVE_WINDOW`]).
    window: Mutex<usize>,
    window_freed: Condvar,
    sink: Sink,
    started: Instant,
    closed: AtomicBool,
}

impl FileChannel {
    /// Starts listening for `partner`. `secret` is this device's Noise key, `peer_key` the partner's static key the
    /// end-to-end handshake authenticated. None when no port could be opened.
    pub fn start(
        is_host: bool,
        local_clean: &str,
        partner: &str,
        secret: &[u8],
        peer_key: &[u8],
        sink: Sink,
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
            phase: Mutex::new(Phase::Connecting),
            pending: Mutex::new(0),
            window: Mutex::new(0),
            window_freed: Condvar::new(),
            sink,
            started: Instant::now(),
            closed: AtomicBool::new(false),
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

    /// The partner's candidates arrived: connect to each.
    pub fn connect_to(self: &Arc<Self>, peers: &[SocketAddr]) {
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

    /// True while connected.
    pub fn is_up(&self) -> bool {
        matches!(*self.phase.lock(), Phase::Up { .. })
    }

    /// Queues one file stream message; `done` runs once it is in the socket (true) or the channel is gone (false).
    /// False when the channel is not up (`done` then already ran with false).
    pub fn send(&self, kind: u8, payload: &[u8], done: Option<OnDelivered>) -> bool {
        let tx = match &*self.phase.lock() {
            Phase::Up { tx, .. } => tx.clone(),
            _ => {
                if let Some(done) = done {
                    done(false);
                }
                return false;
            }
        };
        // Blocks only while the writer has a full queue: the sender keeps a few MB in flight at most.
        match tx.send((kind, payload.to_vec(), done)) {
            Ok(()) => true,
            Err(mpsc::SendError((_, _, done))) => {
                if let Some(done) = done {
                    done(false);
                }
                false
            }
        }
    }

    /// The relay connection handled `bytes` of received payload: the reader may read on.
    pub fn consumed(&self, bytes: usize) {
        let mut w = self.window.lock();
        *w = w.saturating_sub(bytes);
        self.window_freed.notify_all();
    }

    /// Ends the channel (the session ended).
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let old = std::mem::replace(&mut *self.phase.lock(), Phase::Down);
        if let Phase::Up { raw, .. } = old {
            let _ = raw.shutdown(std::net::Shutdown::Both);
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
    /// is the one picked.
    fn secure(self: &Arc<Self>, stream: TcpStream, peer: SocketAddr) {
        {
            let mut pending = self.pending.lock();
            if *pending >= MAX_PENDING || !self.connecting() {
                return;
            }
            *pending += 1;
        }
        let me = Arc::clone(self);
        crate::infra::thread::spawn_named("file-channel-handshake", move || {
            let result = me.handshake(&stream);
            *me.pending.lock() -= 1;
            match result {
                Ok(transport) => me.adopt(stream, peer, transport),
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
    /// end-to-end handshake authenticated. The host then waits for the viewer's pick on this connection.
    fn handshake(&self, stream: &TcpStream) -> io::Result<Arc<snow::StatelessTransportState>> {
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
        if self.is_host {
            // The viewer's pick is the first message on the connection it keeps.
            let frame = read_frame(&mut r)?;
            let mut plain = vec![0u8; frame.len()];
            let n = transport.read_message(0, &frame, &mut plain).map_err(|_| bad("pick"))?;
            let picked = Reader::default().push(&plain[..n]).and_then(|m| m.first().map(|(k, _)| *k == KIND_SELECT));
            if picked != Some(true) {
                return Err(bad("not picked"));
            }
        }
        Ok(transport)
    }

    /// Makes `stream` the channel unless another connection already is; the viewer announces its pick.
    fn adopt(self: &Arc<Self>, stream: TcpStream, peer: SocketAddr, transport: Arc<snow::StatelessTransportState>) {
        let (tx, rx) = mpsc::sync_channel::<Outbound>(WRITE_QUEUE);
        {
            let mut phase = self.phase.lock();
            if !matches!(*phase, Phase::Connecting) || self.closed.load(Ordering::SeqCst) {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return;
            }
            let Ok(raw) = stream.try_clone() else { return };
            *phase = Phase::Up { tx: tx.clone(), raw, peer };
        }
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
        // The writer's first nonce: the host already used 0 to read the pick, the viewer sends it now.
        let (Ok(read_half), Ok(write_half)) = (stream.try_clone(), stream.try_clone()) else { return self.lost() };
        if !self.is_host {
            let _ = tx.try_send((KIND_SELECT, Vec::new(), None));
        }
        let (me, t) = (Arc::clone(self), Arc::clone(&transport));
        crate::infra::thread::spawn_named("file-channel-write", move || me.write_loop(write_half, t, rx));
        let me = Arc::clone(self);
        let first_rx = if self.is_host { 1 } else { 0 };
        crate::infra::thread::spawn_named("file-channel-read", move || me.read_loop(read_half, transport, first_rx));
    }

    fn write_loop(
        self: Arc<Self>,
        mut stream: TcpStream,
        t: Arc<snow::StatelessTransportState>,
        rx: Receiver<Outbound>,
    ) {
        let mut nonce = 0u64;
        let mut frame = vec![0u8; 2 + wire::SEGMENT + 16];
        loop {
            let (kind, payload, done) = match rx.recv_timeout(PING_EVERY) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => (KIND_PING, Vec::new(), None),
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let msg = wire::encode(kind, &payload);
            let mut ok = true;
            for seg in wire::segments(&msg) {
                let Ok(n) = t.write_message(nonce, seg, &mut frame[2..]) else {
                    ok = false;
                    break;
                };
                nonce += 1;
                frame[..2].copy_from_slice(&(n as u16).to_be_bytes());
                if stream.write_all(&frame[..2 + n]).is_err() {
                    ok = false;
                    break;
                }
            }
            if let Some(done) = done {
                done(ok);
            }
            if !ok || self.closed.load(Ordering::SeqCst) {
                break;
            }
        }
        // Whatever is still queued will never leave.
        while let Ok((_, _, done)) = rx.try_recv() {
            if let Some(done) = done {
                done(false);
            }
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    fn read_loop(self: Arc<Self>, mut stream: TcpStream, t: Arc<snow::StatelessTransportState>, first: u64) {
        let mut nonce = first;
        let mut reader = Reader::default();
        let mut plain = vec![0u8; wire::SEGMENT + 16];
        'read: while !self.closed.load(Ordering::SeqCst) {
            let Ok(frame) = read_frame(&mut stream) else { break };
            let Ok(n) = t.read_message(nonce, &frame, &mut plain) else { break };
            nonce += 1;
            let Some(messages) = reader.push(&plain[..n]) else { break };
            for (kind, payload) in messages {
                if kind == KIND_PING || kind == KIND_SELECT {
                    continue;
                }
                // Wait for the relay connection to catch up before taking more off the network.
                let mut w = self.window.lock();
                while *w > RECEIVE_WINDOW {
                    if self.closed.load(Ordering::SeqCst) {
                        break 'read;
                    }
                    self.window_freed.wait_for(&mut w, Duration::from_millis(200));
                }
                *w += payload.len();
                drop(w);
                (self.sink)(kind, payload);
            }
        }
        self.lost();
    }

    /// The connection ended: stop using it and tell the relay connection, as a lost P2P path does.
    fn lost(&self) {
        let old = std::mem::replace(&mut *self.phase.lock(), Phase::Down);
        if let Phase::Up { raw, peer, .. } = old {
            let _ = raw.shutdown(std::net::Shutdown::Both);
            if !self.closed.load(Ordering::SeqCst) {
                logger::info("file_channel", &format!("File channel with {} via {peer} lost.", self.partner));
                (self.sink)(super::p2p::BULK_LOST, Vec::new());
            }
        }
    }
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
        if !me.connecting() {
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

    /// A viewer and a host; `viewer_expects` replaces the host key the viewer checks (a wrong key).
    fn pair(viewer_expects: Option<Vec<u8>>) -> (Arc<FileChannel>, Arc<FileChannel>, Inbox, Inbox) {
        let (kv, kh) = (keypair(), keypair());
        let (iv, ih): (Inbox, Inbox) = Default::default();
        let (sv, sh) = (Arc::clone(&iv), Arc::clone(&ih));
        let expect = viewer_expects.unwrap_or_else(|| kh.public.clone());
        let viewer = FileChannel::start(
            false,
            "111111111",
            "222222222",
            &kv.private,
            &expect,
            Arc::new(move |k, p| sv.lock().push((k, p))),
        )
        .unwrap();
        let host = FileChannel::start(
            true,
            "222222222",
            "111111111",
            &kh.private,
            &kv.public,
            Arc::new(move |k, p| sh.lock().push((k, p))),
        )
        .unwrap();
        (viewer, host, iv, ih)
    }

    fn loopback(c: &FileChannel) -> Vec<SocketAddr> {
        vec![SocketAddr::from(([127, 0, 0, 1], c.port))]
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
        viewer.connect_to(&loopback(&host));
        host.connect_to(&loopback(&viewer));
        assert!(wait(|| viewer.is_up() && host.is_up()), "no channel");
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
        host.consumed(1 << 30);
        // Closing one side is a lost channel on the other.
        viewer.close();
        assert!(wait(|| ih.lock().iter().any(|(k, _)| *k == crate::infra::p2p::BULK_LOST)));
        assert!(!host.is_up());
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

    /// Raw speed of the channel on loopback (encryption, framing, threads): `cargo test --release throughput -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn throughput_on_loopback() {
        let (viewer, host, _iv, ih) = pair(None);
        viewer.connect_to(&loopback(&host));
        host.connect_to(&loopback(&viewer));
        assert!(wait(|| viewer.is_up() && host.is_up()));
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
            if viewer.send(bulk::MSG_BINARY, &chunk, None) {
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
        println!("loopback: {:.0} MB/s", (total * chunk.len()) as f64 / secs / 1e6);
        viewer.close();
        host.close();
    }
}
