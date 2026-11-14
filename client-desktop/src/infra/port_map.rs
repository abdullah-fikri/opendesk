//! Asks the home router to forward a UDP port to this machine (NAT-PMP, then UPnP-IGD), so the partner can reach
//! the P2P socket even when hole punching fails (symmetric NAT on one side). The mapped public address becomes
//! one more P2P candidate. `OPENDESK_PORT_MAPPING=0` turns it off.
//!
//! Only the LAN is involved: NAT-PMP talks to the default gateway, UPnP to the device that answered the SSDP
//! search, and only when its description URL points back at that same device.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use crate::infra::logger;

/// Requested lease; renewed at half of it while the session lasts and removed when it ends.
pub const LEASE: Duration = Duration::from_secs(3600);
const DESCRIPTION: &str = "OpenDesk P2P";
const NATPMP_PORT: u16 = 5351;
const SSDP: &str = "239.255.255.250:1900";
const SSDP_WAIT: Duration = Duration::from_millis(1200);
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_HTTP_BYTES: usize = 256 << 10;

pub fn enabled() -> bool {
    std::env::var("OPENDESK_PORT_MAPPING").map_or(true, |v| v != "0")
}

/// A port forwarded by the router; remove it with [`Mapping::remove`].
#[derive(Debug)]
pub struct Mapping {
    /// Public address the partner can send to.
    pub external: SocketAddrV4,
    internal_port: u16,
    /// LAN address the router forwards to (UPnP renewals repeat it).
    local: Ipv4Addr,
    via: Via,
}

#[derive(Debug)]
enum Via {
    NatPmp(SocketAddrV4),
    Upnp(Control),
}

/// A router's WANIPConnection / WANPPPConnection control endpoint.
#[derive(Debug, Clone)]
struct Control {
    host: SocketAddrV4,
    path: String,
    service: String,
}

/// Maps `internal_port` (UDP, on `local`) to a public port. None when no router agrees or the router's own public
/// address is not public (another NAT above it, e.g. carrier-grade NAT).
pub fn map(internal_port: u16, local: Ipv4Addr) -> Option<Mapping> {
    let started = Instant::now();
    let gateway = default_gateway();
    let mapping = gateway
        .and_then(|gw| natpmp_map(SocketAddrV4::new(gw, NATPMP_PORT), internal_port, local))
        .or_else(|| upnp_map(internal_port, local));
    let Some(m) = mapping else {
        logger::info(
            "anynet.portmap",
            &format!(
                "No port mapping: the router answered neither NAT-PMP nor UPnP (gateway {}, {} ms).",
                gateway.map_or("unknown".into(), |g| g.to_string()),
                started.elapsed().as_millis()
            ),
        );
        return None;
    };
    let how = match &m.via {
        Via::NatPmp(gw) => format!("NAT-PMP at {}", gw.ip()),
        Via::Upnp(c) => format!("UPnP at {}", c.host),
    };
    if !is_public(m.external.ip()) {
        logger::info(
            "anynet.portmap",
            &format!(
                "Router mapped {} via {how}, but that is not a public address (NAT above the router); dropped.",
                m.external
            ),
        );
        m.remove();
        return None;
    }
    logger::info(
        "anynet.portmap",
        &format!(
            "Port mapped via {how}: {} -> {local}:{internal_port} ({} ms).",
            m.external,
            started.elapsed().as_millis()
        ),
    );
    Some(m)
}

impl Mapping {
    /// Extends the lease; false when the router no longer agrees.
    pub fn renew(&mut self) -> bool {
        match &self.via {
            Via::NatPmp(gw) => {
                natpmp_request(*gw, self.internal_port, self.external.port(), LEASE.as_secs() as u32).is_some()
            }
            Via::Upnp(c) => upnp_add(c, self.external.port(), self.internal_port, self.local).is_ok(),
        }
    }

