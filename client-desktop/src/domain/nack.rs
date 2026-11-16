//! Video fragment reassembly with retransmit requests (`video-nack`, the viewer side of B12).
//!
//! Without it one lost UDP fragment drops its whole frame, and for H.264 or delta video every later frame until
//! a key frame (a round trip plus a large IDR, which is itself likely to lose a fragment). With it the viewer asks
//! the host for the missing fragments (the host keeps its last few frames) and holds at most one newer frame back
//! meanwhile, so frames still reach the decoder in order. It gives up after [`GIVE_UP`] and behaves as before.

use std::time::{Duration, Instant};

/// Requests for one frame: the first at once, then again after [`RENACK_AFTER`].
const MAX_NACKS: u8 = 3;
/// Time before asking again for fragments that are still missing.
pub const RENACK_AFTER: Duration = Duration::from_millis(30);
/// An incomplete frame is dropped once it is this old and a newer one is waiting.
pub const GIVE_UP: Duration = Duration::from_millis(120);
/// Most fragment indices one request names (larger losses are better served by a key frame).
pub const MAX_NACK_INDICES: usize = 256;

struct Partial {
    id: u32,
    parts: Vec<Option<Vec<u8>>>,
    got: usize,
    /// One past the highest index received: anything missing below it was lost (fragments leave in order).
    high: usize,
    started: Instant,
    nacks: u8,
    last_nack: Option<Instant>,
}

impl Partial {
    fn new(id: u32, count: usize, now: Instant) -> Self {
        Self { id, parts: vec![None; count], got: 0, high: 0, started: now, nacks: 0, last_nack: None }
    }

    fn add(&mut self, idx: usize, data: &[u8]) {
        if self.parts[idx].is_none() {
            self.parts[idx] = Some(data.to_vec());
            self.got += 1;
        }
        self.high = self.high.max(idx + 1);
    }

    fn complete(&self) -> bool {
        self.got == self.parts.len()
    }

    fn assemble(self) -> Vec<u8> {
        self.parts.into_iter().flatten().flatten().collect()
    }

    /// Missing indices below `upto`.
    fn missing(&self, upto: usize) -> Vec<u16> {
        (0..upto.min(self.parts.len()))
            .filter(|&i| self.parts[i].is_none())
            .take(MAX_NACK_INDICES)
            .map(|i| i as u16)
            .collect()
    }

    /// A request for what is missing below `upto`, if one is due.
    fn nack(&mut self, upto: usize, now: Instant) -> Option<(u32, Vec<u16>)> {
        let due = self.nacks < MAX_NACKS && self.last_nack.is_none_or(|t| now.duration_since(t) >= RENACK_AFTER);
        let missing = if due { self.missing(upto) } else { Vec::new() };
        if missing.is_empty() {
            return None;
        }
        self.nacks += 1;
        self.last_nack = Some(now);
        Some((self.id, missing))
    }
}

/// What a fragment or a tick produced: frames to decode, in order, and retransmit requests to send.
#[derive(Debug, Default, PartialEq)]
pub struct Pushed {
    pub frames: Vec<Vec<u8>>,
    /// ID of the last frame in `frames` (what the frame acknowledgement names).
    pub last_id: Option<u32>,
    pub nacks: Vec<(u32, Vec<u16>)>,
}

/// Frames held at most: the oldest incomplete one and the newer ones waiting behind it.
const MAX_HELD: usize = 4;

/// A fragment this many frames (or fewer) behind the last frame that left is a late copy of it; further back,
/// the host restarted its frame numbering.
const LATE_WINDOW: u32 = 1024;

/// Incomplete frames in id order; complete ones leave as soon as everything before them did.
#[derive(Default)]
pub struct Reassembly {
    held: std::collections::VecDeque<Partial>,
    /// The newest frame that left (decoded or given up). Retransmitted copies of fragments that were only late,
    /// not lost, arrive after their frame completed; without this they would rebuild and decode it again.
    left: Option<u32>,
}

fn newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

impl Reassembly {
    /// Adds fragment `idx` of `count` of frame `id`. `restarts` tells whether a complete frame restarts the
    /// decoder chain (key frame, IDR): then nothing older needs to wait for it.
    pub fn push(
        &mut self,
        now: Instant,
        id: u32,
        idx: usize,
        count: usize,
        data: &[u8],
        restarts: impl Fn(&[u8]) -> bool,
    ) -> Pushed {
        let mut out = Pushed::default();
        if count == 0 || idx >= count {
            return out;
        }
        if self.held.front().is_some_and(|f| newer(f.id, id)) {
            return out; // stale (or a retransmit for a frame we gave up on)
        }
        match self.left {
            Some(left) if !newer(id, left) && left.wrapping_sub(id) <= LATE_WINDOW => return out,
            Some(_) if !newer(id, self.left.unwrap_or(id)) => self.left = None, // numbering restarted
            _ => {}
        }
        let at = match self.held.iter().position(|f| !newer(id, f.id)) {
            Some(i) if self.held[i].id == id => i,
            Some(i) => {
                // First fragment of a frame between two held ones (reordered).
                self.held.insert(i, Partial::new(id, count, now));
                i
            }
            None => {
                self.held.push_back(Partial::new(id, count, now));
                if self.held.len() > MAX_HELD {
                    let dropped = self.held.pop_front().map(|f| f.id);
                    self.mark_left(dropped);
                }
                self.held.len() - 1
            }
        };
        let part = &mut self.held[at];
        if part.parts.len() != count {
            return out;
        }
        part.add(idx, data);
        // Fragments leave in order: a gap below the highest index is a loss, and so is the missing rest of any
        // frame that a newer one already follows.
        let last = self.held.len() - 1;
        for (i, f) in self.held.iter_mut().enumerate() {
            let upto = if i < last { f.parts.len() } else { f.high };
            out.nacks.extend(f.nack(upto, now));
        }
        self.drain(now, &mut out, &restarts);
        out
    }

