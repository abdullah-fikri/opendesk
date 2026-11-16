//! Reliable, ordered byte stream for file transfers over the P2P datagram path (`file-p2p`): sequence-numbered
//! segments, cumulative plus selective acknowledgements, fast and timeout retransmits, and a delay-based window
//! (LEDBAT-like) that grows while the path's queueing delay stays under [`TARGET_DELAY`] and backs off before it
//! hurts the video sharing the path. The receiver advertises how much it can still buffer, so a slow disk throttles
//! the sender instead of filling memory.
//!
//! The stream carries whole messages: `[kind u8][len u32 BE][payload]`. No I/O here; `infra::p2p` moves the
//! segments and acknowledgements.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// Queueing delay the window aims for. Small on purpose: the same path carries interactive video.
pub const TARGET_DELAY: Duration = Duration::from_millis(8);
/// Window bounds, in segments (about 1.2 KB each). The upper bound is the reach of the selective
/// acknowledgement: 1.2 MB per round trip, about 1 GB/s on a LAN and 60 MB/s at 20 ms.
pub const MIN_WINDOW: f64 = 4.0;
pub const MAX_WINDOW: f64 = SACK_BITS as f64;
/// Out-of-order segments the receiver keeps (and the reach of the selective acknowledgement bitmap's sender
/// bookkeeping); later ones are dropped and resent.
pub const MAX_REORDER: u32 = 8192;
/// Selective acknowledgement bitmap after the cumulative point, in bits; also the most segments the sender has
/// outstanding beyond the oldest unacknowledged one, so every segment in flight can be acknowledged selectively.
pub const SACK_BITS: u32 = 1024;
/// Receiver: acknowledge at least every this many segments.
const ACK_EVERY: u32 = 8;
/// Segment flag: acknowledge at once (the sender has nothing more queued, or this is a retransmit).
pub const FLAG_PUSH: u8 = 1;
/// Retransmit timeout bounds.
const MIN_RTO: Duration = Duration::from_millis(40);
const MAX_RTO: Duration = Duration::from_secs(1);
/// Segments acknowledged above a missing one before it counts as lost.
const DUP_THRESHOLD: usize = 3;
/// How long the lowest round trip seen stays the base (path changes raise it).
const BASE_RTT_WINDOW: Duration = Duration::from_secs(10);

/// Message kinds in the stream.
pub const MSG_JSON: u8 = 1;
pub const MSG_BINARY: u8 = 2;
const MSG_HEADER: usize = 5;
/// Largest message accepted (a binary file chunk is 128 KiB plus headers).
pub const MAX_MESSAGE: usize = 1 << 20;

/// Called once per pushed message: `true` when the receiver acknowledged its last byte, `false` when the stream
/// was dropped first.
pub type OnDelivered = Box<dyn FnOnce(bool) + Send>;

struct Sent {
    data: Vec<u8>,
    sent: Instant,
    /// Sent more than once: its round trip is ambiguous (Karn).
    resent: bool,
    sacked: bool,
    /// Marked lost, waiting to go out again.
    lost: bool,
}

/// Sending half of a stream.
pub struct BulkSender {
    seg: usize,
    /// Segments ready to send, and the partly filled last one.
    ready: VecDeque<Vec<u8>>,
    tail: Vec<u8>,
    next_seq: u32,
    inflight: BTreeMap<u32, Sent>,
    /// Callbacks waiting for the cumulative acknowledgement to pass the sequence after their message's end.
    waiting: VecDeque<(u32, OnDelivered)>,
    /// Callbacks of messages whose last byte is still in `tail` or `ready` (their end sequence is not known yet).
    unsent: VecDeque<(u64, OnDelivered)>,
    /// Stream bytes pushed so far, and bytes already cut into segments.
    pushed: u64,
    cut: u64,
    cwnd: f64,
    slow_start: bool,
    /// Receiver window in segments.
    rwnd: u32,
    srtt: Option<Duration>,
    rttvar: Duration,
    base: Option<(Duration, Instant)>,
    /// Queueing delay (round trip above the base), lightly smoothed so the window reacts within a round trip.
    queue: Duration,
    /// No further window cut until this sequence is acknowledged (one cut per loss event).
    recovery_until: Option<u32>,
    /// Bytes resent, for statistics.
    pub resent: u64,
}

