//! The relay connection: connects and registers, pumps the outbound queue (choosing the P2P path when one is
//! up), reads incoming messages and reconnects after a failure.
//!
//! One thread runs this loop for the life of the process. Every tick drains the outbound queue, flushes the
//! socket (which opens the video backpressure gate), drains all readable messages and sleeps 1 ms; with no
//! session and no traffic for a while the pause grows to `IDLE_MAX_SLEEP`, cut short by a new outbound message.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::domain::performance::Performance;
use crate::domain::settings::{self, normalize_gateway_url};
use crate::domain::{capability, frame_rate};
use crate::infra::e2e::{self, E2e};
use crate::infra::p2p::{self, P2p};
use crate::infra::{autostart, device, logger, power, storage, virtual_clipboard, window};
use crate::protocol::{ClientMessage, FilePayload, InputEvent, Outgoing, ServerMessage, binary};
use crate::service::context::Ctx;
use crate::service::file_transfer::{self, FileReceiver, IN_FLIGHT_CHUNKS, chunk_units};
use crate::service::ui_port::RelayStatus;
use crate::service::viewer::decoder;

use super::signal_pacer::SignalPacer;

pub(super) type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Pause before reconnecting after a failed connect.
const RETRY_DELAY: Duration = Duration::from_millis(2000);
/// Pause after losing an established connection.
const RECONNECT_DELAY: Duration = Duration::from_millis(1500);
/// Pause before the connection loop starts over after a panic.
const PANIC_RESTART_DELAY: Duration = Duration::from_secs(2);
/// File messages sent per tick, in `chunk_units`, while a screen session runs (keeps video and voice latency
/// low)...
const FILE_BURST_IN_SESSION: usize = 8;
/// ...and without one (full-speed background transfer).
const FILE_BURST_IDLE: usize = 64;
/// Local relay probed when the configured relay is remote (avoids tunnel hairpinning on the relay machine).
const LOCAL_RELAY: &str = "127.0.0.1:8088";
/// Pause between ticks while a session runs or traffic flowed recently.
const BUSY_SLEEP: Duration = Duration::from_millis(1);
/// Quiet time before the pause starts to grow...
const IDLE_AFTER: Duration = Duration::from_secs(2);
/// ...up to this (far below the relay's 15 s ping interval).
const IDLE_MAX_SLEEP: Duration = Duration::from_millis(20);
/// How often the loop checks whether the main window is on screen.
const WINDOW_CHECK: Duration = Duration::from_millis(500);

/// Starts the connection thread. `outbox` is the receiving end of `ctx.outbox`.
pub fn spawn(ctx: Ctx, outbox: Receiver<Outgoing>) {
    crate::infra::thread::spawn_named("relay-conn", move || {
        let (input_tx, input_rx) = mpsc::channel();
        let mut conn = Connection {
            ctx,
            outbox,
            files: VecDeque::new(),
            receiver: FileReceiver::new(),
            p2p_input_tx: input_tx,
            p2p_input: input_rx,
            links: Vec::new(),
            e2e: Vec::new(),
            monitor_pending: None,
            monitor_in_flight: false,
            stashed: None,
            busy: false,
            signals: SignalPacer::default(),
        };
        // This process idles under EcoQoS until a session or traffic needs full speed.
        power::arm();
        // The app is useless without this loop: after a panic, clean up like a lost socket and start over.
        let pump = |conn: &mut Connection| loop {
            conn.run_once();
        };
        let recover = |conn: &mut Connection| {
            conn.on_connection_lost();
            conn.on_connection_ended();
        };
        crate::infra::thread::restart_on_panic("Relay connection loop", PANIC_RESTART_DELAY, &mut conn, pump, recover);
    });
}

/// One file message ready for the socket: JSON text or a binary chunk (already tagged for its host).
struct QueuedFile {
    message: Message,
    /// Its weight in `IN_FLIGHT_CHUNKS`.
    units: usize,
    status: Option<String>,
}

