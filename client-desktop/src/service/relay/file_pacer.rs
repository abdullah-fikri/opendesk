//! Paces file messages on the relay socket so a transfer never starves the session (see
//! `domain::file_pacing`).
//!
//! While file traffic flows, the connection sends a WebSocket ping with a sequence number after the file bytes
//! it wrote; the relay answers it only after reading everything before it. That round trip is the delay
//! signal: it grows with every queue between here and the relay (our socket buffer, the modem, the relay's
//! backpressure from a slow receiver), the same queues that delay the session's P2P datagrams and input. No new
//! message: pings are part of WebSocket and every relay answers them.
//!
//! The same pong tells senders how far the relay has read their files: every file message written before the
//! ping has arrived there, so progress follows the network rather than the queue (see `FileMark`).
//!
//! `OPENDESK_FILE_RATE_LIMIT=0` turns pacing off: files go out as fast as the per-tick budget allows, and a
//! file message counts as delivered once it is handed to the socket.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tungstenite::Message;

use super::connection::Socket;
use crate::domain::file_pacing::{FileRate, TokenBucket};
use crate::infra::logger;
use crate::protocol::FileMark;
use crate::service::file_transfer;

/// Shortest pause between two probes.
const PROBE_EVERY: Duration = Duration::from_millis(50);
/// A probe unanswered this long is given up (a relay or proxy that drops pings); the connection's own silence
/// check handles a dead socket.
const PROBE_LOST: Duration = Duration::from_secs(10);
/// Most delivery marks kept; the oldest is reported early when a huge queue exceeds it.
const MAX_MARKS: usize = 8192;
/// How often the current rate is logged while files flow.
const LOG_EVERY: Duration = Duration::from_secs(10);

/// Kill switch: `OPENDESK_FILE_RATE_LIMIT=0` sends files unpaced, as before pacing existed.
pub fn enabled() -> bool {
    std::env::var("OPENDESK_FILE_RATE_LIMIT").map(|v| v != "0").unwrap_or(true)
}

/// A ping on its way: its number, when it left and how many file bytes were written before it.
struct Probe {
    seq: u64,
    sent: Instant,
    written: u64,
}

/// File pacing of one relay connection.
pub struct FilePacer {
    enabled: bool,
    rate: FileRate,
    bucket: TokenBucket,
    /// A file message waited for the rate since the last sample.
    limited: bool,
    probe: Option<Probe>,
    next_seq: u64,
    last_probe: Option<Instant>,
    /// File bytes written to the socket on this connection, and how many of them the last answered probe
    /// covered.
    written: u64,
    confirmed: u64,
    /// Transfer progress waiting for a probe to come back: (`written` once its message was written, mark).
    marks: VecDeque<(u64, FileMark)>,
    last_log: Option<Instant>,
    lost_logged: bool,
}

impl FilePacer {
    /// A pacer for a new connection.
    pub fn new() -> Self {
        Self {
            enabled: enabled(),
            rate: FileRate::new(),
            bucket: TokenBucket::default(),
            limited: false,
            probe: None,
            next_seq: 0,
            last_probe: None,
            written: 0,
            confirmed: 0,
            marks: VecDeque::new(),
            last_log: None,
            lost_logged: false,
        }
    }

    /// True when a file message of `bytes` may be written at `now`; it is then charged to the rate.
    pub fn allow(&mut self, bytes: usize, now: Instant) -> bool {
        if !self.enabled {
            return true;
        }
        let ok = self.bucket.try_take(bytes, self.rate.rate(), now);
        self.limited |= !ok;
        ok
    }

    /// A file message of `bytes` was handed to the socket; `mark` says how far its transfer is with it.
    pub fn on_written(&mut self, bytes: usize, mark: Option<FileMark>) {
        self.written += bytes as u64;
        let Some(mark) = mark else { return };
        if !self.enabled {
            // No probes without pacing: a message counts as delivered once it is in the socket.
            return file_transfer::set_delivered(&mark);
        }
        if self.marks.len() >= MAX_MARKS
            && let Some((_, oldest)) = self.marks.pop_front()
        {
            file_transfer::set_delivered(&oldest);
        }
        self.marks.push_back((self.written, mark));
    }

    /// Every file byte up to `written` has reached the relay: report the marks written by then.
    fn confirm(&mut self, written: u64) {
        self.confirmed = written;
        while self.marks.front().is_some_and(|(end, _)| *end <= written) {
            if let Some((_, mark)) = self.marks.pop_front() {
                file_transfer::set_delivered(&mark);
            }
        }
    }

    /// Sends a probe when one is due: while file messages wait in the connection queue (`queued`) or file
    /// bytes were written that no probe has covered yet.
    pub fn poll(&mut self, socket: &mut Socket, queued: bool, now: Instant) {
        if !self.enabled {
            return;
        }
        if let Some(probe) = &self.probe {
            let age = now.duration_since(probe.sent);
            self.rate.on_waiting(age, now);
            if age >= PROBE_LOST {
                let written = probe.written;
                self.probe = None;
                if !std::mem::replace(&mut self.lost_logged, true) {
                    logger::warn(
                        "file_transfer",
                        "The relay did not answer a ping in 10 s; pacing on the last rate, progress estimated.",
                    );
                }
                // Progress must not stall on a relay or proxy that drops pings.
                self.confirm(written);
            }
            return;
        }
        let due = queued || self.written > self.confirmed;
        if !due || self.last_probe.is_some_and(|at| now.duration_since(at) < PROBE_EVERY) {
            return;
        }
        self.next_seq += 1;
        let seq = self.next_seq;
        // A WouldBlock leaves the ping buffered behind the file bytes, which is where it belongs; any other
        // error means the socket is dying and the read loop notices.
        let _ = socket.send(Message::Ping(seq.to_be_bytes().to_vec().into()));
        self.probe = Some(Probe { seq, sent: now, written: self.written });
        self.last_probe = Some(now);
    }

    /// A pong from the relay; one that answers no probe of ours is ignored.
    pub fn on_pong(&mut self, payload: &[u8], now: Instant) {
        let seq = <[u8; 8]>::try_from(payload).ok().map(u64::from_be_bytes);
        let Some(probe) = self.probe.take_if(|p| seq == Some(p.seq)) else { return };
        let rtt = now.duration_since(probe.sent);
        self.rate.on_rtt(rtt, std::mem::take(&mut self.limited), now);
        self.confirm(probe.written);
        if self.last_log.is_none_or(|at| now.duration_since(at) >= LOG_EVERY) {
            self.last_log = Some(now);
            logger::info(
                "file_transfer",
                &format!(
                    "File pacing: {} KB/s, round trip {} ms (base {} ms).",
                    self.rate.rate() / 1024,
                    rtt.as_millis(),
                    self.rate.base_rtt().unwrap_or_default().as_millis()
                ),
            );
        }
    }
}