    pub fn remove(self) {
        match &self.via {
            Via::NatPmp(gw) => {
                let _ = natpmp_request(*gw, self.internal_port, 0, 0);
            }
            Via::Upnp(c) => {
                let args = format!(
                    "<NewRemoteHost></NewRemoteHost><NewExternalPort>{}</NewExternalPort><NewProtocol>UDP</NewProtocol>",
                    self.external.port()
                );
                let _ = soap(c, "DeletePortMapping", &args);
            }
        }
        logger::info("anynet.portmap", &format!("Port mapping {} removed.", self.external));
    }
}

/// Public unicast IPv4: not private, CGNAT (100.64/10), loopback, link-local, multicast or reserved.
fn is_public(ip: &Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_documentation()
        || (a == 100 && (64..128).contains(&b))
        || a >= 240
        || a == 0)
}

// --- NAT-PMP (RFC 6886) ---

fn natpmp_map(gw: SocketAddrV4, internal_port: u16, local: Ipv4Addr) -> Option<Mapping> {
    let external_port = natpmp_request(gw, internal_port, internal_port, LEASE.as_secs() as u32)?;
    let ip = natpmp_exchange(gw, &[0, 0]).filter(|r| r.len() >= 12).map(|r| Ipv4Addr::new(r[8], r[9], r[10], r[11]))?;
    Some(Mapping { external: SocketAddrV4::new(ip, external_port), internal_port, local, via: Via::NatPmp(gw) })
}

/// Maps (or with `lifetime` 0, removes) a UDP port; returns the external port the router chose.
fn natpmp_request(gw: SocketAddrV4, internal_port: u16, suggested: u16, lifetime: u32) -> Option<u16> {
    let mut req = [0u8; 12];
    req[1] = 1; // map UDP
    req[4..6].copy_from_slice(&internal_port.to_be_bytes());
    req[6..8].copy_from_slice(&suggested.to_be_bytes());
    req[8..12].copy_from_slice(&lifetime.to_be_bytes());
    let r = natpmp_exchange(gw, &req)?;
    (r.len() >= 16).then(|| u16::from_be_bytes([r[10], r[11]]))
}

/// One request with the RFC's doubling retry (250, 500, 1000 ms); the reply's result code must be 0.
fn natpmp_exchange(gw: SocketAddrV4, req: &[u8]) -> Option<Vec<u8>> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect(gw).ok()?;
    let mut buf = [0u8; 32];
    for wait in [250, 500, 1000] {
        sock.send(req).ok()?;
        let _ = sock.set_read_timeout(Some(Duration::from_millis(wait)));
        if let Ok(n) = sock.recv(&mut buf)
            && n >= 4
            && buf[0] == 0
            && buf[1] == req[1] + 128
        {
            return (u16::from_be_bytes([buf[2], buf[3]]) == 0).then(|| buf[..n].to_vec());
        }
    }
    None
}

/// The IPv4 default gateway (next hop towards the internet).
#[cfg(windows)]
fn default_gateway() -> Option<Ipv4Addr> {
    #[repr(C)]
    struct MibIpForwardRow {
        dest: u32,
        mask: u32,
        policy: u32,
        next_hop: u32,
        if_index: u32,
        kind: u32,
        proto: u32,
        age: u32,
        next_hop_as: u32,
        metrics: [u32; 5],
    }
    #[link(name = "iphlpapi")]
    unsafe extern "system" {
        fn GetBestRoute(dest: u32, source: u32, row: *mut MibIpForwardRow) -> u32;
    }
    // SAFETY: plain-data struct the call fills; zeroed is a valid value.
    let mut row: MibIpForwardRow = unsafe { std::mem::zeroed() };
    // SAFETY: `row` is a valid out pointer for the duration of the call.
    let err = unsafe { GetBestRoute(u32::from_ne_bytes([8, 8, 8, 8]), 0, &mut row) };
    let gw = Ipv4Addr::from(row.next_hop.to_ne_bytes());
    (err == 0 && !gw.is_unspecified()).then_some(gw)
}

