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
    assert!(open(&host, &mut replay, &seal(&viewer, 5, b"\x05late").unwrap()).is_some(), "in-window reorder rejected");
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

pub(crate) type Bulk = Option<BulkSink>;

/// A viewer and a host whose path runs through a fake UDP relay that loses every `drop_every`-th full-size
/// datagram; the returned flag stops the relay.
pub(crate) fn relayed_pair(
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
/// The timing-sensitive streams below take turns: run side by side (the bulk flood next to the 30 ms video
/// frames) they starved each other's retransmits and the video test failed now and then.
static STREAM_TESTS: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn lossy_stream(nack: bool, drop_every: u32, frames: usize) -> (usize, bool) {
    let _turn = STREAM_TESTS.lock();
    let inbox: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let sink = Arc::clone(&inbox);
    let (viewer, host, stop) = relayed_pair(nack, drop_every, Arc::new(move |f| sink.lock().push(f)), None, None);
    // Each frame starts with its number, so order and integrity can be checked.
    let frame = |n: usize| -> Vec<u8> { (0..150_000u32).map(|i| (i as usize * 7 + n) as u8).collect() };
    for n in 0..frames {
        assert!(host.send_video(&frame(n)));
        thread::sleep(Duration::from_millis(30));
    }
    // Late repairs still land; stop waiting as soon as every frame is in.
    let deadline = Instant::now() + Duration::from_secs(2);
    while inbox.lock().len() < frames && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
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
    let _turn = STREAM_TESTS.lock();
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
    let whole = got.len() == count && got.iter().enumerate().all(|(n, (k, p))| *k == bulk::MSG_BINARY && *p == msg(n));
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
    // Idle: 20 of 20. A retransmit can itself hit a dropped slot, or a busy test run delays the next request
    // past the next frame, so a frame or two may still be lost.
    assert!(with >= 17, "only {with} of 20 frames arrived with retransmits");
    assert!(ordered, "frames reached the decoder out of order or corrupt");
}

/// K3: with all UDP dropped, the path comes up over a direct TCP stream, and video and input flow on it.
#[test]
fn path_comes_up_over_tcp_when_udp_is_dropped() {
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
        nack: false,
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
    viewer.sock.block_udp.store(true, Ordering::SeqCst);
    host.sock.block_udp.store(true, Ordering::SeqCst);
    let signal = |rx: &std::sync::mpsc::Receiver<Outgoing>| loop {
        if let Ok(Outgoing::Message(ClientMessage::P2pSignal { candidate, .. })) =
            rx.recv_timeout(Duration::from_secs(5))
        {
            break candidate;
        }
    };
    // The host answers the viewer's handshake message, so its signal only exists after it got the viewer's.
    let from_viewer = signal(&rx_v);
    if !from_viewer.contains("\"c\":[\"") {
        eprintln!("no usable local address; skipping");
        return;
    }
    host.on_signal(&from_viewer);
    viewer.on_signal(&signal(&rx_h));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !(viewer.is_active() && host.is_active()) {
        assert!(Instant::now() < deadline, "path never came up over TCP");
        thread::sleep(Duration::from_millis(50));
    }
    let Some(Route::Direct(a)) = viewer.route() else { panic!("route {:?}", viewer.route()) };
    assert!(viewer.sock.tcp().has(a), "the path is up, but not over TCP");
    let jpeg: Vec<u8> = (0..150_000u32).map(|i| i as u8).collect();
    assert!(host.send_video(&jpeg));
    let deadline = Instant::now() + Duration::from_secs(3);
    while inbox.lock().is_empty() {
        assert!(Instant::now() < deadline, "frame not delivered over TCP");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(inbox.lock().pop().unwrap(), jpeg);
    assert!(viewer.send_input(&InputEvent::KeyDown { key_code: 65 }));
    assert_eq!(in_rx.recv_timeout(Duration::from_secs(2)).unwrap(), InputEvent::KeyDown { key_code: 65 });
    viewer.close();
    host.close();
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
