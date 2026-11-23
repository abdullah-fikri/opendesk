//! The relay file lane (`file-lane`, see `docs/features/relay-file-lane.md`): a second WebSocket to the relay that
//! carries only file transfers, for partners that neither the file channel nor the P2P path reaches.
//!
//! On the main relay socket a file shares one TCP connection with the session's video, voice and input: the
//! connection loop sends it in small bursts behind them, paced by the relay round trip (`file_pacer.rs`), and the
//! relay queues it behind them again. AnyDesk moves files over a connection of their own; so does this lane. Its
//! thread writes as fast as the socket takes data (an 8 MiB send buffer covers a far relay), the relay forwards
//! lane traffic without video priority, and the main socket's video latency is untouched.
//!
//! Attaching: the lane asks the main connection for a one-time token (`file-lane-request`, answered with
//! `file-lane-token`), dials the same relay URL with the bounded dial of `dial.rs` and presents the token with
//! `file-lane-attach`; the relay answers `file-lane-ack`. A lost lane reports [`LaneEvent::Lost`] (what was moving
//! on it is cut and resumes, like a lost file channel) and dials again with a growing pause; a closed lane stops.
//!
//! What arrives is handed to the relay connection thread as it came ([`LaneEvent::Text`], [`LaneEvent::Binary`]):
//! the same `file-transfer` messages and tagged chunks the main socket carries. The reader stops reading while
//! more than [`RECEIVE_WINDOW`] bytes wait there, so a slow disk throttles the sender through TCP instead of
//! filling memory.
//!
//! `OPENDESK_FILE_LANE=0` turns it off (not advertised, not used).

use std::io::ErrorKind;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::Message;

use super::Socket;
use super::dial;
use super::socket::{set_low_latency, set_send_buffer};
use crate::domain::bulk::OnDelivered;
use crate::infra::logger;

/// How long the lane waits for a token from the main connection, and for the relay's answer to the attach.
const TOKEN_WAIT: Duration = Duration::from_secs(10);
const ACK_WAIT: Duration = Duration::from_secs(10);
/// No frame from the relay (its 15 s keep-alive pings included) for this long: the lane is dead.
const SILENCE: Duration = Duration::from_secs(45);
/// Received bytes not yet handled by the relay connection thread; the reader pauses above this.
const RECEIVE_WINDOW: usize = 32 << 20;
/// Kernel send buffer: ~280 Mbps at 234 ms to a far relay.
const SEND_BUFFER: usize = 8 << 20;
/// Pause before the next attach, doubling up to the maximum; a lane that stayed up this long starts over.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);
const STABLE: Duration = Duration::from_secs(30);
/// Pause of an idle pump tick.
const TICK: Duration = Duration::from_millis(1);

/// Client-side kill switch: `OPENDESK_FILE_LANE=0` neither advertises nor uses the lane.
pub fn enabled() -> bool {
    std::env::var("OPENDESK_FILE_LANE").map(|v| v != "0").unwrap_or(true)
}

/// What the lane reports to the relay connection thread.
#[derive(Debug, PartialEq, Eq)]
pub enum LaneEvent {
    /// Attached: transfers may take the lane.
    Up,
    /// A text message from the relay (`file-transfer`).
    Text(String),
    /// A binary message from the relay (a file chunk tagged with its sender).
    Binary(Vec<u8>),
    /// The lane dropped: everything on it is lost.
    Lost,
}

/// Where the lane's events go, tagged with the lane's generation (events of an earlier lane are stale).
pub type Sink = Sender<(u64, LaneEvent)>;

/// One queued message and what to call once it is in the socket (or could not be).
type Outbound = (Message, Option<OnDelivered>);

/// The file lane of this device.
pub struct FileLane {
    /// Tells this lane's events from those of an earlier one.
    pub generation: u64,
    queue: Sender<Outbound>,
    tokens: Sender<String>,
    up: AtomicBool,
    closed: AtomicBool,
    /// Received bytes not yet consumed (see [`RECEIVE_WINDOW`]).
    window: AtomicUsize,
}

