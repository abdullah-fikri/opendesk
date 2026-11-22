use super::*;

#[test]
fn relay_silence_past_the_relays_own_timeout_counts_as_a_dead_socket() {
    // Pings arrive every 15 s: one or two missed ones are not a dead socket...
    assert!(!relay_silent(Duration::from_secs(30)));
    // ...three are: reconnect before the relay's own 60 s drop, so the device never shows offline.
    assert!(!relay_silent(Duration::from_secs(44)));
    assert!(relay_silent(RELAY_SILENCE_LIMIT));
    assert!(RELAY_SILENCE_LIMIT > Duration::from_secs(30) && RELAY_SILENCE_LIMIT < Duration::from_secs(60));
}

#[test]
fn a_connection_the_relay_never_registers_is_retried() {
    assert!(!registration_overdue(false, Duration::from_secs(5)), "the ack is on its way");
    assert!(registration_overdue(false, REGISTER_TIMEOUT));
    assert!(!registration_overdue(true, Duration::from_secs(3600)), "registered connections stay");
}

#[test]
fn send_buffer_shrinks_only_while_hosted_video_uses_the_socket() {
    // Viewers and idle machines keep the OS default until they host over the relay.
    assert_eq!(wanted_send_buffer(false, None, None), None);
    assert_eq!(wanted_send_buffer(true, None, None), Some(HOSTING_SEND_BUFFER));
    // After the session (or once P2P carries the video) transfers get a large buffer back.
    assert_eq!(wanted_send_buffer(false, Some(HOSTING_SEND_BUFFER), None), Some(TRANSFER_SEND_BUFFER));
    assert_eq!(wanted_send_buffer(true, Some(TRANSFER_SEND_BUFFER), None), Some(HOSTING_SEND_BUFFER));
    // OPENDESK_WS_SNDBUF: a size (with a floor), 0 leaves the socket alone, junk is ignored.
    assert_eq!(wanted_send_buffer(true, None, Some("1048576")), Some(1 << 20));
    assert_eq!(wanted_send_buffer(true, None, Some("1000")), Some(MIN_SEND_BUFFER));
    assert_eq!(wanted_send_buffer(true, None, Some("0")), None);
    assert_eq!(wanted_send_buffer(true, None, Some("big")), Some(HOSTING_SEND_BUFFER));
}

#[test]
fn pending_moves_keep_the_newest_position_per_host_until_taken() {
    let mut moves = PendingMoves::default();
    moves.set("a".into(), 0.1, 0.1);
    moves.set("b".into(), 0.5, 0.5);
    moves.set("a".into(), 0.2, 0.3);
    // A button event for "a" takes its move first; "b" stays coalesced for the end of the tick.
    assert_eq!(moves.take("a"), Some((0.2, 0.3)));
    assert_eq!(moves.take("a"), None, "a move already sent before the button is not sent again after it");
    assert_eq!(moves.0, vec![("b".to_string(), 0.5, 0.5)]);
}

#[test]
fn partner_countdown_rounds_up_and_never_shows_zero() {
    let now = Instant::now();
    assert_eq!(partner_seconds_left(now + Duration::from_secs(30), now), 30);
    assert_eq!(partner_seconds_left(now + Duration::from_millis(29_100), now), 30);
    assert_eq!(partner_seconds_left(now + Duration::from_millis(400), now), 1);
    assert_eq!(partner_seconds_left(now, now + Duration::from_secs(2)), 1);
}

#[test]
fn offline_drops_late_media_but_holds_control_messages() {
    assert!(is_live(&Outgoing::MouseMove("1".into(), 0.5, 0.5)));
    assert!(is_live(&Outgoing::VideoFrame(vec![1])) && is_live(&Outgoing::Audio(vec![1])));
    assert!(is_live(&Outgoing::Message(ClientMessage::input("1", InputEvent::MouseMove { norm_x: 0.0, norm_y: 0.0 }))));
    assert!(!is_live(&Outgoing::Message(ClientMessage::disconnect("1"))), "closing a tab must reach the relay");
    let file = Outgoing::FileChunk { target: "1".into(), frame: vec![1], status: None, mark: None };
    assert!(!is_live(&file));
}

#[test]
fn idle_sleep_stays_at_one_ms_during_sessions_and_recent_traffic() {
    assert_eq!(idle_sleep(Duration::ZERO, false), BUSY_SLEEP);
    assert_eq!(idle_sleep(Duration::from_millis(1999), false), BUSY_SLEEP);
    assert_eq!(idle_sleep(Duration::from_secs(3600), true), BUSY_SLEEP, "a session never backs off");
}

/// One file stream message from a P2P path: sender, kind, payload.
type BulkIn = (String, u8, Vec<u8>);

