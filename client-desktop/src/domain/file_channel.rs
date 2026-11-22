//! The direct file channel's wire format (`file-channel`), without I/O: how messages become a byte stream and back,
//! the `p2p-signal` that carries the candidates, how striped streams are put back in order ([`Reorder`]) and when
//! a lost channel is set up again ([`Redial`]). `infra::file_channel` moves the bytes.
//!
//! On each TCP stream every frame is `[length u16 BE][Noise transport message]`. The plaintext of the frames, in
//! order, is a stream of messages `[kind u8][length u32 BE][payload]`: the file stream kinds of `domain::bulk`
//! (JSON file messages, binary chunks) plus a few of the channel's own (`0xF0` and up).
//!
//! Striping: one TCP flow is held back by its own congestion window, a single loss halves it, and on a long path
//! it takes seconds to grow back. A channel may therefore run up to [`MAX_STREAMS`] streams, each its own TCP
//! connection with its own `Noise_XX` handshake and peer-key check. The viewer's pick says how many it would use
//! ([`Pick`]); a host that can stripe answers with [`KIND_HELLO`], and the viewer then opens the extra streams
//! ([`KIND_JOIN`]). Each side stripes only toward a partner that said it can: every message then follows a
//! [`KIND_SEQ`] naming its place in that direction's order, and the receiving side puts the messages back in
//! that order before anything handles them, so the file stream above sees exactly what one stream would carry.
//! Peers that know none of this send an empty pick and no hello, and keep one stream without sequence numbers.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Largest plaintext in one Noise transport message (65535 minus the 16-byte tag).
pub const SEGMENT: usize = 65535 - 16;
/// Largest message accepted (a binary chunk of 128 KiB plus its header, with room to spare).
pub const MAX_MESSAGE: usize = 1 << 20;
const HEADER: usize = 5;

/// Channel control: the viewer picked this connection (first message on it; the host adopts the connection it
/// arrives on and drops any other).
pub const KIND_SELECT: u8 = 0xF0;
/// Channel control: keep-alive while no file moves.
pub const KIND_PING: u8 = 0xF1;
/// Channel control, viewer -> host, first message on an extra stream: `[token u64 BE]` of the pick it joins.
pub const KIND_JOIN: u8 = 0xF2;
/// Channel control, host -> viewer, its first message: `[streams u8]`, the most streams the host takes. Only
/// toward a viewer whose pick asked for more than one.
pub const KIND_HELLO: u8 = 0xF3;
/// Channel control: `[seq u64 BE]`, the place of the next message on this stream in its direction's order.
pub const KIND_SEQ: u8 = 0xF4;

/// Most streams of one channel.
pub const MAX_STREAMS: u8 = 8;
/// Streams a channel runs unless `OPENDESK_FILE_STREAMS` says otherwise.
pub const DEFAULT_STREAMS: u8 = 4;

/// True for the channel's own messages, which never reach the file stream.
pub fn is_control(kind: u8) -> bool {
    kind >= KIND_SELECT
}

/// Streams this side offers: `OPENDESK_FILE_STREAMS` (1 turns striping off), else [`DEFAULT_STREAMS`].
pub fn streams_setting(env: Option<&str>) -> u8 {
    env.and_then(|v| v.trim().parse::<u8>().ok()).map_or(DEFAULT_STREAMS, |n| n.clamp(1, MAX_STREAMS))
}

/// The viewer's pick (`KIND_SELECT` payload): how many streams it would run and the token its extra streams
/// present. Viewers before striping send an empty pick: one stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pick {
    pub streams: u8,
    pub token: u64,
}

impl Pick {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![self.streams];
        out.extend_from_slice(&self.token.to_be_bytes());
        out
    }

    pub fn parse(payload: &[u8]) -> Self {
        match payload {
            [streams, token @ ..] if token.len() == 8 => {
                Self { streams: (*streams).clamp(1, MAX_STREAMS), token: u64_be(token).unwrap_or(0) }
            }
            _ => Self { streams: 1, token: 0 },
        }
    }
}

/// `[n u64 BE]`, the payload of [`KIND_JOIN`] and [`KIND_SEQ`].
pub fn u64_payload(n: u64) -> [u8; 8] {
    n.to_be_bytes()
}

/// Reads a [`u64_payload`]; None for any other length.
pub fn u64_be(payload: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(payload.try_into().ok()?))
}