#[cfg(not(windows))]
fn default_gateway() -> Option<Ipv4Addr> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    table.lines().skip(1).find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 3 || cols[1] != "00000000" {
            return None;
        }
        let gw = u32::from_str_radix(cols[2], 16).ok()?;
        Some(Ipv4Addr::from(gw.to_le_bytes())).filter(|g| !g.is_unspecified())
    })
}

// --- UPnP IGD ---

fn upnp_map(internal_port: u16, local: Ipv4Addr) -> Option<Mapping> {
    let control = discover(local)?;
    // Same external port first; another device may hold it, so a few random ones after.
    let mut ports = vec![internal_port];
    let mut seed = [0u8; 6];
    let _ = getrandom::fill(&mut seed);
    ports.extend(seed.chunks(2).map(|c| 20_000 + u16::from_be_bytes([c[0], c[1]]) % 40_000));
    let external_port = ports.into_iter().find(|p| upnp_add(&control, *p, internal_port, local).is_ok())?;
    let reply = soap(&control, "GetExternalIPAddress", "").ok()?;
    let ip: Ipv4Addr = tag(&reply, "NewExternalIPAddress")?.trim().parse().ok()?;
    Some(Mapping { external: SocketAddrV4::new(ip, external_port), internal_port, local, via: Via::Upnp(control) })
}

/// AddPortMapping with a lease; routers that only take permanent mappings (error 725) get lease 0.
fn upnp_add(c: &Control, external_port: u16, internal_port: u16, local: Ipv4Addr) -> Result<(), u32> {
    let args = |lease: u64| {
        format!(
            "<NewRemoteHost></NewRemoteHost><NewExternalPort>{external_port}</NewExternalPort>\
             <NewProtocol>UDP</NewProtocol><NewInternalPort>{internal_port}</NewInternalPort>\
             <NewInternalClient>{local}</NewInternalClient><NewEnabled>1</NewEnabled>\
             <NewPortMappingDescription>{DESCRIPTION}</NewPortMappingDescription>\
             <NewLeaseDuration>{lease}</NewLeaseDuration>"
        )
    };
    match soap(c, "AddPortMapping", &args(LEASE.as_secs())) {
        Err(725) => soap(c, "AddPortMapping", &args(0)).map(|_| ()),
        other => other.map(|_| ()),
    }
}

/// SSDP search on the LAN of `local`; the first gateway with a WAN connection service.
fn discover(local: Ipv4Addr) -> Option<Control> {
    let sock = UdpSocket::bind((local, 0)).ok()?;
    for st in [
        "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
        "urn:schemas-upnp-org:device:InternetGatewayDevice:2",
        "urn:schemas-upnp-org:service:WANIPConnection:1",
    ] {
        let msg = format!("M-SEARCH * HTTP/1.1\r\nHOST: {SSDP}\r\nST: {st}\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\n\r\n");
        let _ = sock.send_to(msg.as_bytes(), SSDP);
    }
    let deadline = Instant::now() + SSDP_WAIT;
    let mut seen = Vec::new();
    let mut buf = [0u8; 2048];
    while let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
        let _ = sock.set_read_timeout(Some(left));
        let Ok((n, SocketAddr::V4(from))) = sock.recv_from(&mut buf) else { break };
        let reply = String::from_utf8_lossy(&buf[..n]);
        let Some(location) = header(&reply, "location") else { continue };
        // Only descriptions served by the device that answered.
        let Some((host, path)) = parse_url(&location).filter(|(h, _)| h.ip() == from.ip()) else { continue };
        if seen.contains(&(host, path.clone())) {
            continue;
        }
        seen.push((host, path.clone()));
        if let Some(c) = describe(host, &path) {
            return Some(c);
        }
    }
    None
}

/// Reads a device description and finds its WAN connection service.
fn describe(host: SocketAddrV4, path: &str) -> Option<Control> {
    let (status, body) = http(host, &format!("GET {path} HTTP/1.0\r\nHost: {host}\r\n\r\n"))?;
    if status != 200 {
        return None;
    }
    control_of(&body, host)
}