fn after(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

impl BulkSender {
    /// `seg`: payload bytes per segment.
    pub fn new(seg: usize) -> Self {
        Self {
            seg,
            ready: VecDeque::new(),
            tail: Vec::new(),
            next_seq: 0,
            inflight: BTreeMap::new(),
            waiting: VecDeque::new(),
            unsent: VecDeque::new(),
            pushed: 0,
            cut: 0,
            cwnd: 16.0,
            slow_start: true,
            rwnd: MAX_REORDER,
            srtt: None,
            rttvar: Duration::ZERO,
            base: None,
            queue: Duration::ZERO,
            recovery_until: None,
            resent: 0,
        }
    }

    /// Queues one message; `done` runs once it is acknowledged (or the stream is dropped).
    pub fn push(&mut self, kind: u8, payload: &[u8], done: Option<OnDelivered>) {
        let mut msg = Vec::with_capacity(MSG_HEADER + payload.len());
        msg.push(kind);
        msg.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        msg.extend_from_slice(payload);
        self.pushed += msg.len() as u64;
        if let Some(done) = done {
            self.unsent.push_back((self.pushed, done));
        }
        let mut rest = &msg[..];
        while !rest.is_empty() {
            let room = self.seg - self.tail.len();
            let n = room.min(rest.len());
            self.tail.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if self.tail.len() == self.seg {
                self.ready.push_back(std::mem::take(&mut self.tail));
            }
        }
    }

    /// Nothing queued and nothing waiting for an acknowledgement.
    pub fn is_idle(&self) -> bool {
        self.inflight.is_empty() && self.ready.is_empty() && self.tail.is_empty()
    }

    /// Segments the window allows in flight now.
    pub fn window(&self) -> usize {
        (self.cwnd as u32).min(self.rwnd).max(1) as usize
    }

    /// Bytes per second the window allows at the current round trip, for pacing (None: no estimate yet).
    pub fn rate(&self) -> Option<f64> {
        let rtt = self.srtt?.max(Duration::from_micros(200));
        Some(self.window() as f64 * self.seg as f64 / rtt.as_secs_f64())
    }

    fn rto(&self) -> Duration {
        match self.srtt {
            Some(s) => (s + self.rttvar * 4).clamp(MIN_RTO, MAX_RTO),
            None => Duration::from_millis(250),
        }
    }

    /// The next segment to put on the wire, `(seq, flags, data)`: lost segments first, then new ones while the
    /// window has room. None when nothing may go now.
    pub fn next(&mut self, now: Instant) -> Option<(u32, u8, Vec<u8>)> {
        // Segments the network may still hold: neither acknowledged nor given up as lost.
        let outstanding = self.inflight.values().filter(|s| !s.sacked && !s.lost).count();
        if outstanding >= self.window() {
            return None;
        }
        if let Some((&seq, s)) = self.inflight.iter_mut().find(|(_, s)| s.lost) {
            s.lost = false;
            s.resent = true;
            s.sent = now;
            self.resent += s.data.len() as u64;
            return Some((seq, FLAG_PUSH, s.data.clone()));
        }
        if self.inflight.first_key_value().is_some_and(|(&low, _)| self.next_seq.wrapping_sub(low) >= SACK_BITS) {
            return None;
        }
        let data = match self.ready.pop_front() {
            Some(d) => d,
            None if !self.tail.is_empty() => std::mem::take(&mut self.tail),
            None => return None,
        };
        let seq = self.next_seq;
        self.next_seq = seq.wrapping_add(1);
        self.cut += data.len() as u64;
        // Messages whose last byte is in this segment are delivered once the acknowledgement passes it.
        while self.unsent.front().is_some_and(|(end, _)| *end <= self.cut) {
            let (_, done) = self.unsent.pop_front().unwrap();
            self.waiting.push_back((seq.wrapping_add(1), done));
        }
        // Ask for an acknowledgement at once when nothing follows soon: the queue ran dry or the window is full.
        let last = (self.ready.is_empty() && self.tail.is_empty()) || outstanding + 1 >= self.window();
        let flags = if last { FLAG_PUSH } else { 0 };
        self.inflight.insert(seq, Sent { data: data.clone(), sent: now, resent: false, sacked: false, lost: false });
        Some((seq, flags, data))
    }

    /// Applies an acknowledgement: everything before `cum` arrived, plus bit `i` of `sack` = segment
    /// `cum + 1 + i`; `rwnd` is how many more segments the receiver can buffer.
    pub fn on_ack(&mut self, now: Instant, cum: u32, rwnd: u32, sack: &[u8]) {
        self.rwnd = rwnd.max(1);
        let mut acked = 0u32;
        let mut sample = None;
        let below: Vec<u32> = self.inflight.range(..).map(|(&s, _)| s).filter(|&s| after(cum, s)).collect();
        for seq in below {
            let s = self.inflight.remove(&seq).unwrap();
            if !s.sacked {
                acked += 1;
                if !s.resent {
                    sample = Some(now.duration_since(s.sent));
                }
            }
        }
        for (i, byte) in sack.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) == 0 {
                    continue;
                }
                let seq = cum.wrapping_add(1 + (i * 8 + bit) as u32);
                if let Some(s) = self.inflight.get_mut(&seq)
                    && !s.sacked
                {
                    s.sacked = true;
                    s.lost = false;
                    acked += 1;
                    if !s.resent {
                        sample = Some(now.duration_since(s.sent));
                    }
                }
            }
        }
        while self.waiting.front().is_some_and(|(end, _)| !after(*end, cum)) {
            let (_, done) = self.waiting.pop_front().unwrap();
            done(true);
        }
        if let Some(rtt) = sample {
            self.on_rtt(now, rtt);
        }
        if self.recovery_until.is_some_and(|r| !after(r, cum)) {
            self.recovery_until = None;
        }
        // Fast retransmit: a segment with enough acknowledged ones above it is lost.
        let sacked_above = |seq: u32, inflight: &BTreeMap<u32, Sent>| {
            inflight.range(seq.wrapping_add(1)..).filter(|(_, s)| s.sacked).count()
        };
        let mut lost_any = false;
        let candidates: Vec<u32> =
            self.inflight.iter().filter(|(_, s)| !s.sacked && !s.lost).map(|(&q, _)| q).collect();
        for seq in candidates {
            if sacked_above(seq, &self.inflight) >= DUP_THRESHOLD
                && self.inflight.get(&seq).is_some_and(|s| now.duration_since(s.sent) >= self.srtt.unwrap_or_default())
            {
                self.inflight.get_mut(&seq).unwrap().lost = true;
                lost_any = true;
            }
        }
        if lost_any {
            self.on_loss();
        } else if acked > 0 {
            self.grow(acked);
        }
    }

    /// Marks segments lost whose acknowledgement is overdue; call regularly.
    pub fn on_timer(&mut self, now: Instant) {
        let rto = self.rto();
        let mut lost_any = false;
        for s in self.inflight.values_mut() {
            if !s.sacked && !s.lost && now.duration_since(s.sent) >= rto {
                s.lost = true;
                lost_any = true;
            }
        }
        if lost_any {
            self.on_loss();
        }
    }

    /// Ends the stream: every waiting callback learns its message was not delivered.
    pub fn abort(&mut self) {
        let waiting = self.waiting.drain(..).map(|(_, d)| d);
        for done in waiting.chain(self.unsent.drain(..).map(|(_, d)| d)) {
            done(false);
        }
        self.inflight.clear();
        self.ready.clear();
        self.tail.clear();
    }

    fn on_rtt(&mut self, now: Instant, rtt: Duration) {
        match self.srtt {
            None => {
                self.srtt = Some(rtt);
                self.rttvar = rtt / 2;
            }
            Some(s) => {
                let diff = s.abs_diff(rtt);
                self.rttvar = (self.rttvar * 3 + diff) / 4;
                self.srtt = Some((s * 7 + rtt) / 8);
            }
        }
        match self.base {
            Some((b, at)) if rtt >= b && now.duration_since(at) < BASE_RTT_WINDOW => {}
            _ => self.base = Some((rtt, now)),
        }
        let q = rtt.saturating_sub(self.base.map_or(rtt, |(b, _)| b));
        self.queue = (self.queue * 3 + q) / 4;
    }

    fn grow(&mut self, acked: u32) {
        let q = self.queue.as_secs_f64();
        let target = TARGET_DELAY.as_secs_f64();
        let share = acked as f64 / self.cwnd;
        if q > target {
            // Past the target: shrink multiplicatively, down to half per round trip at twice the target.
            self.slow_start = false;
            self.cwnd *= 1.0 - 0.5 * ((q - target) / target).min(1.0) * share.min(1.0);
        } else if self.slow_start && q < target / 2.0 {
            self.cwnd += acked as f64;
        } else {
            self.slow_start = false;
            // LEDBAT: up to one segment per round trip at zero delay, less towards the target.
            self.cwnd += (target - q) / target * share;
        }
        self.cwnd = self.cwnd.clamp(MIN_WINDOW, MAX_WINDOW);
    }

    fn on_loss(&mut self) {
        if self.recovery_until.is_some() {
            return;
        }
        self.slow_start = false;
        self.cwnd = (self.cwnd / 2.0).max(MIN_WINDOW);
        self.recovery_until = Some(self.next_seq);
    }
}

