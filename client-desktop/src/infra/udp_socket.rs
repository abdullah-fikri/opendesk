//! The P2P path's UDP socket: dual-stack (IPv4 and IPv6 on one port) where the system allows it, else IPv4.
//! Callers always see plain IPv4 addresses for IPv4 peers; the IPv4-mapped form stays inside this module.

use std::io;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::time::Duration;

pub struct PathSocket {
    sock: UdpSocket,
    /// Bound to `[::]` with IPv4 mapped in: IPv6 peers are reachable too.
    dual: bool,
}

impl PathSocket {
    /// Binds an ephemeral port with `buffer` bytes of send and receive buffer. `OPENDESK_IPV6=0` keeps IPv4 only.
    pub fn bind(buffer: usize) -> io::Result<Self> {
        use socket2::{Domain, Protocol, Socket, Type};
        let want_dual = std::env::var("OPENDESK_IPV6").map_or(true, |v| v != "0");
        let open = |domain: Domain| -> io::Result<Socket> {
            let raw = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
            let _ = raw.set_recv_buffer_size(buffer);
            let _ = raw.set_send_buffer_size(buffer);
            Ok(raw)
        };
        if want_dual
            && let Ok(raw) = open(Domain::IPV6)
            && raw.set_only_v6(false).is_ok()
            && raw.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)).into()).is_ok()
        {
            return Ok(Self { sock: raw.into(), dual: true });
        }
        let raw = open(Domain::IPV4)?;
        raw.bind(&SocketAddr::from(([0, 0, 0, 0], 0)).into())?;
        Ok(Self { sock: raw.into(), dual: false })
    }

    /// True when IPv6 candidates can be offered and reached.
    pub fn is_dual(&self) -> bool {
        self.dual
    }

    pub fn port(&self) -> u16 {
        self.sock.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    pub fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<usize> {
        self.sock.send_to(buf, self.wire(to)?)
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let (n, from) = self.sock.recv_from(buf)?;
        Ok((n, canonical(from)))
    }

    pub fn set_read_timeout(&self, wait: Option<Duration>) -> io::Result<()> {
        self.sock.set_read_timeout(wait)
    }

    fn wire(&self, to: SocketAddr) -> io::Result<SocketAddr> {
        match (to, self.dual) {
            (SocketAddr::V4(v4), true) => {
                Ok(SocketAddr::V6(SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0)))
            }
            (SocketAddr::V6(_), false) => Err(io::Error::new(io::ErrorKind::Unsupported, "IPv4-only socket")),
            _ => Ok(to),
        }
    }
}

/// IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) back to plain IPv4; everything else unchanged.
pub fn canonical(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::from((v4, v6.port())),
            None => addr,
        },
        v4 => v4,
    }
}

/// This machine's IPv6 address that routes to the internet (no packet is sent), if it has a global one.
pub fn global_ipv6() -> Option<Ipv6Addr> {
    let probe = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).ok()?;
    probe.connect("[2001:4860:4860::8888]:80").ok()?;
    match probe.local_addr().ok()? {
        SocketAddr::V6(v6) if is_global_v6(v6.ip()) => Some(*v6.ip()),
        _ => None,
    }
}

/// Global unicast (2000::/3) outside the documentation range. Link-local, unique-local, loopback, multicast and
/// IPv4-mapped addresses do not qualify.
pub fn is_global_v6(ip: &Ipv6Addr) -> bool {
    let s = ip.segments();
    (s[0] & 0xe000) == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_socket_talks_to_ipv4_and_reports_plain_ipv4() {
        let a = PathSocket::bind(1 << 16).unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").unwrap();
        a.send_to(b"hi", b.local_addr().unwrap()).unwrap();
        let mut buf = [0u8; 8];
        let (n, from) = b.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hi");
        b.send_to(b"yo", from).unwrap();
        a.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let (n, from) = a.recv_from(&mut buf).unwrap();
        assert_eq!((&buf[..n], from), (&b"yo"[..], b.local_addr().unwrap()));
    }

    #[test]
    fn dual_socket_reaches_ipv6_loopback() {
        let a = PathSocket::bind(1 << 16).unwrap();
        let Ok(b) = UdpSocket::bind("[::1]:0") else { return }; // no IPv6 stack on this machine
        assert!(a.is_dual());
        a.send_to(b"v6", b.local_addr().unwrap()).unwrap();
        let mut buf = [0u8; 8];
        b.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let n = b.recv_from(&mut buf).unwrap().0;
        assert_eq!(&buf[..n], b"v6");
    }

    #[test]
    fn only_global_ipv6_counts() {
        for (ip, global) in [
            ("2a02:1810::1", true),
            ("2001:db8::1", false),
            ("fe80::1", false),
            ("fd00::1", false),
            ("::1", false),
            ("::ffff:1.2.3.4", false),
            ("ff02::1", false),
        ] {
            assert_eq!(is_global_v6(&ip.parse().unwrap()), global, "{ip}");
        }
    }
}