fn control_of(description: &str, host: SocketAddrV4) -> Option<Control> {
    let base = tag(description, "URLBase").and_then(|b| parse_url(b.trim())).map_or(host, |(h, _)| h);
    description.split("<service>").skip(1).find_map(|block| {
        let service = tag(block, "serviceType")?.trim().to_string();
        if !(service.contains(":WANIPConnection:") || service.contains(":WANPPPConnection:")) {
            return None;
        }
        let url = tag(block, "controlURL")?.trim().to_string();
        let (host, path) = match parse_url(&url) {
            Some((h, p)) if h.ip() == host.ip() => (h, p),
            Some(_) => return None, // only the device that answered
            None if url.starts_with('/') => (base, url),
            None => (base, format!("/{url}")),
        };
        Some(Control { host, path, service })
    })
}

/// One SOAP action; the response body, or the UPnP error code (0 when there is none).
fn soap(c: &Control, action: &str, args: &str) -> Result<String, u32> {
    let body = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action} xmlns:u=\"{}\">{args}</u:{action}></s:Body></s:Envelope>",
        c.service
    );
    let request = format!(
        "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Type: text/xml; charset=\"utf-8\"\r\n\
         SOAPAction: \"{}#{action}\"\r\nContent-Length: {}\r\n\r\n{body}",
        c.path,
        c.host,
        c.service,
        body.len()
    );
    match http(c.host, &request) {
        Some((200, reply)) => Ok(reply),
        Some((_, reply)) => Err(tag(&reply, "errorCode").and_then(|e| e.trim().parse().ok()).unwrap_or(0)),
        None => Err(0),
    }
}

/// HTTP/1.0 (no chunked bodies) over a short-lived connection: status code and body.
fn http(host: SocketAddrV4, request: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(&SocketAddr::V4(host), HTTP_TIMEOUT).ok()?;
    let _ = stream.set_read_timeout(Some(HTTP_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HTTP_TIMEOUT));
    stream.write_all(request.as_bytes()).ok()?;
    let mut raw = Vec::new();
    let _ = stream.take(MAX_HTTP_BYTES as u64).read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw);
    let status = text.split_whitespace().nth(1)?.parse().ok()?;
    let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_string();
    Some((status, body))
}

/// A header's value (case-insensitive name).
fn header(reply: &str, name: &str) -> Option<String> {
    reply.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
    })
}

/// Text inside the first `<name>...</name>`, also with a namespace prefix (`<u:name>`).
fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = xml.match_indices('<').find_map(|(i, _)| {
        let rest = &xml[i + 1..];
        let end = rest.find('>')?;
        let tag_name = rest[..end].split_whitespace().next()?;
        let local = tag_name.rsplit(':').next()?;
        (local == name && !tag_name.starts_with('/')).then_some(i + 1 + end + 1)
    })?;
    let close = xml[open..].find("</")?;
    Some(&xml[open..open + close])
}

