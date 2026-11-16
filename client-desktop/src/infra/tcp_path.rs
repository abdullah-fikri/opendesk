//! Direct TCP for the P2P path (K3): the same sealed datagrams, length-prefixed on a TCP stream, for networks that
//! drop UDP (offices, hotels) but let TCP through to the partner (same LAN, a public or IPv6 address, a TCP
//! hole punch). [`super::udp_socket::PathSocket`] owns one; the path code above it still sees datagrams from and
//! to socket addresses and never knows which way they travelled.
//!
//! Nothing here is trusted: a frame is only bytes until the Noise layer above opens it, exactly as with UDP.
//! Connections are tried only after UDP found nobody (see `P2p::tick`), at most [`MAX_STREAMS`] live at once.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use socket2::{Domain, Protocol, Socket, Type};

use crate::infra::logger;

/// Streams alive at once; more connections than the partner's candidates could ever need are refused.
const MAX_STREAMS: usize = 8;
/// Largest datagram a frame may carry (a sealed datagram is at most 1200 bytes).
const MAX_FRAME: usize = 2048;
/// Datagrams queued to one stream's writer; a full queue drops the newest, as a full UDP buffer would.
const WRITE_QUEUE: usize = 512;
/// One connect attempt and the pause before the next one at the same address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_RETRY: Duration = Duration::from_secs(1);
/// Connect attempts to one address (a simultaneous open may need a few to line up).
const CONNECT_TRIES: u32 = 5;
/// A stream is used for sending only while UDP from that address has been silent this long.
const UDP_SILENCE: Duration = Duration::from_secs(5);
/// Marks the datagram a reader thread sends itself to end the UDP socket's wait.
const WAKE: u8 = 0xFF;

/// `OPENDESK_TCP_PATH=0` keeps the path UDP-only.
pub fn enabled() -> bool {
    std::env::var("OPENDESK_TCP_PATH").map(|v| v != "0").unwrap_or(true)
}

struct Stream {
    tx: SyncSender<Vec<u8>>,
    raw: TcpStream,
}

pub struct TcpPath {
    port: u16,
    dual: bool,
    wake: Option<UdpSocket>,
    wake_to: SocketAddr,
    streams: Mutex<HashMap<SocketAddr, Stream>>,
    inbox: Mutex<VecDeque<(Vec<u8>, SocketAddr)>>,
    /// Addresses a connect thread works on now.
    connecting: Mutex<Vec<SocketAddr>>,
    /// Last UDP datagram from each address while any stream exists.
    udp_heard: Mutex<HashMap<SocketAddr, Instant>>,
    listening: AtomicBool,
    /// A stream exists: sends and receives take the slow path that looks at it.
    pub active: AtomicBool,
    closed: AtomicBool,
}