    /// Asks again for what is still missing (also the lost tail of the newest frame, which nothing after it
    /// reveals), and gives up on frames that waited too long.
    pub fn tick(&mut self, now: Instant, restarts: impl Fn(&[u8]) -> bool) -> Pushed {
        let mut out = Pushed::default();
        for f in self.held.iter_mut() {
            if now.duration_since(f.started) >= RENACK_AFTER {
                out.nacks.extend(f.nack(f.parts.len(), now));
            }
        }
        self.drain(now, &mut out, &restarts);
        out
    }

    /// Delivers complete frames in order. Drops what precedes a complete frame that restarts the chain, and the
    /// oldest frame once it waited [`GIVE_UP`] with newer ones behind it.
    fn drain(&mut self, now: Instant, out: &mut Pushed, restarts: &impl Fn(&[u8]) -> bool) {
        if let Some(k) = self.held.iter().rposition(|f| f.complete() && restarts(&first_fragment(f))) {
            self.held.drain(..k);
        }
        loop {
            match self.held.front() {
                Some(f) if f.complete() => {
                    let f = self.held.pop_front().unwrap();
                    self.mark_left(Some(f.id));
                    out.last_id = Some(f.id);
                    out.frames.push(f.assemble());
                }
                Some(f) if self.held.len() > 1 && now.duration_since(f.started) >= GIVE_UP => {
                    let dropped = self.held.pop_front().map(|f| f.id);
                    self.mark_left(dropped);
                }
                _ => return,
            }
        }
    }

    fn mark_left(&mut self, id: Option<u32>) {
        if let Some(id) = id {
            self.left = Some(self.left.filter(|&l| newer(l, id)).unwrap_or(id));
        }
    }
}

fn first_fragment(p: &Partial) -> Vec<u8> {
    // Only the first fragment matters for the frame type.
    p.parts.first().cloned().flatten().unwrap_or_default()
}

/// A retransmit request on the wire: `[frame id u32][index u16]...` (big-endian).
pub fn encode_nack(id: u32, missing: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + missing.len() * 2);
    out.extend_from_slice(&id.to_be_bytes());
    for i in missing.iter().take(MAX_NACK_INDICES) {
        out.extend_from_slice(&i.to_be_bytes());
    }
    out
}