/// Receiving half of a stream.
#[derive(Default)]
pub struct BulkReceiver {
    next: u32,
    held: BTreeMap<u32, Vec<u8>>,
    /// Stream bytes not yet forming a whole message.
    partial: Vec<u8>,
    since_ack: u32,
    /// The stream broke (a message over [`MAX_MESSAGE`]); nothing more is delivered.
    pub broken: bool,
}

/// What one segment produced.
#[derive(Debug, Default, PartialEq)]
pub struct Received {
    /// Whole messages, in order: `(kind, payload)`.
    pub messages: Vec<(u8, Vec<u8>)>,
    /// Send an acknowledgement now.
    pub ack: bool,
}

impl BulkReceiver {
    /// Takes segment `seq`.
    pub fn on_segment(&mut self, seq: u32, flags: u8, data: &[u8]) -> Received {
        let mut out = Received::default();
        let ahead = seq.wrapping_sub(self.next);
        if ahead >= MAX_REORDER {
            // Old (a duplicate): the acknowledgement that covered it was probably lost.
            out.ack = true;
            return out;
        }
        if ahead > 0 {
            self.held.entry(seq).or_insert_with(|| data.to_vec());
            out.ack = true; // a gap: tell the sender at once
            return out;
        }
        self.take(data, &mut out);
        self.next = self.next.wrapping_add(1);
        while let Some(d) = self.held.remove(&self.next) {
            self.take(&d, &mut out);
            self.next = self.next.wrapping_add(1);
        }
        self.since_ack += 1;
        out.ack = flags & FLAG_PUSH != 0 || self.since_ack >= ACK_EVERY || !self.held.is_empty();
        out
    }