/// Puts the messages of a striped direction back in order. Each stream delivers its messages in order, but the
/// streams race each other, so a message may arrive before an earlier one still on its way on another stream; it
/// waits here until those are through. At most `limit` bytes wait (see [`Reorder::admits`]).
pub struct Reorder {
    next: u64,
    held: BTreeMap<u64, (u8, Vec<u8>)>,
    bytes: usize,
    limit: usize,
}

impl Reorder {
    pub fn new(limit: usize) -> Self {
        Self { next: 0, held: BTreeMap::new(), bytes: 0, limit }
    }

    /// Whether message `seq` of `len` bytes may be taken now. The next one always may: it is at the head of its
    /// stream, so taking it is what unblocks everything held; any other waits (its stream stops reading, and TCP
    /// then slows that stream's sender) while the held bytes would go over the limit.
    pub fn admits(&self, seq: u64, len: usize) -> bool {
        seq == self.next || self.bytes + len <= self.limit
    }

    /// Takes message `seq`; returns the messages now in order (possibly none). None when `seq` was already
    /// delivered or is held twice: a broken or hostile stream, which ends the channel.
    pub fn push(&mut self, seq: u64, kind: u8, payload: Vec<u8>) -> Option<Vec<(u8, Vec<u8>)>> {
        if seq < self.next || self.held.contains_key(&seq) {
            return None;
        }
        self.bytes += payload.len();
        self.held.insert(seq, (kind, payload));
        let mut out = Vec::new();
        while let Some((kind, payload)) = self.held.remove(&self.next) {
            self.bytes -= payload.len();
            self.next += 1;
            out.push((kind, payload));
        }
        Some(out)
    }

    /// Bytes waiting for an earlier message.
    #[cfg(test)]
    pub fn held_bytes(&self) -> usize {
        self.bytes
    }
}

/// When a lost channel is set up again: after a back-off that doubles from [`REDIAL_FIRST`] to [`REDIAL_MAX`],
/// at most [`MAX_REDIALS`] times in a row; a channel that then stays up for [`REDIAL_STABLE`] earns a fresh
/// count. A partner that set up its side again (its new candidates arrived) is answered at once.
#[derive(Clone, Debug, Default)]
pub struct Redial {
    /// Set-ups since the channel was last stable.
    tries: u32,
    retry_at: Option<Instant>,
    up_since: Option<Instant>,
    /// This instance is a set-up again, not the session's first; it counts as failed if not up in time.
    pub again: bool,
}

/// See [`Redial`].
pub const MAX_REDIALS: u32 = 8;
pub const REDIAL_FIRST: Duration = Duration::from_secs(1);
pub const REDIAL_MAX: Duration = Duration::from_secs(30);
pub const REDIAL_STABLE: Duration = Duration::from_secs(60);
/// A channel set up again that is not up after this long is given up and tried once more later.
pub const REDIAL_CONNECT: Duration = Duration::from_secs(20);

impl Redial {
    /// The channel came up.
    pub fn up(&mut self, now: Instant) {
        self.up_since = Some(now);
        self.retry_at = None;
    }

    /// The channel was lost (or a set-up again did not come up): schedule the next try, unless out of tries.
    pub fn lost(&mut self, now: Instant) {
        if self.up_since.take().is_some_and(|t| now.duration_since(t) >= REDIAL_STABLE) {
            self.tries = 0;
        }
        self.retry_at = (self.tries < MAX_REDIALS).then(|| {
            let backoff = REDIAL_FIRST.saturating_mul(1 << self.tries.min(5));
            now + backoff.min(REDIAL_MAX)
        });
    }

    /// Time to set the channel up again.
    pub fn due(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|t| now >= t)
    }

    /// A try is still coming (the back-off runs).
    pub fn pending(&self) -> bool {
        self.retry_at.is_some()
    }

    /// May the channel be set up again now, out of turn (the partner just did)?
    pub fn may_answer(&self) -> bool {
        self.tries < MAX_REDIALS
    }

    /// The state the new instance starts with: one try more, nothing scheduled.
    pub fn next_try(&self) -> Self {
        Self { tries: self.tries + 1, retry_at: None, up_since: None, again: true }
    }
}

/// Encodes one message for the stream.
pub fn encode(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Cuts a message stream into frame plaintexts of at most [`SEGMENT`] bytes.
pub fn segments(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.chunks(SEGMENT)
}

/// Reassembles messages from frame plaintexts.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
}