impl FileLane {
    /// Starts the lane of device `desk_id` to the relay at `url`. `request_token` asks the main connection for a
    /// token, which comes back through [`Self::give_token`].
    pub fn start(
        generation: u64,
        url: &str,
        desk_id: &str,
        sink: Sink,
        request_token: Box<dyn Fn() + Send>,
    ) -> Arc<Self> {
        let (queue, queued) = mpsc::channel();
        let (tokens, token_rx) = mpsc::channel();
        let me = Arc::new(Self {
            generation,
            queue,
            tokens,
            up: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            window: AtomicUsize::new(0),
        });
        let (lane, url, desk_id) = (Arc::clone(&me), url.to_string(), desk_id.to_string());
        crate::infra::thread::spawn_named("file-lane", move || {
            run(&lane, &url, &desk_id, &queued, &token_rx, &sink, &*request_token)
        });
        me
    }

    /// True while attached.
    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::SeqCst)
    }

    /// A token the main connection received for this lane.
    pub fn give_token(&self, token: String) {
        let _ = self.tokens.send(token);
    }

    /// Queues one message; `done` runs once it is in the socket (true) or the lane is gone (false). False when the
    /// lane is not up (`done` then already ran with false). Never blocks: the senders' in-flight limit
    /// (`IN_FLIGHT_CHUNKS`) bounds what waits here, as it bounds the main socket's file queue.
    pub fn send(&self, msg: Message, done: Option<OnDelivered>) -> bool {
        if !self.is_up() {
            if let Some(done) = done {
                done(false);
            }
            return false;
        }
        match self.queue.send((msg, done)) {
            Ok(()) => true,
            Err(mpsc::SendError((_, done))) => {
                if let Some(done) = done {
                    done(false);
                }
                false
            }
        }
    }

    /// The relay connection handled `bytes` of what the lane received: the reader may read on.
    pub fn consumed(&self, bytes: usize) {
        let _ = self.window.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |w| Some(w.saturating_sub(bytes)));
    }

    /// Ends the lane for good (no session needs it, or the main connection ended).
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.up.store(false, Ordering::SeqCst);
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// The lane's thread: attach, pump until the lane drops, pause, attach again, until closed.
fn run(
    lane: &FileLane,
    url: &str,
    desk_id: &str,
    queued: &Receiver<Outbound>,
    tokens: &Receiver<String>,
    sink: &Sink,
    request_token: &dyn Fn(),
) {
    let mut delay = RETRY_FIRST;
    while !lane.is_closed() {
        fail_queued(queued);
        while tokens.try_recv().is_ok() {}
        request_token();
        let attached = match wait_token(lane, tokens) {
            Some(token) => attach(url, desk_id, &token),
            None => Err("no lane token from the relay".into()),
        };
        match attached {
            Ok(mut socket) if !lane.is_closed() => {
                // Anything queued before this lane existed belongs to transfers that were already cut.
                fail_queued(queued);
                lane.window.store(0, Ordering::SeqCst);
                lane.up.store(true, Ordering::SeqCst);
                let _ = sink.send((lane.generation, LaneEvent::Up));
                logger::info("file_lane", "File lane to the relay is up.");
                let since = Instant::now();
                let why = pump(lane, &mut socket, queued, sink);
                lane.up.store(false, Ordering::SeqCst);
                let _ = socket.close(None);
                let _ = socket.flush();
                fail_queued(queued);
                let _ = sink.send((lane.generation, LaneEvent::Lost));
                if lane.is_closed() {
                    break;
                }
                logger::warn("file_lane", &format!("File lane dropped ({why}); attaching again."));
                if since.elapsed() >= STABLE {
                    delay = RETRY_FIRST;
                }
            }
            Ok(_) => break,
            Err(e) => logger::info("file_lane", &format!("File lane not attached: {e}.")),
        }
        nap(lane, delay);
        delay = (delay * 2).min(RETRY_MAX);
    }
    lane.up.store(false, Ordering::SeqCst);
    fail_queued(queued);
}

/// Waits for a token from the main connection; None past [`TOKEN_WAIT`] or once the lane is closed.
fn wait_token(lane: &FileLane, tokens: &Receiver<String>) -> Option<String> {
    let deadline = Instant::now() + TOKEN_WAIT;
    while !lane.is_closed() && Instant::now() < deadline {
        if let Ok(token) = tokens.recv_timeout(Duration::from_millis(100)) {
            return Some(token);
        }
    }
    None
}