    fn take(&mut self, data: &[u8], out: &mut Received) {
        if self.broken {
            return;
        }
        self.partial.extend_from_slice(data);
        let mut at = 0;
        while self.partial.len() - at >= MSG_HEADER {
            let len = u32::from_be_bytes(self.partial[at + 1..at + 5].try_into().unwrap()) as usize;
            if len > MAX_MESSAGE {
                self.broken = true;
                self.partial.clear();
                return;
            }
            if self.partial.len() - at < MSG_HEADER + len {
                break;
            }
            let kind = self.partial[at];
            out.messages.push((kind, self.partial[at + MSG_HEADER..at + MSG_HEADER + len].to_vec()));
            at += MSG_HEADER + len;
        }
        self.partial.drain(..at);
    }

    /// The acknowledgement: `[cum u32][rwnd u32][sack bitmap]`, with `rwnd` the segments the caller can still take.
    pub fn ack(&mut self, rwnd: u32) -> Vec<u8> {
        self.since_ack = 0;
        let mut out = Vec::with_capacity(8 + (SACK_BITS / 8) as usize);
        out.extend_from_slice(&self.next.to_be_bytes());
        out.extend_from_slice(&rwnd.to_be_bytes());
        let mut bits = [0u8; (SACK_BITS / 8) as usize];
        for &seq in self.held.keys() {
            let i = seq.wrapping_sub(self.next).wrapping_sub(1);
            if i < SACK_BITS {
                bits[(i / 8) as usize] |= 1 << (i % 8);
            }
        }
        out.extend_from_slice(&bits);
        out
    }
}