/// `http://a.b.c.d[:port]/path` with an IPv4 host; names are not resolved.
fn parse_url(url: &str) -> Option<(SocketAddrV4, String)> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("HTTP://"))?;
    let (authority, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(a, p)| (a, format!("/{p}")));
    let host = match authority.split_once(':') {
        Some((ip, port)) => SocketAddrV4::new(ip.parse().ok()?, port.parse().ok()?),
        None => SocketAddrV4::new(authority.parse().ok()?, 80),
    };
    Some((host, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCRIPTION_XML: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceList><device><deviceList><device>
<serviceList>
<service><serviceType>urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1</serviceType>
<controlURL>/ctl/CmnIfCfg</controlURL></service>
<service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
<serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId><controlURL>/ctl/IPConn</controlURL></service>
</serviceList></device></deviceList></device></deviceList></device></root>"#;

    #[test]
    fn finds_the_wan_connection_control_url() {
        let host: SocketAddrV4 = "192.168.1.1:5000".parse().unwrap();
        let c = control_of(DESCRIPTION_XML, host).unwrap();
        assert_eq!((c.host, c.path.as_str()), (host, "/ctl/IPConn"));
        assert_eq!(c.service, "urn:schemas-upnp-org:service:WANIPConnection:1");
    }

    #[test]
    fn control_urls_pointing_elsewhere_are_refused() {
        let xml = DESCRIPTION_XML.replace("/ctl/IPConn", "http://10.9.9.9:80/x");
        assert!(control_of(&xml, "192.168.1.1:5000".parse().unwrap()).is_none());
    }

    #[test]
    fn soap_replies_and_ssdp_headers_parse() {
        let reply = r#"<s:Envelope><s:Body><u:GetExternalIPAddressResponse xmlns:u="x">
<NewExternalIPAddress>203.0.113.9</NewExternalIPAddress></u:GetExternalIPAddressResponse></s:Body></s:Envelope>"#;
        assert_eq!(tag(reply, "NewExternalIPAddress"), Some("203.0.113.9"));
        assert_eq!(tag("<UPnPError><errorCode>718</errorCode></UPnPError>", "errorCode"), Some("718"));
        let ssdp =
            "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nLocation: http://192.168.1.1:5000/rootDesc.xml\r\n\r\n";
        assert_eq!(header(ssdp, "location").as_deref(), Some("http://192.168.1.1:5000/rootDesc.xml"));
        assert_eq!(parse_url("http://192.168.1.1/a"), Some(("192.168.1.1:80".parse().unwrap(), "/a".into())));
        assert_eq!(parse_url("http://router.local/a"), None);
    }

    #[test]
    fn only_public_addresses_are_worth_offering() {
        for (ip, public) in [
            ("203.0.114.9", true),
            ("8.8.8.8", true),
            ("192.168.1.2", false),
            ("100.72.1.1", false),
            ("10.0.0.1", false),
        ] {
            assert_eq!(is_public(&ip.parse().unwrap()), public, "{ip}");
        }
    }

    /// A fake NAT-PMP gateway on loopback answers the map and address requests.
    #[test]
    fn natpmp_maps_through_a_gateway() {
        let gw = UdpSocket::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(at) = gw.local_addr().unwrap() else { panic!() };
        let server = std::thread::spawn(move || {
            let mut buf = [0u8; 16];
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (_, from) = gw.recv_from(&mut buf).unwrap();
                seen.push(buf);
                let reply: Vec<u8> = match buf[1] {
                    0 => [&[0, 128, 0, 0, 0, 0, 0, 1][..], &[198, 51, 100, 7]].concat(),
                    _ => [&[0, 129, 0, 0, 0, 0, 0, 1][..], &buf[4..6], &50_000u16.to_be_bytes(), &buf[8..12]].concat(),
                };
                gw.send_to(&reply, from).unwrap();
            }
            seen
        });
        let m = natpmp_map(at, 12_345, Ipv4Addr::LOCALHOST).unwrap();
        assert_eq!(m.external, "198.51.100.7:50000".parse().unwrap());
        let seen = server.join().unwrap();
        // Map request: UDP, internal 12345, suggested 12345, lease one hour.
        assert_eq!(&seen[0][..12], &[0, 1, 0, 0, 0x30, 0x39, 0x30, 0x39, 0, 0, 0x0E, 0x10]);
    }

    /// Real router on this network, if any: `cargo test router_maps_a_port -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn router_maps_a_port() {
        let probe = UdpSocket::bind("0.0.0.0:0").unwrap();
        probe.connect("8.8.8.8:80").unwrap();
        let SocketAddr::V4(local) = probe.local_addr().unwrap() else { panic!() };
        println!("gateway {:?}", default_gateway());
        let m = map(local.port(), *local.ip());
        println!("mapping {m:?}");
        if let Some(m) = m {
            m.remove();
        }
    }
}