impl TcpPath {
    /// For the UDP socket on `port`; `dual` when that socket also speaks IPv6.
    pub fn new(port: u16, dual: bool) -> Arc<Self> {
        let wake_to = if dual {
            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port))
        } else {
            SocketAddr::from(([127, 0, 0, 1], port))
        };
        let wake = UdpSocket::bind(if dual { "[::1]:0" } else { "127.0.0.1:0" }).ok();
        Arc::new(Self {
            port,
            dual,
            wake,
            wake_to,
            streams: Mutex::new(HashMap::new()),
            inbox: Mutex::new(VecDeque::new()),
            connecting: Mutex::new(Vec::new()),
            udp_heard: Mutex::new(HashMap::new()),
            listening: AtomicBool::new(false),
            active: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    /// Stops listening and drops every stream.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for (_, s) in self.streams.lock().drain() {
            let _ = s.raw.shutdown(std::net::Shutdown::Both);
        }
        self.active.store(false, Ordering::SeqCst);
    }

    /// Opens the listener (once) and starts connecting to `peers`, from the same port number so a partner doing
    /// the same meets us in the middle (TCP simultaneous open). Safe to call again with the same peers.
    pub fn open(self: &Arc<Self>, peers: &[SocketAddr]) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        if !self.listening.swap(true, Ordering::SeqCst) && !self.listen() {
            self.listening.store(false, Ordering::SeqCst);
        }
        for &peer in peers {
            if (peer.is_ipv6() && !self.dual) || self.streams.lock().contains_key(&peer) {
                continue;
            }
            {
                let mut busy = self.connecting.lock();
                if busy.contains(&peer) || self.streams.lock().len() >= MAX_STREAMS {
                    continue;
                }
                busy.push(peer);
            }
            let me = Arc::clone(self);
            crate::infra::thread::spawn_named("p2p-tcp-connect", move || {
                for attempt in 0..CONNECT_TRIES {
                    if me.closed.load(Ordering::SeqCst) || me.streams.lock().contains_key(&peer) {
                        break;
                    }
                    match me.connect(peer) {
                        Ok(stream) => {
                            logger::info("anynet.p2p", &format!("TCP path connected to {peer}."));
                            me.adopt(stream, peer);
                            break;
                        }
                        Err(e) if attempt + 1 == CONNECT_TRIES => {
                            logger::info("anynet.p2p", &format!("TCP to {peer} failed: {e}."));
                        }
                        Err(_) => std::thread::sleep(CONNECT_RETRY),
                    }
                }
                me.connecting.lock().retain(|a| *a != peer);
            });
        }
    }

    fn socket(&self) -> io::Result<Socket> {
        let raw = Socket::new(if self.dual { Domain::IPV6 } else { Domain::IPV4 }, Type::STREAM, Some(Protocol::TCP))?;
        if self.dual {
            raw.set_only_v6(false)?;
        }
        raw.set_reuse_address(true)?;
        // Linux and macOS refuse a second socket on the listener's port (EADDRINUSE) unless every socket on it
        // sets SO_REUSEPORT; the connect sockets must share that port for the simultaneous open.
        #[cfg(unix)]
        raw.set_reuse_port(true)?;
        raw.set_tcp_nodelay(true)?;
        let any: SocketAddr = if self.dual {
            SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, self.port))
        } else {
            SocketAddr::from(([0, 0, 0, 0], self.port))
        };
        raw.bind(&any.into())?;
        Ok(raw)
    }

    fn connect(&self, peer: SocketAddr) -> io::Result<TcpStream> {
        let raw = self.socket()?;
        let wire = match (peer, self.dual) {
            (SocketAddr::V4(v4), true) => SocketAddr::from((v4.ip().to_ipv6_mapped(), v4.port())),
            _ => peer,
        };
        raw.connect_timeout(&wire.into(), CONNECT_TIMEOUT)?;
        Ok(raw.into())
    }

    fn listen(self: &Arc<Self>) -> bool {
        let Ok(raw) = self.socket() else { return false };
        if raw.listen(8).is_err() {
            return false;
        }
        let listener: std::net::TcpListener = raw.into();
        let weak: Weak<Self> = Arc::downgrade(self);
        crate::infra::thread::spawn_named("p2p-tcp-accept", move || {
            for conn in listener.incoming() {
                let Some(me) = weak.upgrade() else { return };
                if me.closed.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = conn else { continue };
                let Ok(from) = stream.peer_addr() else { continue };
                let from = super::udp_socket::canonical(from);
                if me.streams.lock().len() >= MAX_STREAMS {
                    continue;
                }
                logger::info("anynet.p2p", &format!("TCP path accepted from {from}."));
                me.adopt(stream, from);
            }
        });
        true
    }

    /// Starts the reader and writer threads of a connected stream known by `peer`.
    fn adopt(self: &Arc<Self>, stream: TcpStream, peer: SocketAddr) {
        let _ = stream.set_nodelay(true);
        let (Ok(read_half), Ok(mut write_half)) = (stream.try_clone(), stream.try_clone()) else { return };
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(WRITE_QUEUE);
        {
            let mut streams = self.streams.lock();
            if streams.contains_key(&peer) {
                return;
            }
            streams.insert(peer, Stream { tx, raw: stream });
        }
        self.active.store(true, Ordering::SeqCst);
        crate::infra::thread::spawn_named("p2p-tcp-write", move || {
            let _ = write_half.set_write_timeout(Some(Duration::from_secs(5)));
            while let Ok(pkt) = rx.recv() {
                let mut frame = Vec::with_capacity(pkt.len() + 2);
                frame.extend_from_slice(&(pkt.len() as u16).to_be_bytes());
                frame.extend_from_slice(&pkt);
                if write_half.write_all(&frame).is_err() {
                    break;
                }
            }
            let _ = write_half.shutdown(std::net::Shutdown::Both);
        });
        let me = Arc::clone(self);
        crate::infra::thread::spawn_named("p2p-tcp-read", move || {
            let mut read_half = read_half;
            let mut head = [0u8; 2];
            while !me.closed.load(Ordering::SeqCst) && read_half.read_exact(&mut head).is_ok() {
                let len = u16::from_be_bytes(head) as usize;
                if len == 0 || len > MAX_FRAME {
                    break; // not our framing: drop the connection
                }
                let mut pkt = vec![0u8; len];
                if read_half.read_exact(&mut pkt).is_err() {
                    break;
                }
                me.inbox.lock().push_back((pkt, peer));
                if let Some(w) = &me.wake {
                    let _ = w.send_to(&[WAKE], me.wake_to);
                }
            }
            me.drop_stream(peer);
        });
    }

    fn drop_stream(&self, peer: SocketAddr) {
        let mut streams = self.streams.lock();
        if let Some(s) = streams.remove(&peer) {
            let _ = s.raw.shutdown(std::net::Shutdown::Both);
            logger::info("anynet.p2p", &format!("TCP path to {peer} closed."));
        }
        self.active.store(!streams.is_empty(), Ordering::SeqCst);
    }

    /// Queues `pkt` on the stream to `to` unless UDP from there is alive; false when UDP should carry it.
    pub fn send(&self, pkt: &[u8], to: SocketAddr) -> bool {
        if self.udp_heard.lock().get(&to).is_some_and(|t| t.elapsed() < UDP_SILENCE) {
            return false;
        }
        let streams = self.streams.lock();
        let Some(stream) = streams.get(&to) else { return false };
        // A full queue drops this datagram; the path above already copes with loss.
        let _ = stream.tx.try_send(pkt.to_vec());
        true
    }

    /// The next datagram that arrived over TCP, copied into `buf`.
    pub fn recv(&self, buf: &mut [u8]) -> Option<(usize, SocketAddr)> {
        let (pkt, from) = self.inbox.lock().pop_front()?;
        let n = pkt.len().min(buf.len());
        buf[..n].copy_from_slice(&pkt[..n]);
        Some((n, from))
    }

    /// A UDP datagram arrived from `from`: while that keeps happening, UDP stays the way to send there.
    pub fn heard_udp(&self, from: SocketAddr) {
        self.udp_heard.lock().insert(from, Instant::now());
    }

    /// Whether `buf` from `from` is a reader thread's own wake-up.
    pub fn is_wake(&self, buf: &[u8], from: SocketAddr) -> bool {
        buf == [WAKE] && from.ip().is_loopback()
    }

    /// Whether a stream to `peer` is open.
    pub fn has(&self, peer: SocketAddr) -> bool {
        self.streams.lock().contains_key(&peer)
    }

    /// Streams open now.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.streams.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_port() -> u16 {
        UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// Two paths on loopback connect to each other at the same time (the simultaneous open of a hole punch) and
    /// carry datagrams both ways.
    #[test]
    fn two_paths_meet_and_exchange_datagrams() {
        let (pa, pb) = (free_port(), free_port());
        let (a, b) = (TcpPath::new(pa, false), TcpPath::new(pb, false));
        let (addr_a, addr_b) = (SocketAddr::from(([127, 0, 0, 1], pa)), SocketAddr::from(([127, 0, 0, 1], pb)));
        a.open(&[addr_b]);
        b.open(&[addr_a]);
        let start = Instant::now();
        while (a.count() == 0 || b.count() == 0) && start.elapsed() < Duration::from_secs(8) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(a.count() > 0 && b.count() > 0, "no stream");
        // Whichever side's connection won, each knows the other under some address.
        let to_b = *a.streams.lock().keys().next().unwrap();
        let to_a = *b.streams.lock().keys().next().unwrap();
        assert!(a.send(b"hello b", to_b));
        assert!(b.send(b"hello a", to_a));
        let mut buf = [0u8; 64];
        let got = |p: &Arc<TcpPath>, buf: &mut [u8]| {
            let t = Instant::now();
            loop {
                if let Some((n, _)) = p.recv(buf) {
                    return buf[..n].to_vec();
                }
                assert!(t.elapsed() < Duration::from_secs(3), "nothing arrived");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        assert_eq!(got(&b, &mut buf), b"hello b");
        assert_eq!(got(&a, &mut buf), b"hello a");
        // UDP heard from the partner takes the traffic back.
        a.heard_udp(to_b);
        assert!(!a.send(b"x", to_b));
        a.close();
        b.close();
    }

    #[test]
    fn a_frame_that_is_not_ours_ends_the_connection() {
        let port = free_port();
        let a = TcpPath::new(port, false);
        a.open(&[]);
        std::thread::sleep(Duration::from_millis(200));
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let start = Instant::now();
        while a.count() == 0 && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(a.count(), 1);
        s.write_all(&[0xFF, 0xFF, 1, 2, 3]).unwrap(); // 65535-byte frame: refused
        let start = Instant::now();
        while a.count() > 0 && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(a.count(), 0);
        a.close();
    }
}