/// Sleeps `pause`, cut short when the lane is closed.
fn nap(lane: &FileLane, pause: Duration) {
    let end = Instant::now() + pause;
    while !lane.is_closed() && Instant::now() < end {
        thread::sleep(Duration::from_millis(50).min(end.saturating_duration_since(Instant::now())));
    }
}

/// Fails every queued message (its transfer is cut with the lane).
fn fail_queued(queued: &Receiver<Outbound>) {
    while let Ok((_, done)) = queued.try_recv() {
        if let Some(done) = done {
            done(false);
        }
    }
}

/// Dials the relay (bounded like the main connection, see `dial.rs`) and attaches with `token`.
fn attach(url: &str, desk_id: &str, token: &str) -> Result<Socket, String> {
    let attempt = Arc::new(dial::Attempt::default());
    let (tx, rx) = mpsc::channel();
    let (target, a) = (url.to_string(), Arc::clone(&attempt));
    crate::infra::thread::spawn_named("file-lane-dial", move || {
        let _ = tx.send(dial::dial(&target, &a, dial::HANDSHAKE_TIMEOUT));
    });
    let mut socket = match rx.recv_timeout(dial::CONNECT_DEADLINE) {
        Ok(result) => result?,
        Err(_) => {
            attempt.abandon();
            return Err("no answer from the relay".into());
        }
    };
    set_low_latency(&mut socket);
    set_send_buffer(&mut socket, SEND_BUFFER);
    let hello = json!({ "type": "file-lane-attach", "deskId": desk_id, "token": token }).to_string();
    match socket.send(Message::Text(hello.into())) {
        Ok(()) => {}
        Err(tungstenite::Error::Io(ref e)) if e.kind() == ErrorKind::WouldBlock => {}
        Err(e) => return Err(e.to_string()),
    }
    let deadline = Instant::now() + ACK_WAIT;
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                let v: Value = serde_json::from_str(&text).unwrap_or_default();
                match v.get("type").and_then(Value::as_str) {
                    Some("file-lane-ack") => return Ok(socket),
                    Some("error") => return Err(v["message"].as_str().unwrap_or("refused by the relay").to_string()),
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if Instant::now() >= deadline {
                    return Err("the relay did not confirm the lane".into());
                }
                let _ = socket.flush();
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Moves messages both ways until the lane drops or is closed; returns why it ended.
fn pump(lane: &FileLane, socket: &mut Socket, queued: &Receiver<Outbound>, sink: &Sink) -> String {
    let mut last_rx = Instant::now();
    loop {
        if lane.is_closed() {
            return "closed".into();
        }
        let mut busy = false;
        // Write while the socket takes it: a new message only once the previous one left tungstenite's buffer,
        // so at most one message waits outside the kernel.
        loop {
            match socket.flush() {
                Ok(()) => {}
                Err(tungstenite::Error::Io(ref e)) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return e.to_string(),
            }
            let Ok((msg, done)) = queued.try_recv() else { break };
            busy = true;
            match socket.write(msg) {
                Ok(()) => {}
                // Buffered; the next flush sends it.
                Err(tungstenite::Error::Io(ref e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => {
                    if let Some(done) = done {
                        done(false);
                    }
                    return e.to_string();
                }
            }
            if let Some(done) = done {
                done(true);
            }
        }
        while lane.window.load(Ordering::SeqCst) < RECEIVE_WINDOW {
            let event = match socket.read() {
                Ok(Message::Text(text)) => Some(LaneEvent::Text(text.to_string())),
                Ok(Message::Binary(bin)) => Some(LaneEvent::Binary(bin.to_vec())),
                Ok(Message::Close(_)) => return "closed by the relay".into(),
                // Pings are answered by tungstenite on the next flush.
                Ok(_) => None,
                Err(tungstenite::Error::Io(ref e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    break;
                }
                Err(e) => return e.to_string(),
            };
            last_rx = Instant::now();
            busy = true;
            if let Some(event) = event {
                let len = match &event {
                    LaneEvent::Text(t) => t.len(),
                    LaneEvent::Binary(b) => b.len(),
                    _ => 0,
                };
                lane.window.fetch_add(len, Ordering::SeqCst);
                if sink.send((lane.generation, event)).is_err() {
                    return "the relay connection is gone".into();
                }
            }
        }
        if last_rx.elapsed() >= SILENCE {
            return format!("no data from the relay for {} s", SILENCE.as_secs());
        }
        if !busy {
            thread::sleep(TICK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Mutex;

    /// A fake relay: accepts one connection, checks the attach, then runs `script` on it.
    fn fake_relay(
        token: &'static str,
        script: impl FnOnce(&mut tungstenite::WebSocket<std::net::TcpStream>) + Send + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let hello: Value = serde_json::from_str(&ws.read().unwrap().into_text().unwrap()).unwrap();
            if hello["type"] != "file-lane-attach" || hello["deskId"] != "111111111" || hello["token"] != token {
                let _ = ws.send(Message::Text(r#"{"type":"error","message":"File lane refused."}"#.into()));
                return;
            }
            ws.send(Message::Text(r#"{"type":"file-lane-ack"}"#.into())).unwrap();
            script(&mut ws);
        });
        url
    }

    fn start(url: &str) -> (Arc<FileLane>, Receiver<(u64, LaneEvent)>) {
        let (sink, events) = mpsc::channel();
        let lane_slot: Arc<Mutex<Option<Arc<FileLane>>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&lane_slot);
        // The main connection answers a token request (as soon as the lane exists).
        let request: Box<dyn Fn() + Send> = Box::new(move || {
            let slot = Arc::clone(&slot);
            thread::spawn(move || {
                for _ in 0..200 {
                    if let Some(lane) = slot.lock().unwrap().as_ref() {
                        lane.give_token("t0k3n".into());
                        return;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            });
        });
        let lane = FileLane::start(7, url, "111111111", sink, request);
        *lane_slot.lock().unwrap() = Some(Arc::clone(&lane));
        (lane, events)
    }

    fn next(events: &Receiver<(u64, LaneEvent)>) -> LaneEvent {
        let (generation, event) = events.recv_timeout(Duration::from_secs(10)).expect("lane event");
        assert_eq!(generation, 7);
        event
    }

    #[test]
    fn carries_messages_both_ways_and_reports_its_loss() {
        let (got_tx, got_rx) = mpsc::channel();
        let url = fake_relay("t0k3n", move |ws| {
            for _ in 0..2 {
                let _ = got_tx.send(ws.read().unwrap());
            }
            ws.send(Message::Text(r#"{"type":"file-transfer","fromClean":"222222222"}"#.into())).unwrap();
            ws.send(Message::Binary(vec![0x10, 1, 2, 3].into())).unwrap();
            ws.close(None).unwrap();
            let _ = ws.flush();
        });
        let (lane, events) = start(&url);
        assert_eq!(next(&events), LaneEvent::Up);
        assert!(lane.is_up());
        let delivered = Arc::new(AtomicUsize::new(0));
        for msg in [Message::Text("{\"type\":\"relay-file\"}".into()), Message::Binary(vec![0x10; 64].into())] {
            let d = Arc::clone(&delivered);
            let done: OnDelivered = Box::new(move |ok| {
                if ok {
                    d.fetch_add(1, Ordering::SeqCst);
                }
            });
            assert!(lane.send(msg, Some(done)));
        }
        assert!(matches!(got_rx.recv_timeout(Duration::from_secs(5)).unwrap(), Message::Text(_)));
        assert_eq!(got_rx.recv_timeout(Duration::from_secs(5)).unwrap(), Message::Binary(vec![0x10; 64].into()));
        assert_eq!(delivered.load(Ordering::SeqCst), 2);
        assert!(matches!(next(&events), LaneEvent::Text(t) if t.contains("file-transfer")));
        assert_eq!(next(&events), LaneEvent::Binary(vec![0x10, 1, 2, 3]));
        assert_eq!(next(&events), LaneEvent::Lost);
        assert!(!lane.is_up());
        // While down, nothing is queued: the caller learns at once.
        let failed = Arc::new(AtomicBool::new(false));
        let f = Arc::clone(&failed);
        assert!(!lane.send(Message::Text("x".into()), Some(Box::new(move |ok| f.store(!ok, Ordering::SeqCst)))));
        assert!(failed.load(Ordering::SeqCst));
        lane.close();
    }

    #[test]
    fn a_refused_attach_never_comes_up() {
        let url = fake_relay("another", |_| {});
        let (lane, events) = start(&url);
        assert!(events.recv_timeout(Duration::from_millis(1500)).is_err(), "no event for a refused lane");
        assert!(!lane.is_up());
        lane.close();
    }
}
