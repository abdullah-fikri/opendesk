//! Address candidates: which ones are acceptable, how they are signalled, and STUN discovery.

use super::*;

/// Only public or private IPv4 unicast and global IPv6 on non-system ports, so a hostile candidate list cannot aim
/// probes at local services.
pub(super) fn valid_candidate(addr: &SocketAddr) -> bool {
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
pub(super) fn parse_candidates(v: &Value) -> Vec<SocketAddr> {
    v.get("c")
        .and_then(|c| c.as_array())
        .map(|list| {
            list.iter().filter_map(|s| s.as_str()?.parse().ok()).filter(valid_candidate).take(MAX_CANDIDATES).collect()
        })
        .unwrap_or_default()
}

/// A `p2p-signal` payload: v1 carries this path's `Noise_NN` message, v2 (with e2e) only candidates. A refresh
/// (`"r": 1`, after a session resume) only replaces the candidates of a path whose handshake is done.
pub(super) fn signal_json(e2e: bool, hs_msg: &[u8], cands: &[SocketAddr], refresh: bool) -> String {
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
pub(super) fn refresh_candidates(candidate: &str) -> Option<Vec<SocketAddr>> {
    let v = serde_json::from_str::<Value>(candidate).ok()?;
    (v.get("r").and_then(Value::as_u64) == Some(1)).then(|| parse_candidates(&v))
}

/// Builds a STUN Binding request (RFC 5389).
pub(super) fn stun_request(txid: &[u8; 12]) -> [u8; 20] {
    let mut req = [0u8; 20];
    req[0..2].copy_from_slice(&0x0001u16.to_be_bytes());
    req[4..8].copy_from_slice(&0x2112_A442u32.to_be_bytes());
    req[8..20].copy_from_slice(txid);
    req
}

/// Extracts the IPv4 XOR-MAPPED-ADDRESS from a Binding success response matching `txid`.
pub(super) fn parse_stun_response(buf: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
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

pub(super) fn stun_query(sock: &PathSocket, server: &str) -> Option<SocketAddr> {
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

pub(super) fn gather_candidates(sock: &PathSocket) -> Vec<SocketAddr> {
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

/// This machine's LAN IPv4 address among the candidates (the one a router forwards to).
pub(super) fn lan_ipv4(cands: &[SocketAddr]) -> Option<Ipv4Addr> {
    cands.iter().find_map(|c| match c {
        SocketAddr::V4(v4) if v4.ip().is_private() => Some(*v4.ip()),
        _ => None,
    })
}
