//! Opening the relay socket with bounded time: fresh DNS, TCP, TLS and the WebSocket upgrade.
//!
//! `tungstenite::connect` blocks without any timeout. When the network drops in the middle of the TLS or
//! upgrade handshake (a flapping link, a modem that restarts and loses its NAT state), the server's reset never
//! arrives and the blocking read waits forever: the process stays up but never reconnects, and the device shows
//! offline until OpenDesk is restarted. Here every attempt runs on its own thread with a deadline and the loop
//! starts over from a fresh DNS lookup past it. The stuck read itself ends through a socket timeout during the
//! handshake (Windows: a `shutdown` from another thread does not wake a blocked `recv` there) or the shutdown at
//! the deadline (elsewhere: a timeout there reads as `WouldBlock`, which tungstenite's blocking TLS handshake
//! treats as a bug and panics on).

use std::io;
use std::net::ToSocketAddrs;

use parking_lot::Mutex;
use socket2::{SockRef, TcpKeepalive};
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::HandshakeError;

use super::*;

/// Longest one connect attempt may take, DNS to `101 Switching Protocols`.
pub(super) const CONNECT_DEADLINE: Duration = Duration::from_secs(20);
/// Per-address TCP connect timeout (Windows otherwise waits ~21 s per address).
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Read and write timeout of the handshake on Windows, below `CONNECT_DEADLINE`.
pub(super) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// TCP keep-alive: the OS notices a vanished relay even while a blocking handshake read waits on it.
const KEEPALIVE_TIME: Duration = Duration::from_secs(10);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// The TCP socket of an attempt in progress, so the waiting side can shut it down past the deadline.
#[derive(Default)]
pub(super) struct Attempt {
    /// `(abandoned, socket)`: once abandoned, a socket connected later is shut down right away.
    state: Mutex<(bool, Option<TcpStream>)>,
}

impl Attempt {
    /// Records the connected socket; false when the attempt was already abandoned.
    fn attach(&self, stream: &TcpStream) -> bool {
        let mut state = self.state.lock();
        if state.0 {
            return false;
        }
        state.1 = stream.try_clone().ok();
        true
    }

    /// Gives up on the attempt: its socket is shut down, which ends a blocked handshake read with an error.
    pub(super) fn abandon(&self) {
        let mut state = self.state.lock();
        state.0 = true;
        if let Some(s) = state.1.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Connects to `url` (blocking, on the caller's thread): resolves it afresh, connects TCP with a timeout and
/// runs the TLS and WebSocket handshakes, each read and write limited to `handshake_timeout` on Windows.
/// `attempt` lets another thread cut a hung handshake short.
pub(super) fn dial(url: &str, attempt: &Attempt, handshake_timeout: Duration) -> Result<Socket, String> {
    let request = url.into_client_request().map_err(|e| e.to_string())?;
    let uri = request.uri();
    let host = uri.host().ok_or("no host name in the relay URL")?;
    let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("wss") { 443 } else { 80 });
    let addrs = (host.as_str(), port).to_socket_addrs().map_err(|e| format!("IO error: {e}"))?;
    let mut last = io::Error::new(ErrorKind::NotFound, "no address for the relay host");
    let stream = addrs
        .into_iter()
        .find_map(|addr| TcpStream::connect_timeout(&addr, TCP_CONNECT_TIMEOUT).map_err(|e| last = e).ok())
        .ok_or_else(|| format!("IO error: {last}"))?;
    let _ = stream.set_nodelay(true);
    let keepalive = TcpKeepalive::new().with_time(KEEPALIVE_TIME).with_interval(KEEPALIVE_INTERVAL);
    let _ = SockRef::from(&stream).set_tcp_keepalive(&keepalive);
    if !attempt.attach(&stream) {
        return Err("timed out".into());
    }
    #[cfg(windows)]
    {
        let _ = stream.set_read_timeout(Some(handshake_timeout));
        let _ = stream.set_write_timeout(Some(handshake_timeout));
    }
    #[cfg(not(windows))]
    let _ = handshake_timeout;
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut socket = tungstenite::client_tls(request, stream).map(|(socket, _)| socket).map_err(|e| match e {
        HandshakeError::Failure(e) => e.to_string(),
        HandshakeError::Interrupted(_) => "handshake interrupted".into(),
    })?;
    // The connection loop polls the socket without blocking (see `set_low_latency`); no timeouts from here on.
    #[cfg(windows)]
    {
        let tcp = match socket.get_mut() {
            MaybeTlsStream::Plain(s) => &*s,
            MaybeTlsStream::NativeTls(tls) => tls.get_ref(),
            _ => return Ok(socket),
        };
        let _ = tcp.set_read_timeout(None);
        let _ = tcp.set_write_timeout(None);
    }
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn abandoning_wakes_a_handshake_the_server_never_answers() {
        // A server that accepts TCP but never answers the upgrade: the old blocking connect hung here forever.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let attempt = Arc::new(Attempt::default());
        let a = Arc::clone(&attempt);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(dial(&url, &a, Duration::from_secs(2)).map(|_| ()));
        });
        let _held = listener.accept().unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "still waiting for the server");
        attempt.abandon();
        let result = rx.recv_timeout(Duration::from_secs(5)).expect("abandon must end the attempt");
        assert!(result.is_err());
    }

    #[test]
    fn a_socket_connected_after_the_deadline_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let attempt = Attempt::default();
        attempt.abandon();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        assert_eq!(dial(&url, &attempt, HANDSHAKE_TIMEOUT).err().as_deref(), Some("timed out"));
    }

    #[test]
    fn unreachable_relay_fails_instead_of_hanging() {
        // Nothing listens on this port (bound, then closed): refused right away.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(dial(&format!("ws://127.0.0.1:{port}"), &Attempt::default(), HANDSHAKE_TIMEOUT).is_err());
    }
}
