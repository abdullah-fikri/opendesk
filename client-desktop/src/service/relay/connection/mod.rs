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

use crate::domain::bulk;
use crate::domain::file_route::{FileRoutes, Path, Step};
use crate::domain::performance::Performance;
use crate::domain::settings::{self, normalize_gateway_url};
use crate::domain::{capability, file_squeeze, frame_rate};
use crate::infra::e2e::{self, E2e};
use crate::infra::file_channel::{self, FileChannel};
use crate::infra::p2p::{self, P2p};
use crate::infra::{device, logger, power, storage, virtual_clipboard, window};
use crate::protocol::{ClientMessage, FileAction, FileMark, FilePayload, InputEvent, Outgoing, ServerMessage, binary};
use crate::service::context::Ctx;
use crate::service::file_transfer::{self, FileReceiver, IN_FLIGHT_CHUNKS, chunk_units};
use crate::service::ui_port::{RelayStatus, SessionChange};
use crate::service::viewer::decoder;

use super::file_pacer::FilePacer;
use super::resume::{self, Resume};
use super::signal_pacer::SignalPacer;

mod channels;
mod dial;
mod file_lane;
mod inbound;
mod lane;
mod outbound;
mod p2p_files;
mod sessions;
mod socket;
#[cfg(test)]
mod tests;
mod watchdog;

use socket::*;

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
/// Most P2P transfers remembered as waiting for our verdict (a paste of many small files ends many at once).
const MAX_P2P_ACKS: usize = 256;
/// Local relay probed when the configured relay is remote (avoids tunnel hairpinning on the relay machine).
const LOCAL_RELAY: &str = "127.0.0.1:8088";
/// Pause between ticks while a session runs or traffic flowed recently.
const BUSY_SLEEP: Duration = Duration::from_millis(1);
/// Quiet time before the pause starts to grow...
const IDLE_AFTER: Duration = Duration::from_secs(2);
/// ...up to this (far below the relay's 15 s ping interval).
const IDLE_MAX_SLEEP: Duration = Duration::from_millis(20);
/// No frame at all from the relay (its keep-alive pings included) for this long means the socket is dead
/// even though the OS has not said so (a NAT or modem dropped the TCP state, the PC slept): reconnect. The
/// relay pings every 15 s and drops a peer after 60 s of silence; giving up after 3 missed pings registers
/// the new connection while the relay still holds the old one, so it replaces it and the device never shows
/// offline. Waiting on the OS instead could leave the device offline for good.
const RELAY_SILENCE_LIMIT: Duration = Duration::from_secs(45);
/// Connected but no `registered-ack` for this long: the relay never took the registration (it was lost, or
/// the relay keeps the socket after an error). Start over on a fresh socket instead of waiting forever.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the loop checks whether the main window is on screen.
const WINDOW_CHECK: Duration = Duration::from_millis(500);
/// Kernel send buffer (`SO_SNDBUF`) while the hosted session's video goes over this socket. The video gate
/// opens once a frame is in the kernel buffer, and an auto-tuned buffer grows to megabytes: on a link slower
/// than the stream, seconds of video could wait there before `flush` blocks and the congestion meter notices.
/// The buffer also holds the bytes in flight, so it caps throughput at buffer / RTT: 512 KiB still carries
/// ~17 Mbps at 234 ms RTT (a far relay), above the High preset at 144 FPS on 1080p (~12 Mbps), while a slow
/// link queues at most ~0.3 s of such a stream here instead of seconds.
const HOSTING_SEND_BUFFER: usize = 512 * 1024;
/// Smallest `OPENDESK_WS_SNDBUF` override honored: below this one key frame needs many round trips.
const MIN_SEND_BUFFER: usize = 64 * 1024;
/// Send buffer once such a session is over: file transfers get their throughput back (4 MiB carries
/// ~140 Mbps at 234 ms RTT). A buffer set once is no longer auto-tuned, so the OS default cannot come back.
const TRANSFER_SEND_BUFFER: usize = 4 * 1024 * 1024;