/// A connection with no relay socket, carrying `links`; its outbox is never read.
fn test_conn(ctx: Ctx, links: Vec<Arc<P2p>>, bulk: (Sender<BulkIn>, Receiver<BulkIn>)) -> Connection {
    let (input_tx, input_rx) = mpsc::channel();
    let (_keep, outbox) = mpsc::channel();
    let (lane_tx, lane_rx) = mpsc::channel();
    Connection {
        lane: None,
        lane_gen: 0,
        lane_in_tx: lane_tx,
        lane_in: lane_rx,
        lane_incoming: Vec::new(),
        lane_idle: None,
        ctx,
        outbox,
        files: VecDeque::new(),
        pacer: FilePacer::new(),
        receiver: FileReceiver::new(),
        p2p_input_tx: input_tx,
        p2p_input: input_rx,
        bulk_in_tx: bulk.0,
        bulk_in: bulk.1,
        channel_in_tx: mpsc::channel().0,
        channel_in: mpsc::channel().1,
        channels: Vec::new(),
        channel_pending: Vec::new(),
        routed_now: Path::Relay,
        file_routes: FileRoutes::default(),
        p2p_incoming: Vec::new(),
        p2p_acks: VecDeque::new(),
        links,
        e2e: Vec::new(),
        monitor_pending: None,
        monitor_in_flight: false,
        held: VecDeque::new(),
        busy: false,
        last_rx: Instant::now(),
        signals: SignalPacer::default(),
        resume: Resume::default(),
        partner_waits: Vec::new(),
        url: String::new(),
        quiet: false,
    }
}

/// The next file verdict the receiving side queued for the relay socket.
fn next_ack(outbox: &Receiver<Outgoing>) -> (String, FilePayload) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(Outgoing::Message(ClientMessage::RelayFile { target_clean, payload })) =
            outbox.recv_timeout(Duration::from_millis(100))
            && payload.action == FileAction::Ack
        {
            return (target_clean, payload);
        }
    }
    panic!("no ack queued");
}

/// The verdict on a transfer that came over the P2P path goes back over that path and reaches the waiting
/// sender: the relay has no grant for such a transfer and dropped a relayed ack, so every P2P transfer
/// failed after two minutes with "no confirmation from the receiver". A transfer the path did not carry
/// keeps the relay.
#[test]
fn verdicts_on_p2p_transfers_go_back_over_the_p2p_path() {
    const VIEWER: &str = "111111111";
    const HOST: &str = "222222222";
    // The viewer sends; what reaches it over the path is handed to its connection like the live loop does.
    let (viewer_tx, viewer_rx) = mpsc::channel::<BulkIn>();
    let sink_tx = parking_lot::Mutex::new(viewer_tx.clone());
    let viewer_bulk: p2p::tests::Bulk = Some(Arc::new(move |kind, payload| {
        let _ = sink_tx.lock().send((HOST.to_string(), kind, payload));
    }));
    let (viewer_link, host_link, stop) = p2p::tests::relayed_pair(false, 0, Arc::new(|_| {}), viewer_bulk, None);
    let (host_ctx, host_out) = crate::service::test_support::context_with_outbox();
    let mut host = test_conn(host_ctx, vec![Arc::clone(&host_link)], mpsc::channel());
    let mut viewer =
        test_conn(crate::service::test_support::context(), vec![Arc::clone(&viewer_link)], (viewer_tx, viewer_rx));

    let mut verdict_reaches_sender = |host: &mut Connection, id: &str, sent: FilePayload| -> bool {
        let (ack_tx, ack_rx) = mpsc::channel();
        file_transfer::expect_ack(id, ack_tx);
        host.on_bulk(VIEWER, bulk::MSG_JSON, &serde_json::to_vec(&sent).unwrap());
        let (target, ack) = next_ack(&host_out);
        assert_eq!((target.as_str(), ack.id.as_str(), ack.ok), (VIEWER, id, Some(false)));
        assert!(host.ack_over_p2p(&target, &ack), "verdict on {id} did not take the P2P path");
        let (from, kind, payload) = viewer.bulk_in.recv_timeout(Duration::from_secs(5)).expect("nothing arrived");
        viewer.on_bulk(&from, kind, &payload);
        ack_rx.recv_timeout(Duration::from_secs(1)) == Ok(false)
    };
    // A rejected start (no name) is answered at once; an `end` the receiver knows nothing of is NACKed too.
    assert!(verdict_reaches_sender(&mut host, "ft_p2pack_start", FilePayload::start("ft_p2pack_start", "", 5)));
    assert!(verdict_reaches_sender(
        &mut host,
        "ft_p2pack_end",
        FilePayload::end("ft_p2pack_end", "a.txt", "00".into(), false)
    ));
    assert!(host.p2p_incoming.is_empty() && host.p2p_acks.is_empty(), "one verdict per transfer");

    // Not carried by the path (a relayed transfer), or already answered: the relay, as before.
    let relayed = FilePayload::ack("ft_relayed", true);
    assert!(!host.ack_over_p2p(VIEWER, &relayed));
    assert!(!host.ack_over_p2p(VIEWER, &FilePayload::ack("ft_p2pack_end", false)));

    // An `end` whose path then drops: its verdict can no longer go that way.
    host.on_bulk(
        VIEWER,
        bulk::MSG_JSON,
        &serde_json::to_vec(&FilePayload::end("ft_cut", "b", "00".into(), false)).unwrap(),
    );
    let (target, ack) = next_ack(&host_out);
    host.on_bulk(VIEWER, p2p::BULK_LOST, &[]);
    assert!(!host.ack_over_p2p(&target, &ack), "forgotten with its path");

    stop.store(true, Ordering::SeqCst);
    viewer_link.close();
    host_link.close();
}