/// State of the relay connection thread.
pub(super) struct Connection {
    pub(super) ctx: Ctx,
    outbox: Receiver<Outgoing>,
    /// File messages waiting their turn behind video and voice.
    files: VecDeque<QueuedFile>,
    pub(super) receiver: FileReceiver,
    /// Input that arrived over a P2P path (host side).
    pub(super) p2p_input_tx: Sender<InputEvent>,
    p2p_input: Receiver<InputEvent>,
    /// One direct path per session: the hosted viewer, or each viewed host.
    pub(super) links: Vec<Arc<P2p>>,
    /// One end-to-end channel per session whose partner lists `e2e`; same lifetime as `links`.
    pub(super) e2e: Vec<Arc<E2e>>,
    /// Newest monitor-wall thumbnail not sent yet, and whether the previous one is still in the socket.
    monitor_pending: Option<Vec<u8>>,
    monitor_in_flight: bool,
    /// Message that ended an idle wait; `pump_outbox` sends it before the rest of the queue.
    stashed: Option<Outgoing>,
    /// Something was sent or received this tick.
    busy: bool,
    /// `p2p-signal` messages spaced out per partner (the relay drops ones that follow too closely).
    signals: SignalPacer,
}

impl Connection {
    /// One connection lifetime: connect, register, pump until it drops or a reconnect is requested.
    fn run_once(&mut self) {
        let ctx = Arc::clone(&self.ctx);
        ctx.relay.reconnect.store(false, Ordering::SeqCst);
        ctx.relay.online.store(false, Ordering::SeqCst);
        let url = resolve_url(&ctx.relay.gateway_url.lock());
        logger::info("relay", &format!("Connecting to relay {url}"));
        ctx.ui.relay_status(RelayStatus::Connecting);

        let mut socket = match tungstenite::connect(&url) {
            Ok((s, _)) => s,
            Err(e) => {
                logger::warn("relay", &format!("Connection failed: {e}"));
                ctx.ui.relay_status(RelayStatus::Offline { reason: None });
                thread::sleep(RETRY_DELAY);
                return;
            }
        };
        let _ = socket.send(Message::Text(self.register_message().to_json().into()));
        let _ = socket.flush();
        set_low_latency(&mut socket);
        logger::info("relay", "Connection established.");
        ctx.ui.relay_status(RelayStatus::Registering);

        ctx.host.can_send_frame.open();
        ctx.viewers.set_multi(false);
        let mut lost = false;
        let mut video_in_flight = false;
        let mut last_expire = Instant::now();
        let mut last_path_update = Instant::now();
        let backoff = idle_backoff_enabled();
        let mut last_traffic = Instant::now();
        let mut awake: Option<power::Awake> = None;
        let (mut window_shown, mut last_window_check) = (false, Instant::now() - WINDOW_CHECK);

        while !ctx.relay.reconnect.load(Ordering::SeqCst) && !lost {
            let partner = ctx.partner();
            self.prune_links(&partner);
            if last_path_update.elapsed() >= Duration::from_millis(250) {
                last_path_update = Instant::now();
                for p in self.links.iter().filter(|p| !p.is_host()) {
                    if let Some(s) = ctx.viewers.get(&p.partner) {
                        let route = p.route();
                        s.p2p_active.store(route.is_some(), Ordering::Relaxed);
                        s.p2p_relayed.store(matches!(route, Some(p2p::Route::Relayed(_))), Ordering::Relaxed);
                    }
                }
                let hosted_p2p = self.links.iter().any(|p| p.is_host() && p.is_active());
                ctx.host.p2p_active.store(hosted_p2p, Ordering::Relaxed);
                for e in self.e2e.iter().filter(|e| !e.is_host()) {
                    if let Some(s) = ctx.viewers.get(&e.partner) {
                        s.e2e_active.store(e.is_on(), Ordering::Relaxed);
                    }
                }
            }

            video_in_flight |= self.pump_outbox(&mut socket, &partner);
            if !video_in_flight {
                // Thumbnails only fill gaps: they never queue in front of the hosted session's video.
                self.pump_monitor(&mut socket);
            }
            let mut status = self.pump_files(&mut socket, partner.is_empty());
            if last_expire.elapsed() >= Duration::from_secs(1) {
                last_expire = Instant::now();
                status = self.receiver.expire_stale(ctx.ui.as_ref(), last_expire).or(status);
            }
            if let Some(status) = status {
                ctx.ui.status(status);
            }

            // Release the video gate only once the previous frame has fully left the socket.
            match socket.flush() {
                Ok(()) => {
                    if std::mem::take(&mut video_in_flight) {
                        ctx.host.can_send_frame.open();
                    }
                    self.monitor_in_flight = false;
                }
                Err(tungstenite::Error::Io(ref e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    self.busy = true;
                }
                Err(_) => {
                    ctx.host.can_send_frame.open();
                    self.monitor_in_flight = false;
                }
            }

            lost = !self.read_all(&mut socket);
            if lost {
                self.on_connection_lost();
                thread::sleep(RECONNECT_DELAY);
                break;
            }
            let traffic = std::mem::take(&mut self.busy)
                || video_in_flight
                || self.monitor_in_flight
                || !self.files.is_empty()
                || IN_FLIGHT_CHUNKS.load(Ordering::Relaxed) > 0;
            if traffic {
                last_traffic = Instant::now();
            }
            // Someone using the open window gets full speed too (typing an ID, browsing the history).
            if last_window_check.elapsed() >= WINDOW_CHECK {
                last_window_check = Instant::now();
                window_shown = window::is_window_shown(&ctx.window_title);
            }
            let session = !partner.is_empty() || ctx.is_hosting() || !ctx.viewers.is_empty() || window_shown;
            // Transfers and monitor streams run without a session: recent traffic keeps full speed too.
            let quiet = last_traffic.elapsed();
            if (session || quiet < IDLE_AFTER) != awake.is_some() {
                awake = awake.is_none().then(power::awake);
            }
            if backoff {
                self.pause(idle_sleep(quiet, session));
            } else {
                thread::sleep(BUSY_SLEEP);
            }
        }
        self.on_connection_ended();
    }

    fn register_message(&self) -> ClientMessage {
        let edition = self.ctx.edition;
        let mut caps = vec![capability::FILE_ACK];
        // Clipboard file offers need Explorer's virtual files: Windows only, and not from the service's SYSTEM
        // agent, whose clipboard objects the user's Explorer cannot pull from reliably. Partners then send
        // copied files as ordinary transfers.
        if cfg!(windows) && !autostart::supervised_by_service() {
            caps.push(capability::FILE_OFFER);
        }
        caps.extend([capability::FILE_FOLDER, capability::FILE_BINARY, capability::CURSOR_SHAPE]);
        caps.extend(decoder::supported_video_caps());
        // The Opus codec is built in, so every desktop build decodes it.
        caps.push(capability::AUDIO_OPUS);
        if p2p::enabled() {
            caps.push(capability::P2P);
        }
        if p2p::relay_enabled() {
            caps.push(capability::UDP_RELAY);
        }
        if e2e::enabled() && crate::infra::identity::local().is_some() {
            caps.push(capability::E2E);
        }
        // Sent before the relay's caps are known; multi-session is used only once `registered-ack` lists it too.
        if edition.can_view() {
            caps.push(capability::MULTI_SESSION);
        }
        // Hosts list their optional controls; Windows hosts omit the marker and keep the legacy "all of them".
        if !cfg!(windows) {
            caps.push(capability::HOST_FEATURES);
        }
        // A machine that can capture its screen can also be watched on the monitor wall; only the admin edition
        // watches.
        if core_engine::capture::host_support() == core_engine::capture::HostSupport::Supported {
            caps.push(capability::MONITOR_HOST);
        } else {
            caps.push(capability::VIEWER_ONLY);
        }
        #[cfg(feature = "admin")]
        caps.push(capability::MONITOR_VIEW);
        let stored = storage::load_settings();
        let mut caps: Vec<String> = caps.into_iter().map(String::from).collect();
        // Last, so a relay that keeps only its first 16 names drops these (the host then streams 60 on Medium).
        // The preset goes after the cap older hosts need, and Medium, like 60 FPS, is what listing none means.
        // Hosts that know presets also get both from `set-performance` at session start.
        if edition.can_view() {
            let preset = stored.performance();
            let fps = crate::service::settings::viewer_fps_cap(preset);
            if fps != frame_rate::DEFAULT_FPS {
                caps.push(capability::max_fps_cap(fps));
            }
            if preset != Performance::Medium {
                caps.push(preset.cap().to_string());
            }
        }
        let gateway = self.ctx.relay.gateway_url.lock().clone();
        let device_token =
            std::env::var("OPENDESK_DEVICE_TOKEN").unwrap_or_else(|_| settings::token_for(&stored, &gateway));
        ClientMessage::Register {
            desk_id: self.ctx.local_id.clone(),
            platform: edition.platform_tag().into(),
            device_name: device::display_name(),
            device_token,
            is_admin: edition.is_admin(),
            caps,
        }
    }

    /// A P2P path lives exactly as long as the session it was opened for.
    fn prune_links(&mut self, partner: &str) {
        let ctx = &self.ctx;
        let mut host_link_closed = false;
        self.links.retain(|p| {
            let keep =
                if p.is_host() { p.partner == partner && ctx.is_hosting() } else { ctx.viewers.is_live(&p.partner) };
            if !keep {
                p.close();
                host_link_closed |= p.is_host();
            }
            keep
        });
        if host_link_closed {
            while self.p2p_input.try_recv().is_ok() {}
        }
        self.e2e.retain(|e| {
            if e.is_host() { e.partner == partner && ctx.is_hosting() } else { ctx.viewers.is_live(&e.partner) }
        });
    }

    /// Sleeps `pause`; a longer (idle) pause ends early when a message is queued, which is kept for
    /// `pump_outbox` so the queue order holds.
    fn pause(&mut self, pause: Duration) {
        // Queued signals must not wait out an idle pause.
        let pause = if self.signals.pending() { pause.min(BUSY_SLEEP) } else { pause };
        if pause <= BUSY_SLEEP {
            thread::sleep(pause);
            return;
        }
        match self.outbox.recv_timeout(pause) {
            Ok(item) => self.stashed = Some(item),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => thread::sleep(pause),
        }
    }

    fn direct(&self, id: &str) -> Option<&Arc<P2p>> {
        self.links.iter().find(|p| p.partner == id && p.is_active())
    }

    /// Sends everything queued: control messages and voice at once, mouse moves coalesced per host, only the
    /// newest video frame. File messages are parked for `pump_files`. Returns true if video went to the socket.
    fn pump_outbox(&mut self, socket: &mut Socket, partner: &str) -> bool {
        let mut video: Option<Vec<u8>> = None;
        let mut mouse: Vec<(String, f64, f64)> = Vec::new();
        while let Some(item) = self.stashed.take().or_else(|| self.outbox.try_recv().ok()) {
            self.busy = true;
            match item {
                Outgoing::Message(msg) => {
                    let text = match &msg {
                        ClientMessage::RelayInput { target_clean, event } => {
                            if self.direct(target_clean).is_some_and(|p| p.send_input(event)) {
                                continue;
                            }
                            self.input_text(target_clean, event)
                        }
                        ClientMessage::P2pSignal { target_clean, .. } => {
                            self.signals.push(target_clean.clone(), msg.to_json());
                            continue;
                        }
                        _ => msg.to_json(),
                    };
                    let _ = socket.send(Message::Text(text.into()));
                }
                Outgoing::MouseMove(target, x, y) => match mouse.iter_mut().find(|m| m.0 == target) {
                    Some(m) => (m.1, m.2) = (x, y),
                    None => mouse.push((target, x, y)),
                },
                Outgoing::VideoFrame(frame) => video = Some(frame),
                Outgoing::MonitorFrame(frame) => self.monitor_pending = Some(frame),
                Outgoing::Audio(packet) => self.send_audio(socket, partner, packet),
                Outgoing::File { message, status } => {
                    let message = Message::Text(message.to_json().into());
                    self.files.push_back(QueuedFile { message, units: 1, status });
                }
                Outgoing::FileChunk { target, frame, status } => {
                    let units = chunk_units(frame.len());
                    // Sealed here, in queue order, so the receiver sees its nonces ascending.
                    let frame = self.seal(&target, frame);
                    // Like voice: a viewer of several hosts names the host; the relay's grant decides anyway.
                    let frame = if self.ctx.viewers.multi() && self.ctx.viewers.get(&target).is_some() {
                        binary::tag(&target, &frame).unwrap_or(frame)
                    } else {
                        frame
                    };
                    self.files.push_back(QueuedFile { message: Message::Binary(frame.into()), units, status });
                }
            }
        }
        for text in self.signals.due(Instant::now()) {
            let _ = socket.send(Message::Text(text.into()));
        }
        for (target, norm_x, norm_y) in mouse.into_iter().filter(|m| !m.0.is_empty()) {
            let event = InputEvent::MouseMove { norm_x, norm_y };
            if !self.direct(&target).is_some_and(|p| p.send_input(&event)) {
                let _ = socket.send(Message::Text(self.input_text(&target, &event).into()));
            }
        }
        let Some(frame) = video else { return false };
        // Over P2P the viewer's frame acknowledgement opens the gate instead of the socket flush.
        if self.direct(partner).is_some_and(|p| p.send_video(&frame)) {
            return false;
        }
        let _ = socket.send(Message::Binary(self.seal(partner, frame).into()));
        true
    }

    /// Seals a binary message for `partner` on the relay path when the session is end-to-end encrypted.
    fn seal(&self, partner: &str, msg: Vec<u8>) -> Vec<u8> {
        self.e2e_for(partner).and_then(|e| e.seal(&msg)).unwrap_or(msg)
    }

    /// Opens a binary message from `partner`; None drops it (see `E2e::open`).
    fn open<'a>(&self, partner: &str, msg: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        match self.e2e_for(partner) {
            Some(e) => e.open(msg),
            None => Some(Cow::Borrowed(msg)),
        }
    }

    /// `relay-input` text for `target`; the event is sealed when the session is end-to-end encrypted. The
    /// relay only needs `targetClean` to authorize it.
    fn input_text(&self, target: &str, event: &InputEvent) -> String {
        let sealed = self.e2e_for(target).and_then(|e| serde_json::to_value(event).ok().and_then(|v| e.seal_event(&v)));
        match sealed {
            Some(event) => {
                serde_json::json!({ "type": "relay-input", "targetClean": target, "event": event }).to_string()
            }
            None => ClientMessage::input(target, event.clone()).to_json(),
        }
    }

    /// Sends the newest monitor-wall thumbnail once the previous one has left the socket, so a slow link holds
    /// at most one thumbnail here and older ones are dropped, never queued.
    fn pump_monitor(&mut self, socket: &mut Socket) {
        if self.monitor_in_flight {
            return;
        }
        if let Some(frame) = self.monitor_pending.take() {
            let _ = socket.send(Message::Binary(frame.into()));
            self.monitor_in_flight = true;
            self.busy = true;
        }
    }

    /// Voice goes to the partner; a viewer of several hosts talks to the focused one only (tagged).
    fn send_audio(&self, socket: &mut Socket, partner: &str, packet: Vec<u8>) {
        if partner.is_empty() || self.direct(partner).is_some_and(|p| p.send_audio(&packet)) {
            return;
        }
        let packet = self.seal(partner, packet);
        let out = if self.ctx.viewers.multi() && self.ctx.viewers.get(partner).is_some() {
            binary::tag(partner, &packet)
        } else {
            Some(packet)
        };
        if let Some(out) = out {
            let _ = socket.send(Message::Binary(out.into()));
        }
    }

    /// Sends a burst of queued file messages and returns the newest status line among them.
    fn pump_files(&mut self, socket: &mut Socket, idle: bool) -> Option<String> {
        let mut budget = if idle { FILE_BURST_IDLE } else { FILE_BURST_IN_SESSION };
        let mut latest = None;
        while budget > 0 {
            let Some(file) = self.files.pop_front() else { break };
            // WouldBlock means tungstenite buffered the frame; any other error leaves it queued until the read
            // loop notices the dead socket and fails the transfer. Messages are reference counted: the clone
            // copies no data.
            match socket.send(file.message.clone()) {
                Ok(()) => {}
                Err(tungstenite::Error::Io(ref e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => {
                    self.files.push_front(file);
                    break;
                }
            }
            IN_FLIGHT_CHUNKS.fetch_sub(file.units, Ordering::Relaxed);
            budget = budget.saturating_sub(file.units);
            latest = file.status.or(latest);
        }
        latest
    }

    /// Drains P2P input and every readable socket message. Returns false when the connection is gone.
    fn read_all(&mut self, socket: &mut Socket) -> bool {
        while let Ok(event) = self.p2p_input.try_recv() {
            self.busy = true;
            crate::service::host::input::apply(&self.ctx, event);
        }
        loop {
            let read = socket.read();
            // Keep-alive pings are not traffic: an idle connection stays idle.
            if matches!(read, Ok(Message::Binary(_) | Message::Text(_))) {
                self.busy = true;
            }
            match read {
                Ok(Message::Ping(payload)) => {
                    let _ = socket.send(Message::Pong(payload));
                    let _ = socket.flush();
                }
                Ok(Message::Binary(bin)) => self.on_binary(&bin),
                Ok(Message::Text(text)) => {
                    if let Some(msg) = ServerMessage::parse(&text) {
                        self.on_message(msg);
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(ref e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    return true;
                }
                Err(e) => {
                    logger::warn("relay", &format!("Socket disconnected: {e}"));
                    return false;
                }
            }
        }
    }

    /// Voice and video from the relay: tagged per host for a multi-session viewer, else the single session.
    /// File chunks are always tagged with their sender.
    fn on_binary(&mut self, bin: &[u8]) {
        if let Some((from, payload)) = binary::untag(bin)
            && binary::parse_file_chunk(payload).is_some()
        {
            // A sealed chunk keeps its routing header, so it is recognized before it is opened.
            if let Some(payload) = self.open(from, payload)
                && let Some(chunk) = binary::parse_file_chunk(&payload)
            {
                self.on_file_chunk(from, &chunk);
            }
            return;
        }
        let ctx = &self.ctx;
        if binary::parse_file_chunk(bin).is_some() {
            return; // an untagged chunk has no stamped sender
        }
        if binary::is_monitor(bin) {
            // Monitor-wall thumbnails never belong to a viewer session (and reach only the admin edition).
            #[cfg(feature = "admin")]
            if let Some((host, jpeg)) = binary::untag_monitor(bin) {
                crate::service::monitor_wall::on_frame(ctx, host, jpeg);
            }
            return;
        }
        match binary::untag(bin) {
            // Frames for a closed tab are dropped.
            Some((host, payload)) => {
                if let Some(s) = ctx.viewers.get(host)
                    && let Some(payload) = self.open(host, payload)
                {
                    if binary::is_audio(&payload) {
                        if s.audio_on.load(Ordering::Relaxed) {
                            ctx.voice.play_binary_packet(&payload);
                        }
                    } else {
                        s.inbox.push(payload.into_owned());
                    }
                }
            }
            None if binary::is_audio(bin) => {
                // Untagged voice comes from the hosted viewer, or from the single viewed host.
                let from = if ctx.is_hosting() { ctx.partner() } else { single_id(ctx) };
                if let Some(packet) = self.open(&from, bin) {
                    ctx.voice.play_binary_packet(&packet);
                }
            }
            None => {
                if let Some(s) = ctx.viewers.single()
                    && let Some(frame) = self.open(&s.id, bin)
                {
                    s.inbox.push(frame.into_owned());
                }
            }
        }
    }

    /// A binary file chunk; `from` is the relay's tag, like `fromClean` on `file-transfer`. A rejected chunk is
    /// answered with a NACK so the sender stops at once.
    fn on_file_chunk(&mut self, from: &str, chunk: &binary::FileChunk) {
        let ctx = Arc::clone(&self.ctx);
        let (status, ack) = self.receiver.handle_chunk(&ctx, chunk, from);
        if let Some(ok) = ack {
            ctx.send(ClientMessage::file(from, FilePayload::ack(chunk.id, ok)));
        }
        if let Some(status) = status {
            ctx.ui.status(status);
        }
    }

    /// The socket failed: relay grants died with it, so every transfer and session on it has ended.
    fn on_connection_lost(&mut self) {
        let ctx = &self.ctx;
        file_transfer::abort_pending_sends();
        virtual_clipboard::abort_pulls();
        virtual_clipboard::clear_offer("");
        for file in self.files.drain(..) {
            IN_FLIGHT_CHUNKS.fetch_sub(file.units, Ordering::Relaxed);
        }
        let reason = self.receiver.abort_all(ctx.ui.as_ref());
        crate::service::sessions::end_all(ctx);
        ctx.ui.relay_status(RelayStatus::Offline { reason });
    }

    /// The connection ended for any reason: the relay owns session authorization, so direct paths and viewer
    /// sessions end with it (a reconnect starts without them).
    fn on_connection_ended(&mut self) {
        for p in self.links.drain(..) {
            p.close();
        }
        self.e2e.clear();
        self.signals.clear();
        let ctx = &self.ctx;
        // Watches live in the relay: a new connection starts without them until the relay asks again.
        crate::service::host::monitor_stream::stop(ctx);
        (self.monitor_pending, self.monitor_in_flight) = (None, false);
        #[cfg(feature = "admin")]
        crate::service::monitor_wall::relay_lost(ctx);
        if !ctx.viewers.is_empty() {
            crate::service::viewer::controller::end_all(ctx);
            if !ctx.is_hosting() {
                ctx.set_partner("");
            }
            ctx.voice.is_session_active.store(ctx.is_hosting(), Ordering::SeqCst);
        }
    }
}

/// The single viewed host's ID, or "" without one.
fn single_id(ctx: &Ctx) -> String {
    ctx.viewers.single().map(|s| s.id.clone()).unwrap_or_default()
}

/// Idle backoff of the connection loop: on unless `OPENDESK_IDLE_BACKOFF=0` (a fixed 1 ms tick).
fn idle_backoff_enabled() -> bool {
    std::env::var("OPENDESK_IDLE_BACKOFF").as_deref() != Ok("0")
}

/// Pause between ticks: 1 ms during a session and for `IDLE_AFTER` after the last traffic, then growing step
/// by step to `IDLE_MAX_SLEEP`.
fn idle_sleep(idle_for: Duration, session_active: bool) -> Duration {
    if session_active || idle_for < IDLE_AFTER {
        return BUSY_SLEEP;
    }
    let ms = match (idle_for - IDLE_AFTER).as_secs() {
        0 => 2,
        1 => 5,
        2 => 10,
        _ => return IDLE_MAX_SLEEP,
    };
    Duration::from_millis(ms)
}

/// Uses the loopback relay when one runs on this machine and the configured URL is a remote `wss://` one
/// (the relay machine itself would otherwise hairpin through its own tunnel).
fn resolve_url(configured: &str) -> String {
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
fn set_low_latency(socket: &mut Socket) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_sleep_stays_at_one_ms_during_sessions_and_recent_traffic() {
        assert_eq!(idle_sleep(Duration::ZERO, false), BUSY_SLEEP);
        assert_eq!(idle_sleep(Duration::from_millis(1999), false), BUSY_SLEEP);
        assert_eq!(idle_sleep(Duration::from_secs(3600), true), BUSY_SLEEP, "a session never backs off");
    }

    #[test]
    fn idle_sleep_grows_gradually_to_the_cap() {
        let steps: Vec<u128> =
            [2, 3, 4, 5, 60].iter().map(|s| idle_sleep(Duration::from_secs(*s), false).as_millis()).collect();
        assert_eq!(steps, vec![2, 5, 10, 20, 20]);
        assert!(idle_sleep(Duration::MAX, false) <= IDLE_MAX_SLEEP);
    }
}