impl Reader {
    /// Adds a frame's plaintext; returns the messages it completed, or None when the stream is not ours (a message
    /// larger than [`MAX_MESSAGE`]), which ends the connection.
    pub fn push(&mut self, plain: &[u8]) -> Option<Vec<(u8, Vec<u8>)>> {
        self.buf.extend_from_slice(plain);
        let mut out = Vec::new();
        let mut at = 0;
        while self.buf.len() - at >= HEADER {
            let len = u32::from_be_bytes(self.buf[at + 1..at + HEADER].try_into().ok()?) as usize;
            if len > MAX_MESSAGE {
                return None;
            }
            if self.buf.len() - at < HEADER + len {
                break;
            }
            out.push((self.buf[at], self.buf[at + HEADER..at + HEADER + len].to_vec()));
            at += HEADER + len;
        }
        self.buf.drain(..at);
        Some(out)
    }
}

/// The candidates signal: `{"v":2,"fc":["addr", ...]}`. Sent only to partners that list `file-channel`, so
/// older peers never see one.
pub fn signal(cands: &[SocketAddr]) -> String {
    json!({ "v": 2, "fc": cands.iter().map(|a| a.to_string()).collect::<Vec<_>>() }).to_string()
}

/// True for a file channel signal.
pub fn is_signal(v: &Value) -> bool {
    v.get("v").and_then(Value::as_u64) == Some(2) && v.get("fc").is_some_and(Value::is_array)
}

/// The partner's candidates in a signal: at most 8 usable addresses (no unspecified, multicast or port 0).
pub fn parse_signal(v: &Value) -> Vec<SocketAddr> {
    let Some(list) = v.get("fc").and_then(Value::as_array) else { return Vec::new() };
    let mut out: Vec<SocketAddr> = list
        .iter()
        .filter_map(|c| c.as_str()?.parse::<SocketAddr>().ok())
        .filter(|a| a.port() != 0 && !a.ip().is_unspecified() && !a.ip().is_multicast())
        .collect();
    out.dedup();
    out.truncate(8);
    out
}