#[test]
fn verdicts_remembered_for_the_p2p_path_are_bounded() {
    let mut host = test_conn(crate::service::test_support::context(), Vec::new(), mpsc::channel());
    for n in 0..MAX_P2P_ACKS + 10 {
        let end = FilePayload::end(&format!("ft_b{n}"), "x", "00".into(), false);
        host.on_bulk("111111111", bulk::MSG_JSON, &serde_json::to_vec(&end).unwrap());
    }
    assert_eq!(host.p2p_acks.len(), MAX_P2P_ACKS);
    assert_eq!(host.p2p_acks.front().map(String::as_str), Some("111111111:ft_b10"), "oldest dropped first");
    // No link to that partner: the caller falls back to the relay.
    assert!(!host.ack_over_p2p("111111111", &FilePayload::ack("ft_b20", true)));
}

#[test]
fn idle_sleep_grows_gradually_to_the_cap() {
    let steps: Vec<u128> =
        [2, 3, 4, 5, 60].iter().map(|s| idle_sleep(Duration::from_secs(*s), false).as_millis()).collect();
    assert_eq!(steps, vec![2, 5, 10, 20, 20]);
    assert!(idle_sleep(Duration::MAX, false) <= IDLE_MAX_SLEEP);
}

/// Messages from the relay file lane feed the same handlers as the main socket's: a rejected start and a chunk of
/// an unknown transfer are answered with a NACK over the relay (which holds the grant), events of an earlier lane
/// are ignored, and a lost lane forgets what ran on it.
#[test]
fn lane_messages_take_the_relay_handlers_and_a_lost_lane_cuts_them() {
    const VIEWER: &str = "111111111";
    let (ctx, out) = crate::service::test_support::context_with_outbox();
    let mut host = test_conn(ctx, Vec::new(), mpsc::channel());

    let start = FilePayload::start("ft_lane1", "", 5);
    let text = serde_json::json!({ "type": "file-transfer", "fromClean": VIEWER, "payload": start }).to_string();
    host.on_lane_text(&text);
    let (target, ack) = next_ack(&out);
    assert_eq!((target.as_str(), ack.id.as_str(), ack.ok), (VIEWER, "ft_lane1", Some(false)));
    assert!(!host.ack_over_p2p(&target, &ack), "a lane transfer's verdict goes over the relay");
    assert_eq!(host.lane_incoming, vec![format!("{VIEWER}:ft_lane1")]);

    // Binaries: untagged chunks, non-file data and chunks of unknown transfers are dropped without a verdict; the
    // next verdict is the one for an `end` nobody started.
    host.on_lane_binary(&binary::file_chunk("ft_nolane", 0, b"abc").unwrap());
    host.on_lane_binary(&[0xFF, 0xD8, 1, 2, 3]);
    host.on_lane_binary(&binary::tag(VIEWER, &binary::file_chunk("ft_unknown", 0, b"abc").unwrap()).unwrap());
    let end = FilePayload::end("ft_lane_end", "a.txt", "00".into(), false);
    host.on_lane_text(&serde_json::json!({ "type": "file-transfer", "fromClean": VIEWER, "payload": end }).to_string());
    let (_, ack) = next_ack(&out);
    assert_eq!((ack.id.as_str(), ack.ok), ("ft_lane_end", Some(false)));
    // Anything but `file-transfer` is ignored on a lane.
    host.on_lane_text(r#"{"type":"session-closed","message":"spoofed"}"#);

    // Events of an earlier lane are stale.
    host.lane_gen = 2;
    host.lane_in_tx.send((1, file_lane::LaneEvent::Lost)).unwrap();
    host.drain_lane();
    assert_eq!(host.lane_incoming.len(), 1, "stale loss ignored");

    // The relay reports the transfer cut by a lane on the other side.
    host.on_file_lane_lost(VIEWER, &["ft_lane1".to_string()]);
    assert!(host.lane_incoming.is_empty());

    // Our own lane drops: transfers we started on it are forgotten (and resume elsewhere).
    host.file_routes.route(VIEWER, "ft_out", None, Step::Start, || Path::Lane);
    host.lane_in_tx.send((2, file_lane::LaneEvent::Lost)).unwrap();
    host.drain_lane();
    assert!(!host.file_routes.uses(Path::Lane));
    // Without a lane nothing is sent on it, and the caller learns at once.
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f = Arc::clone(&failed);
    assert!(!host.send_lane(VIEWER, bulk::MSG_JSON, b"{}", Box::new(move |ok| f.store(!ok, Ordering::SeqCst))));
    assert!(failed.load(Ordering::SeqCst));
    assert_eq!(host.file_path_to(VIEWER), Path::Relay);
}