/// Splits an acknowledgement built by [`BulkReceiver::ack`].
pub fn parse_ack(body: &[u8]) -> Option<(u32, u32, &[u8])> {
    if body.len() < 8 {
        return None;
    }
    let cum = u32::from_be_bytes(body[0..4].try_into().ok()?);
    let rwnd = u32::from_be_bytes(body[4..8].try_into().ok()?);
    Some((cum, rwnd, &body[8..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Runs a transfer of `msgs` through a path that loses the datagrams `lose` picks (by send count), with a
    /// fixed one-way delay; returns the messages received, acknowledged callbacks and resent bytes.
    fn run(msgs: &[Vec<u8>], lose: impl Fn(usize) -> bool) -> (Vec<Vec<u8>>, usize, u64) {
        let mut tx = BulkSender::new(1000);
        let mut rx = BulkReceiver::default();
        let delivered = Arc::new(AtomicUsize::new(0));
        for m in msgs {
            let d = Arc::clone(&delivered);
            tx.push(
                MSG_BINARY,
                m,
                Some(Box::new(move |ok| {
                    assert!(ok);
                    d.fetch_add(1, Ordering::SeqCst);
                })),
            );
        }
        let start = Instant::now();
        let delay = Duration::from_millis(5);
        let mut t = start;
        let mut wire: VecDeque<(Instant, u32, u8, Vec<u8>)> = VecDeque::new();
        let mut acks: VecDeque<(Instant, Vec<u8>)> = VecDeque::new();
        let mut got = Vec::new();
        let mut sent = 0usize;
        while !tx.is_idle() {
            assert!(
                t - start < Duration::from_secs(30),
                "stuck: {} in flight, window {}, receiver at {}",
                tx.inflight.len(),
                tx.cwnd,
                rx.next
            );
            while let Some((seq, flags, data)) = tx.next(t) {
                sent += 1;
                if !lose(sent) {
                    wire.push_back((t + delay, seq, flags, data));
                }
            }
            while wire.front().is_some_and(|(at, ..)| *at <= t) {
                let (_, seq, flags, data) = wire.pop_front().unwrap();
                let r = rx.on_segment(seq, flags, &data);
                got.extend(r.messages.into_iter().map(|(k, p)| {
                    assert_eq!(k, MSG_BINARY);
                    p
                }));
                if r.ack {
                    acks.push_back((t + delay, rx.ack(MAX_REORDER)));
                }
            }
            while acks.front().is_some_and(|(at, _)| *at <= t) {
                let (_, a) = acks.pop_front().unwrap();
                let (cum, rwnd, sack) = parse_ack(&a).unwrap();
                tx.on_ack(t, cum, rwnd, sack);
            }
            tx.on_timer(t);
            t += Duration::from_micros(500);
        }
        (got, delivered.load(Ordering::SeqCst), tx.resent)
    }

    fn messages() -> Vec<Vec<u8>> {
        (0..40u32).map(|n| (0..(n as usize * 997 % 20_000 + 1)).map(|i| (i as u32 * 31 + n) as u8).collect()).collect()
    }

    #[test]
    fn messages_arrive_whole_and_in_order() {
        let msgs = messages();
        let (got, delivered, resent) = run(&msgs, |_| false);
        assert_eq!(got, msgs);
        assert_eq!(delivered, msgs.len());
        assert_eq!(resent, 0);
    }

    #[test]
    fn lost_segments_are_resent() {
        let msgs = messages();
        // About 5% loss, including retransmits now and then.
        let (got, delivered, resent) = run(&msgs, |n| n % 19 == 0);
        assert_eq!(got, msgs);
        assert_eq!(delivered, msgs.len());
        assert!(resent > 0);
    }

    #[test]
    fn large_chunks_cross_a_lossy_path_with_few_resends() {
        let msgs: Vec<Vec<u8>> = (0..16).map(|n| vec![n as u8; 128 * 1024]).collect();
        let (got, delivered, resent) = run(&msgs, |n| n % 20 == 0);
        assert_eq!(got, msgs);
        assert_eq!(delivered, msgs.len());
        assert!(resent < 16 * 128 * 1024 / 10, "{resent} bytes resent for 5% loss");
    }

    #[test]
    fn a_lost_tail_is_recovered_by_the_timer() {
        let msgs = vec![vec![7u8; 2500]];
        // The last of three segments is lost once: nothing after it can reveal the loss.
        let (got, ..) = run(&msgs, |n| n == 3);
        assert_eq!(got, msgs);
    }

    #[test]
    fn the_window_respects_the_receiver() {
        let mut tx = BulkSender::new(100);
        tx.push(MSG_BINARY, &[0u8; 5000], None);
        let t = Instant::now();
        tx.on_ack(t, 0, 2, &[]);
        assert!(tx.next(t).is_some() && tx.next(t).is_some());
        assert!(tx.next(t).is_none(), "receiver has room for two segments");
    }

    #[test]
    fn queueing_delay_stops_growth() {
        let mut tx = BulkSender::new(100);
        tx.push(MSG_BINARY, &vec![0u8; 400_000], None);
        let mut t = Instant::now();
        let mut rtt = Duration::from_millis(2);
        for round in 0..200 {
            let mut seqs = Vec::new();
            while let Some((seq, ..)) = tx.next(t) {
                seqs.push(seq);
            }
            // The queue builds up with the window: each round adds delay in proportion.
            rtt = Duration::from_millis(2) + Duration::from_micros((tx.window() as u64) * 20);
            t += rtt;
            if let Some(&last) = seqs.last() {
                tx.on_ack(t, last.wrapping_add(1), MAX_REORDER, &[]);
            }
            let _ = round;
        }
        let q = rtt - Duration::from_millis(2);
        assert!(q < TARGET_DELAY * 2, "queueing delay {q:?} ran past the target");
    }

    #[test]
    fn an_oversized_message_breaks_the_stream() {
        let mut rx = BulkReceiver::default();
        let mut seg = vec![MSG_BINARY];
        seg.extend_from_slice(&((MAX_MESSAGE as u32) + 1).to_be_bytes());
        assert!(rx.on_segment(0, 0, &seg).messages.is_empty());
        assert!(rx.broken);
    }

    #[test]
    fn aborting_reports_undelivered_messages() {
        let mut tx = BulkSender::new(100);
        let n = Arc::new(AtomicUsize::new(0));
        let m = Arc::clone(&n);
        tx.push(
            MSG_JSON,
            b"{}",
            Some(Box::new(move |ok| {
                assert!(!ok);
                m.fetch_add(1, Ordering::SeqCst);
            })),
        );
        tx.next(Instant::now());
        tx.abort();
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
