//! Spacing of `p2p-signal` messages. The relay forwards at most one signal per sender and target every 100 ms
//! and silently drops the rest, so back-to-back signals to one partner (the end-to-end handshake replying at
//! once on a fast link, or a P2P candidate right after a handshake message) wait their turn here instead.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// The relay's `minSignalInterval` plus a margin for timer and scheduling jitter.
pub const SIGNAL_SPACING: Duration = Duration::from_millis(110);

/// Signals waiting for their target's spacing; each target keeps its own order.
#[derive(Default)]
pub struct SignalPacer {
    queue: VecDeque<(String, String)>,
    last: HashMap<String, Instant>,
}

impl SignalPacer {
    /// Queues `text` (a serialized `p2p-signal`) for `target`.
    pub fn push(&mut self, target: String, text: String) {
        self.queue.push_back((target, text));
    }

    /// The signals that may go out at `now`: at most one per target, the oldest queued for it.
    pub fn due(&mut self, now: Instant) -> Vec<String> {
        let mut out = Vec::new();
        let mut blocked: Vec<String> = Vec::new();
        let mut i = 0;
        while i < self.queue.len() {
            let target = &self.queue[i].0;
            let free = !blocked.contains(target) && self.last.get(target).is_none_or(|t| now - *t >= SIGNAL_SPACING);
            if free {
                let (target, text) = self.queue.remove(i).expect("index in range");
                self.last.insert(target.clone(), now);
                blocked.push(target);
                out.push(text);
            } else {
                blocked.push(target.clone());
                i += 1;
            }
        }
        if self.last.len() > 64 {
            self.last.retain(|_, t| now - *t < SIGNAL_SPACING);
        }
        out
    }

    /// True while signals are waiting (the connection loop then keeps its short tick).
    pub fn pending(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Drops everything (the connection ended).
    pub fn clear(&mut self) {
        self.queue.clear();
        self.last.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_signal_per_target_per_spacing_in_order() {
        let mut p = SignalPacer::default();
        let t0 = Instant::now();
        p.push("a".into(), "a1".into());
        p.push("a".into(), "a2".into());
        p.push("b".into(), "b1".into());
        p.push("a".into(), "a3".into());
        assert_eq!(p.due(t0), ["a1", "b1"]);
        assert!(p.due(t0 + Duration::from_millis(50)).is_empty());
        assert_eq!(p.due(t0 + SIGNAL_SPACING), ["a2"]);
        assert!(p.pending());
        assert_eq!(p.due(t0 + SIGNAL_SPACING * 2), ["a3"]);
        assert!(!p.pending());
        // A signal after a quiet spell goes out at once.
        p.push("b".into(), "b2".into());
        assert_eq!(p.due(t0 + SIGNAL_SPACING * 3), ["b2"]);
    }

    #[test]
    fn clear_forgets_queue_and_timing() {
        let mut p = SignalPacer::default();
        let t0 = Instant::now();
        p.push("a".into(), "a1".into());
        assert_eq!(p.due(t0), ["a1"]);
        p.push("a".into(), "a2".into());
        p.clear();
        assert!(!p.pending());
        p.push("a".into(), "a3".into());
        assert_eq!(p.due(t0), ["a3"]);
    }
}