/// Noise prologue binding the channel to this session's two devices (as the end-to-end handshake does).
pub fn prologue(viewer: &str, host: &str) -> String {
    format!("opendesk-fc-v1|{viewer}|{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_any_cut_of_the_stream() {
        let big = vec![7u8; SEGMENT * 2 + 10];
        let mut stream = encode(2, &big);
        stream.extend(encode(1, b"{\"action\":\"end\"}"));
        stream.extend(encode(KIND_PING, &[]));
        for cut in [1, 3, 5, 4096, SEGMENT] {
            let mut r = Reader::default();
            let mut got = Vec::new();
            for piece in stream.chunks(cut) {
                got.extend(r.push(piece).unwrap());
            }
            assert_eq!(got.len(), 3, "cut {cut}");
            assert_eq!(got[0], (2, big.clone()));
            assert_eq!(got[1].1, b"{\"action\":\"end\"}");
            assert_eq!(got[2], (KIND_PING, vec![]));
        }
        assert!(segments(&stream).all(|s| s.len() <= SEGMENT));
    }

    #[test]
    fn an_oversized_message_ends_the_stream() {
        let mut head = vec![1u8];
        head.extend_from_slice(&((MAX_MESSAGE + 1) as u32).to_be_bytes());
        assert!(Reader::default().push(&head).is_none());
    }

    #[test]
    fn signal_round_trip_keeps_only_usable_addresses() {
        let cands: Vec<SocketAddr> = vec!["192.168.1.5:40000".parse().unwrap(), "[2001:db8::1]:40000".parse().unwrap()];
        let v: Value = serde_json::from_str(&signal(&cands)).unwrap();
        assert!(is_signal(&v));
        assert_eq!(parse_signal(&v), cands);
        let bad: Value = json!({"v": 2, "fc": ["0.0.0.0:1", "1.2.3.4:0", "224.0.0.1:5", "x", "5.6.7.8:9"]});
        assert_eq!(parse_signal(&bad), vec!["5.6.7.8:9".parse::<SocketAddr>().unwrap()]);
        // The end-to-end and P2P path signals are not ours.
        assert!(!is_signal(&json!({"v": 2, "e2e": "x"})));
        assert!(!is_signal(&json!({"v": 2, "c": []})));
    }

    #[test]
    fn picks_hellos_and_settings() {
        let pick = Pick { streams: 4, token: 0xDEAD_BEEF_0102_0304 };
        assert_eq!(Pick::parse(&pick.encode()), pick);
        // A viewer before striping picks with an empty payload: one stream.
        assert_eq!(Pick::parse(&[]), Pick { streams: 1, token: 0 });
        assert_eq!(Pick::parse(&[200, 0, 0, 0, 0, 0, 0, 0, 1]).streams, MAX_STREAMS);
        assert_eq!(Pick::parse(&[0, 0, 0, 0, 0, 0, 0, 0, 1]).streams, 1);
        assert_eq!(u64_be(&u64_payload(77)), Some(77));
        assert_eq!(u64_be(&[1, 2, 3]), None);
        assert_eq!(streams_setting(None), DEFAULT_STREAMS);
        assert_eq!(streams_setting(Some("1")), 1);
        assert_eq!(streams_setting(Some(" 2 ")), 2);
        assert_eq!(streams_setting(Some("0")), 1);
        assert_eq!(streams_setting(Some("99")), MAX_STREAMS);
        assert_eq!(streams_setting(Some("x")), DEFAULT_STREAMS);
        assert!(is_control(KIND_SEQ) && is_control(KIND_PING) && !is_control(crate::domain::bulk::MSG_BINARY));
    }

    /// Messages striped over three streams that race each other come out in the order they were sent.
    #[test]
    fn striped_messages_come_out_in_order() {
        // Message i went to stream i % 3; each stream delivers in order, the streams interleave at random.
        let mut streams: Vec<Vec<u64>> = vec![Vec::new(); 3];
        for i in 0..300u64 {
            streams[(i % 3) as usize].push(i);
        }
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut r = Reorder::new(usize::MAX);
        let mut out = Vec::new();
        while streams.iter().any(|s| !s.is_empty()) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let s = (seed % 3) as usize;
            if streams[s].is_empty() {
                continue;
            }
            let seq = streams[s].remove(0);
            for (kind, payload) in r.push(seq, 2, seq.to_be_bytes().to_vec()).unwrap() {
                assert_eq!(kind, 2);
                out.push(u64_be(&payload).unwrap());
            }
        }
        assert_eq!(out, (0..300).collect::<Vec<_>>());
        assert_eq!(r.held_bytes(), 0);
    }

    #[test]
    fn reorder_bounds_what_waits_and_refuses_repeats() {
        let mut r = Reorder::new(10);
        assert!(r.admits(3, 6));
        assert_eq!(r.push(3, 1, vec![0; 6]).unwrap(), vec![]);
        // Over the limit: a later message waits, but the next in order is always taken.
        assert!(!r.admits(2, 6) && r.admits(0, 1000));
        assert!(r.push(3, 1, vec![]).is_none(), "held twice");
        assert_eq!(r.push(0, 1, vec![1]).unwrap().len(), 1);
        assert_eq!(r.push(1, 1, vec![]).unwrap().len(), 1);
        assert_eq!(r.push(2, 1, vec![]).unwrap().len(), 2, "2 releases the held 3");
        assert!(r.push(1, 1, vec![]).is_none(), "already delivered");
        assert_eq!(r.held_bytes(), 0);
    }

    #[test]
    fn a_lost_channel_is_redialed_with_backoff_and_bounded_tries() {
        let t0 = Instant::now();
        let mut r = Redial::default();
        r.up(t0);
        r.lost(t0 + Duration::from_secs(5));
        assert!(r.pending() && !r.due(t0 + Duration::from_secs(5)));
        assert!(r.due(t0 + Duration::from_secs(6)), "first try after a second");
        // Each try that fails waits longer: 2 s, 4 s, ... up to 30 s.
        let mut now = t0 + Duration::from_secs(6);
        let mut waits = Vec::new();
        for _ in 0..MAX_REDIALS {
            r = r.next_try();
            assert!(r.again && !r.pending());
            r.lost(now);
            if !r.pending() {
                break;
            }
            let mut wait = 0;
            while !r.due(now + Duration::from_secs(wait)) {
                wait += 1;
            }
            waits.push(wait);
            now += Duration::from_secs(wait);
        }
        assert_eq!(waits, vec![2, 4, 8, 16, 30, 30, 30]);
        assert!(!r.pending() && !r.may_answer(), "gives up after {MAX_REDIALS} tries");
        // A channel that stayed up long enough starts counting again.
        r.up(now);
        r.lost(now + REDIAL_STABLE);
        assert!(r.due(now + REDIAL_STABLE + REDIAL_FIRST));
    }
}