/// Starts the connection thread. `outbox` is the receiving end of `ctx.outbox`.
pub fn spawn(ctx: Ctx, outbox: Receiver<Outgoing>) {
    crate::infra::thread::spawn_named("relay-conn", move || {
        let (input_tx, input_rx) = mpsc::channel();
        let (bulk_tx, bulk_rx) = mpsc::channel();
        let (channel_tx, channel_rx) = mpsc::channel();
        let (lane_tx, lane_rx) = mpsc::channel();
        let mut conn = Connection {
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
            bulk_in_tx: bulk_tx,
            bulk_in: bulk_rx,
            channel_in_tx: channel_tx,
            channel_in: channel_rx,
            channels: Vec::new(),
            channel_pending: Vec::new(),
            routed_now: Path::Relay,
            file_routes: FileRoutes::default(),
            p2p_incoming: Vec::new(),
            p2p_acks: VecDeque::new(),
            links: Vec::new(),
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
        };
        // This process idles under EcoQoS until a session or traffic needs full speed.
        power::arm();
        // A loop stuck in some blocking call is caught by the watchdog (see `watchdog.rs`).
        watchdog::spawn();
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
    /// Its size on the wire, charged to the file rate.
    bytes: usize,
    /// Progress of its transfer, reported once the relay has read it.
    mark: Option<FileMark>,
    status: Option<String>,
}

/// Mouse moves drained in one tick, coalesced to the newest position per host.
#[derive(Default)]
struct PendingMoves(Vec<(String, f64, f64)>);

impl PendingMoves {
    /// Replaces the pending position for `target`.
    fn set(&mut self, target: String, x: f64, y: f64) {
        match self.0.iter_mut().find(|m| m.0 == target) {
            Some(m) => (m.1, m.2) = (x, y),
            None => self.0.push((target, x, y)),
        }
    }

    /// Removes and returns the pending position for `target`.
    fn take(&mut self, target: &str) -> Option<(f64, f64)> {
        let i = self.0.iter().position(|m| m.0 == target)?;
        let (_, x, y) = self.0.swap_remove(i);
        Some((x, y))
    }
}

/// Whole seconds left until `deadline` (at least 1, so the overlay stays until the host is back or closed).
fn partner_seconds_left(deadline: Instant, now: Instant) -> i32 {
    deadline.saturating_duration_since(now).as_secs_f64().ceil().max(1.0) as i32
}

/// State of the relay connection thread.
pub(super) struct Connection {
    pub(super) ctx: Ctx,
    outbox: Receiver<Outgoing>,
    /// File messages waiting their turn behind video and voice.
    files: VecDeque<QueuedFile>,
    /// Paces `files` by the relay round trip (see `file_pacer.rs`).
    pacer: FilePacer,
    pub(super) receiver: FileReceiver,
    /// Input that arrived over a P2P path (host side).
    pub(super) p2p_input_tx: Sender<InputEvent>,
    p2p_input: Receiver<InputEvent>,
    /// File stream messages that arrived over a P2P path (`file-p2p`): sender, kind, payload.
    pub(super) bulk_in_tx: Sender<(String, u8, Vec<u8>)>,
    bulk_in: Receiver<(String, u8, Vec<u8>)>,
    /// File stream messages from the file channels, as `bulk_in` for the P2P paths.
    channel_in_tx: Sender<(String, u8, Vec<u8>)>,
    channel_in: Receiver<(String, u8, Vec<u8>)>,
    /// One direct file channel per session whose partner lists `file-channel` (see `channels.rs`).
    pub(super) channels: Vec<Arc<FileChannel>>,
    /// The partner's channel candidates that arrived before our side of the channel started.
    channel_pending: Vec<(String, Vec<SocketAddr>)>,
    /// The direct path the file message being sent was routed to (see `file_route_ready`).
    routed_now: Path,
    /// Our relay file lane while a session partner can use it (see `lane.rs`), and its generation: events of an
    /// earlier lane are stale.
    lane: Option<Arc<file_lane::FileLane>>,
    lane_gen: u64,
    /// Messages and state changes from the lane, tagged with its generation.
    lane_in_tx: Sender<(u64, file_lane::LaneEvent)>,
    lane_in: Receiver<(u64, file_lane::LaneEvent)>,
    /// Incoming transfers (`sender:id`) that arrive over the lane, cut if it drops.
    lane_incoming: Vec<String>,
    /// Since when no session needed the lane.
    lane_idle: Option<Instant>,
    /// Path of each outgoing transfer (relay or P2P, see `domain::file_route`).
    file_routes: FileRoutes,
    /// Incoming transfers (`sender:id`) that arrived over a P2P path, cancelled if that path drops.
    p2p_incoming: Vec<String>,
    /// P2P transfers (`sender:id`) whose `end` arrived and whose verdict is still to be sent: it goes back over
    /// the P2P path, because the relay holds no grant for them and would drop it (see [`Self::ack_over_p2p`]).
    p2p_acks: VecDeque<String>,
    /// One direct path per session: the hosted viewer, or each viewed host.
    pub(super) links: Vec<Arc<P2p>>,
    /// One end-to-end channel per session whose partner lists `e2e`; same lifetime as `links`.
    pub(super) e2e: Vec<Arc<E2e>>,
    /// Newest monitor-wall thumbnail not sent yet, and whether the previous one is still in the socket.
    monitor_pending: Option<Vec<u8>>,
    monitor_in_flight: bool,
    /// Messages taken off the queue but not sent yet (one that ended an idle wait, or control messages held
    /// while the socket was down); `pump_outbox` sends them before the rest of the queue.
    held: VecDeque<Outgoing>,
    /// Something was sent or received this tick.
    busy: bool,
    /// When the relay last sent anything (keep-alive pings included); see `RELAY_SILENCE_LIMIT`.
    last_rx: Instant,
    /// `p2p-signal` messages spaced out per partner (the relay drops ones that follow too closely).
    signals: SignalPacer,
    /// Sessions waiting for the connection to come back (see `resume.rs`).
    pub(super) resume: Resume,
    /// Viewed hosts whose own connection dropped (`session-suspended`): host, end of its grace, seconds shown.
    pub(super) partner_waits: Vec<(String, Instant, i32)>,
    /// The relay URL of the current connection.
    pub(super) url: String,
    /// Set from a reconnect with waiting sessions until `registered-ack`: input, voice and video that have no
    /// live direct path are dropped, never replayed late.
    pub(super) quiet: bool,
}

impl Connection {
    /// One connection lifetime: connect, register, pump until it drops or a reconnect is requested.
    fn run_once(&mut self) {
        watchdog::beat();
        let ctx = Arc::clone(&self.ctx);
        ctx.relay.reconnect.store(false, Ordering::SeqCst);
        ctx.relay.online.store(false, Ordering::SeqCst);
        for id in self.resume.expire(Instant::now()) {
            self.end_waiting(&id, resume::EXPIRED);
        }
        let url = resolve_url(&ctx.relay.gateway_url.lock());
        logger::info("relay", &format!("Connecting to relay {url}"));
        ctx.ui.relay_status(RelayStatus::Connecting);

        let Some(mut socket) = self.connect(&url) else {
            ctx.ui.relay_status(RelayStatus::Offline { reason: self.reconnecting_status() });
            let delay = self.resume.next_delay(RETRY_DELAY);
            self.sleep_offline(delay);
            return;
        };
        self.last_rx = Instant::now();
        let connected_at = Instant::now();
        let resume_token = self.resume.token_for(&url);
        self.quiet = resume_token.is_some();
        self.url = url;
        let _ = socket.send(Message::Text(self.register_message(resume_token).to_json().into()));
        let _ = socket.flush();
        set_low_latency(&mut socket);
        logger::info("relay", "Connection established.");
        ctx.ui.relay_status(RelayStatus::Registering);

        ctx.host.can_send_frame.open();
        ctx.viewers.set_multi(false);
        let mut lost = false;
        let mut video_in_flight = false;
        // When the frame now in flight on this socket was taken from the outbox (the `host-stats` send time).
        let mut video_taken: Option<Instant> = None;
        let mut last_expire = Instant::now();
        let mut last_path_update = Instant::now();
        let backoff = idle_backoff_enabled();
        let mut last_traffic = Instant::now();
        let mut awake: Option<power::Awake> = None;
        let (mut window_shown, mut last_window_check) = (false, Instant::now() - WINDOW_CHECK);
        let mut send_buffer: Option<usize> = None;
        let sndbuf_env = std::env::var("OPENDESK_WS_SNDBUF").ok();

        while !ctx.relay.reconnect.load(Ordering::SeqCst) && !lost {
            watchdog::beat();
            self.show_countdown();
            self.show_partner_countdown();
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
                self.maintain_channels();
                for p in &self.links {
                    file_transfer::set_fast_path(&p.partner, self.file_path_to(&p.partner) != Path::Relay);
                }
                for c in &self.channels {
                    file_transfer::set_fast_path(&c.partner, self.file_path_to(&c.partner) != Path::Relay);
                }
                self.maintain_lane();
                let hosted_p2p = self.links.iter().any(|p| p.is_host() && p.is_active());
                ctx.host.p2p_active.store(hosted_p2p, Ordering::Relaxed);
                let wanted = wanted_send_buffer(ctx.is_hosting() && !hosted_p2p, send_buffer, sndbuf_env.as_deref());
                if wanted != send_buffer
                    && let Some(bytes) = wanted
                {
                    set_send_buffer(&mut socket, bytes);
                    send_buffer = wanted;
                }
                for e in self.e2e.iter().filter(|e| !e.is_host()) {
                    if let Some(s) = ctx.viewers.get(&e.partner) {
                        s.e2e_active.store(e.is_on(), Ordering::Relaxed);
                    }
                }
            }

            let pumped = Instant::now();
            if self.pump_outbox(&mut socket, &partner) {
                video_in_flight = true;
                video_taken.get_or_insert(pumped);
            }
            if !video_in_flight {
                // Thumbnails only fill gaps: they never queue in front of the hosted session's video.
                self.pump_monitor(&mut socket);
            }
            let mut status = self.pump_files(&mut socket, self.no_video(&partner));
            self.pacer.poll(&mut socket, !self.files.is_empty(), Instant::now());
            if last_expire.elapsed() >= Duration::from_secs(1) {
                last_expire = Instant::now();
                status = self.receiver.expire_stale(ctx.ui.as_ref(), last_expire).or(status);
                self.receiver.ask_resumes(&ctx, last_expire);
            }
            if let Some(status) = status {
                ctx.ui.status(status);
            }

            // Release the video gate only once the previous frame has fully left the socket.
            match socket.flush() {
                Ok(()) => {
                    if std::mem::take(&mut video_in_flight) {
                        ctx.host.can_send_frame.open();
                        if let Some(taken) = video_taken.take() {
                            let mut link = ctx.host.video_link.stats.lock();
                            link.send.record(taken.elapsed());
                            link.window = None;
                        }
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
                    video_taken = None;
                    self.monitor_in_flight = false;
                }
            }

            lost = !self.read_all(&mut socket);
            if !lost && relay_silent(self.last_rx.elapsed()) {
                logger::warn(
                    "relay",
                    &format!(
                        "No data from the relay for {} s; the connection is dead. Reconnecting.",
                        self.last_rx.elapsed().as_secs()
                    ),
                );
                lost = true;
            }
            if !lost && registration_overdue(ctx.relay.online.load(Ordering::SeqCst), connected_at.elapsed()) {
                logger::warn(
                    "relay",
                    &format!("No registered-ack within {} s; reconnecting.", REGISTER_TIMEOUT.as_secs()),
                );
                lost = true;
            }
            if lost {
                self.on_connection_lost();
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
        if lost {
            let delay = self.resume.next_delay(RECONNECT_DELAY);
            self.sleep_offline(delay);
        }
    }

    /// Opens the relay socket. The attempt runs on its own thread and is abandoned after `CONNECT_DEADLINE`
    /// (see `dial.rs`); while sessions wait for the connection, this thread keeps their direct paths going
    /// meanwhile (see `pump_offline`).
    fn connect(&mut self, url: &str) -> Option<Socket> {
        let attempt = Arc::new(dial::Attempt::default());
        let (tx, rx) = mpsc::channel();
        let (target, a) = (url.to_string(), Arc::clone(&attempt));
        crate::infra::thread::spawn_named("relay-connect", move || {
            let _ = tx.send(dial::dial(&target, &a, dial::HANDSHAKE_TIMEOUT));
        });
        let deadline = Instant::now() + dial::CONNECT_DEADLINE;
        loop {
            watchdog::beat();
            let wait = if self.resume.any_waiting() { BUSY_SLEEP } else { Duration::from_millis(100) };
            match rx.recv_timeout(wait) {
                Ok(Ok(socket)) => return Some(socket),
                Ok(Err(e)) => {
                    logger::warn("relay", &format!("Connection failed: {e}"));
                    return None;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    attempt.abandon();
                    logger::warn(
                        "relay",
                        &format!(
                            "Connection failed: no answer within {} s (network down or the handshake hung); retrying.",
                            dial::CONNECT_DEADLINE.as_secs()
                        ),
                    );
                    return None;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.resume.any_waiting() {
                        self.pump_offline();
                    }
                }
            }
        }
    }

    /// Sleeps `delay` before the next connect, pumping the waiting sessions' direct paths meanwhile.
    fn sleep_offline(&mut self, delay: Duration) {
        let end = Instant::now() + delay;
        watchdog::beat();
        while self.resume.any_waiting() && Instant::now() < end {
            watchdog::beat();
            self.pump_offline();
            thread::sleep(BUSY_SLEEP);
        }
        thread::sleep(end.saturating_duration_since(Instant::now()));
    }

    /// One tick without a relay socket while sessions wait: input that arrived over P2P is applied, video,
    /// voice and input go over a live direct path or are dropped (never replayed late), and everything else is
    /// held for the next connection. Sessions whose grace ran out end.
    fn pump_offline(&mut self) {
        for id in self.resume.expire(Instant::now()) {
            self.end_waiting(&id, resume::EXPIRED);
        }
        self.show_countdown();
        let partner = self.ctx.partner();
        self.prune_links(&partner);
        while let Ok(event) = self.p2p_input.try_recv() {
            self.on_p2p_event(event);
        }
        while let Ok(item) = self.outbox.try_recv() {
            if !is_live(&item) {
                self.held.push_back(item);
                continue;
            }
            match item {
                Outgoing::Message(ClientMessage::RelayInput { target_clean, event }) => {
                    if let Some(p) = self.direct(&target_clean) {
                        p.send_input(&event);
                    }
                }
                Outgoing::MouseMove(target, norm_x, norm_y) => {
                    if let Some(p) = self.direct(&target) {
                        p.send_input(&InputEvent::MouseMove { norm_x, norm_y });
                    }
                }
                Outgoing::VideoFrame(frame) => {
                    if let Some(p) = self.direct(&partner) {
                        p.send_video(&frame);
                    }
                }
                Outgoing::Audio(packet) if !partner.is_empty() => {
                    if let Some(p) = self.direct(&partner) {
                        p.send_audio(&packet);
                    }
                }
                _ => {}
            }
        }
    }

    fn register_message(&self, resume_token: Option<String>) -> ClientMessage {
        let edition = self.ctx.edition;
        let mut caps = vec![capability::FILE_ACK];
        // The service agent's clipboard helper starts with the app; give it a moment so the first registration
        // already lists file-offer (caps cannot change until the next registration).
        #[cfg(windows)]
        if crate::infra::autostart::supervised_by_service() {
            crate::infra::clipboard_helper::wait_ready(std::time::Duration::from_secs(5));
        }
        // Without clipboard file offers, partners send copied files as ordinary transfers.
        match file_transfer::local_file_offer() {
            Ok(()) => caps.push(capability::FILE_OFFER),
            Err(reason) => logger::info("file_transfer", &format!("Not advertising file-offer: {reason}.")),
        }
        caps.extend([capability::FILE_FOLDER, capability::FILE_BINARY, capability::CURSOR_SHAPE]);
        caps.extend(decoder::supported_video_caps());
        // As a host: early H.264 for viewers that ask (phones), so they need not request the High preset.
        caps.push(capability::H264_EAGER);
        // The Opus codec is built in, so every desktop build decodes it.
        caps.push(capability::AUDIO_OPUS);
        caps.push(capability::REMOTE_MIC);
        if p2p::enabled() {
            caps.push(capability::P2P);
            if crate::infra::p2p::nack_enabled() {
                caps.push(capability::VIDEO_NACK);
            }
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
        if resume::enabled() {
            caps.push(capability::SESSION_RESUME);
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
        // Late in the list: a relay that keeps only 16 names drops it, which only sends loose files one by one.
        caps.push(capability::FILE_GROUP);
        caps.push(capability::FILE_PULL_BIN);
        caps.push(capability::TILE_LOSSLESS);
        #[cfg(windows)]
        caps.push(capability::MOUSE_RELATIVE);
        #[cfg(windows)]
        caps.push(capability::CLIPBOARD_IMAGE);
        if p2p::enabled() && p2p::file_p2p_enabled() {
            caps.push(capability::FILE_P2P);
        }
        caps.push(capability::FILE_DEFLATE);
        caps.push(capability::FILE_RESUME);
        #[cfg(windows)]
        caps.push(capability::FILE_DROP);
        if file_channel::enabled() && e2e::enabled() && crate::infra::identity::local().is_some() {
            caps.push(capability::FILE_CHANNEL);
        }
        let stored = storage::load_settings();
        let mut caps: Vec<String> = caps.into_iter().map(String::from).collect();
        // Right before `fps-N`: a relay that keeps only 16 names drops it first, which only turns host sound off.
        if crate::infra::audio::HOST_SOUND {
            caps.push(capability::AUDIO_SYSTEM.to_string());
        }
        // Last, so a relay that keeps only its first 16 names (up to 1.2.0) drops these (the host then streams 60 on Medium).
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
        // After everything: a relay that keeps only its first 32 names (up to 1.5.0) drops this first, and it
        // cannot run a file lane anyway.
        if file_lane::enabled() {
            caps.push(capability::FILE_LANE.to_string());
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
            resume_token,
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
        // A channel lives as long as the session's end-to-end channel.
        let live: Vec<String> = self.e2e.iter().map(|e| e.partner.clone()).collect();
        self.prune_channels(|p| live.iter().any(|l| l == p));
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
            Ok(item) => self.held.push_back(item),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => thread::sleep(pause),
        }
    }

    fn direct(&self, id: &str) -> Option<&Arc<P2p>> {
        self.links.iter().find(|p| p.partner == id && p.is_active())
    }
}

/// The single viewed host's ID, or "" without one.
fn single_id(ctx: &Ctx) -> String {
    ctx.viewers.single().map(|s| s.id.clone()).unwrap_or_default()
}

/// Input, voice and video: worthless once late, so without a live direct path they are dropped instead of held
/// for the next relay connection.
fn is_live(item: &Outgoing) -> bool {
    matches!(
        item,
        Outgoing::Message(ClientMessage::RelayInput { .. })
            | Outgoing::MouseMove(..)
            | Outgoing::VideoFrame(_)
            | Outgoing::Audio(_)
            | Outgoing::MonitorFrame(_)
    )
}

/// Whether the relay has been silent long enough to call the socket dead (see `RELAY_SILENCE_LIMIT`).
fn relay_silent(since_last_rx: Duration) -> bool {
    since_last_rx >= RELAY_SILENCE_LIMIT
}

/// Whether a connection up for `connected_for` should have been registered by now (see `REGISTER_TIMEOUT`).
fn registration_overdue(registered: bool, connected_for: Duration) -> bool {
    !registered && connected_for >= REGISTER_TIMEOUT
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