/// Parses a retransmit request; None when malformed or too long.
pub fn decode_nack(body: &[u8]) -> Option<(u32, Vec<u16>)> {
    if body.len() < 6 || !body.len().is_multiple_of(2) || (body.len() - 4) / 2 > MAX_NACK_INDICES {
        return None;
    }
    let id = u32::from_be_bytes(body[..4].try_into().ok()?);
    Some((id, body[4..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never(_: &[u8]) -> bool {
        false
    }

    #[test]
    fn complete_frames_pass_straight_through() {
        let (mut r, t) = (Reassembly::default(), Instant::now());
        assert_eq!(r.push(t, 1, 0, 2, b"ab", never), Pushed::default());
        assert_eq!(r.push(t, 1, 1, 2, b"cd", never).frames, vec![b"abcd".to_vec()]);
        assert_eq!(r.push(t, 2, 0, 1, b"x", never).frames, vec![b"x".to_vec()]);
    }

    #[test]
    fn a_gap_is_requested_and_the_retransmit_completes_the_frame_in_order() {
        let (mut r, t) = (Reassembly::default(), Instant::now());
        r.push(t, 7, 0, 3, b"a", never);
        // Fragment 1 lost: fragment 2 shows the gap.
        assert_eq!(r.push(t, 7, 2, 3, b"c", never).nacks, vec![(7, vec![1])]);
        // The next frame arrives complete but waits behind frame 7.
        let held = r.push(t, 8, 0, 1, b"N", never);
        assert!(held.frames.is_empty());
        assert_eq!(held.nacks, vec![], "asked a moment ago");
        // Retransmit: both frames come out, oldest first.
        assert_eq!(r.push(t, 7, 1, 3, b"b", never).frames, vec![b"abc".to_vec(), b"N".to_vec()]);
    }

    #[test]
    fn a_lost_tail_is_requested_from_the_tick() {
        let (mut r, t) = (Reassembly::default(), Instant::now());
        assert_eq!(r.push(t, 1, 0, 3, b"a", never).nacks, vec![], "nothing missing below the highest index yet");
        assert_eq!(r.tick(t + Duration::from_millis(5), never).nacks, vec![], "too early");
        assert_eq!(r.tick(t + RENACK_AFTER, never).nacks, vec![(1, vec![1, 2])]);
        assert_eq!(r.tick(t + RENACK_AFTER + Duration::from_millis(5), never).nacks, vec![], "not again so soon");
        assert_eq!(r.tick(t + RENACK_AFTER * 2, never).nacks, vec![(1, vec![1, 2])]);
    }

    #[test]
    fn requests_are_capped_and_a_stuck_frame_is_dropped() {
        let (mut r, t) = (Reassembly::default(), Instant::now());
        r.push(t, 1, 1, 2, b"b", never);
        r.push(t, 2, 0, 1, b"N", never);
        let mut asked = 0;
        for i in 1..20 {
            asked += r.tick(t + RENACK_AFTER * i, never).nacks.iter().filter(|(id, _)| *id == 1).count() as u32;
        }
        assert!(asked < MAX_NACKS as u32, "{asked} more requests");
        // GIVE_UP passed long ago: frame 2 was released by a tick.
        let late = r.push(t + GIVE_UP * 5, 1, 0, 2, b"a", never);
        assert!(late.frames.is_empty(), "frame 1 already given up");
    }

    #[test]
    fn a_waiting_frame_is_given_up_after_the_deadline() {
        let (mut r, t) = (Reassembly::default(), Instant::now());
        r.push(t, 1, 0, 2, b"a", never);
        assert!(r.push(t, 2, 0, 1, b"N", never).frames.is_empty());
        assert_eq!(r.tick(t + GIVE_UP, never).frames, vec![b"N".to_vec()]);
    }

    #[test]
    fn a_key_frame_releases_the_wait_at_once() {
        let (mut r, t) = (Reassembly::default(), Instant::now());
        r.push(t, 1, 0, 2, b"a", never);
        let key = |f: &[u8]| f == b"K";
        assert_eq!(r.push(t, 2, 0, 1, b"K", key).frames, vec![b"K".to_vec()]);
    }

    #[test]
    fn several_frames_wait_behind_a_lost_one_and_come_out_in_order() {
        let (mut r, t) = (Reassembly::default(), Instant::now());
        r.push(t, 1, 0, 2, b"a", never);
        for id in 2..=3 {
            assert!(r.push(t, id, 0, 1, b"n", never).frames.is_empty(), "frame {id} waits for frame 1");
        }
        let out = r.push(t, 1, 1, 2, b"A", never);
        assert_eq!(out.frames, vec![b"aA".to_vec(), b"n".to_vec(), b"n".to_vec()]);
        assert_eq!(out.last_id, Some(3));
    }

    #[test]
    fn the_queue_is_bounded_and_stale_fragments_are_ignored() {
        let (mut r, t) = (Reassembly::default(), Instant::now());
        r.push(t, 1, 0, 2, b"a", never);
        for id in 2..=4 {
            r.push(t, id, 0, 2, b"b", never);
        }
        r.push(t, 5, 0, 1, b"c", never); // fifth frame: frame 1 is dropped
        assert!(r.push(t, 1, 1, 2, b"x", never).frames.is_empty(), "frame 1 is gone");
        let out = r.push(t, 2, 1, 2, b"B", never);
        assert_eq!(out.frames, vec![b"bB".to_vec()], "frames 3 and 4 still incomplete");
    }

    /// Fragments that were only late got requested and resent; the copies arriving after their frame was
    /// decoded must not rebuild and decode it a second time.
    #[test]
    fn late_copies_of_a_decoded_frame_are_ignored() {
        let (mut r, t) = (Reassembly::default(), Instant::now());
        r.push(t, 6, 0, 2, b"a", never);
        assert_eq!(r.push(t, 6, 1, 2, b"b", never).frames, vec![b"ab".to_vec()]);
        assert!(r.push(t, 6, 0, 2, b"a", never).frames.is_empty());
        assert!(r.push(t, 6, 1, 2, b"b", never).frames.is_empty(), "frame 6 decoded twice");
        assert_eq!(r.push(t, 7, 0, 1, b"c", never).frames, vec![b"c".to_vec()]);
        // A host that restarted its numbering is followed, not ignored.
        assert_eq!(r.push(t, 7 + 5000, 0, 1, b"d", never).frames, vec![b"d".to_vec()]);
        assert_eq!(r.push(t, 1, 0, 1, b"e", never).frames, vec![b"e".to_vec()], "numbering restarted");
    }

    #[test]
    fn nack_wire_format_round_trips_and_rejects_garbage() {
        let wire = encode_nack(0xABCD_0102, &[0, 5, 300]);
        assert_eq!(decode_nack(&wire), Some((0xABCD_0102, vec![0, 5, 300])));
        assert_eq!(decode_nack(&wire[..5]), None);
        assert_eq!(decode_nack(&[0; 4]), None, "no index");
        assert_eq!(decode_nack(&vec![0; 4 + 2 * (MAX_NACK_INDICES + 1)]), None);
    }
}
