//! Socket setup: the relay URL, TCP_NODELAY and the send buffer size.

use super::*;

/// Uses the loopback relay when one runs on this machine and the configured URL is a remote `wss://` one
/// (the relay machine itself would otherwise hairpin through its own tunnel).
pub(super) fn resolve_url(configured: &str) -> String {
    let url = normalize_gateway_url(configured);
    if url.starts_with("wss://")
        && let Ok(addr) = LOCAL_RELAY.parse::<SocketAddr>()
        && TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok()
    {
        logger::info("relay", "Local relay detected on 127.0.0.1:8088; using loopback.");
        return format!("ws://{LOCAL_RELAY}");
    }
    url
}

/// Disables Nagle and switches the socket to non-blocking polling.
pub(super) fn set_low_latency(socket: &mut Socket) {
    match socket.get_mut() {
        MaybeTlsStream::Plain(s) => {
            let _ = s.set_nodelay(true);
            let _ = s.set_nonblocking(true);
        }
        MaybeTlsStream::NativeTls(tls) => {
            let _ = tls.get_mut().set_nodelay(true);
            let _ = tls.get_mut().set_nonblocking(true);
        }
        _ => {}
    }
}

/// The send buffer this socket should have: [`HOSTING_SEND_BUFFER`] (or `OPENDESK_WS_SNDBUF` bytes, `0` = leave
/// the OS alone) while the hosted session's video goes over it, [`TRANSFER_SEND_BUFFER`] after such a session,
/// None (the OS default, auto-tuned) before one.
pub(super) fn wanted_send_buffer(video_on_socket: bool, current: Option<usize>, env: Option<&str>) -> Option<usize> {
    let hosting = match env.map(|v| v.trim().parse::<usize>()) {
        Some(Ok(0)) => return current,
        Some(Ok(n)) => n.max(MIN_SEND_BUFFER),
        _ => HOSTING_SEND_BUFFER,
    };
    if video_on_socket { Some(hosting) } else { current.map(|_| TRANSFER_SEND_BUFFER) }
}

/// Sets the kernel send buffer (`SO_SNDBUF`) of the relay socket.
pub(super) fn set_send_buffer(socket: &mut Socket, bytes: usize) {
    let tcp = match socket.get_mut() {
        MaybeTlsStream::Plain(s) => &*s,
        MaybeTlsStream::NativeTls(tls) => tls.get_ref(),
        _ => return,
    };
    if let Err(e) = socket2::SockRef::from(tcp).set_send_buffer_size(bytes) {
        logger::warn("relay", &format!("Could not set the socket send buffer to {bytes} bytes: {e}"));
    }
}
