//! The per-session video inbox: the network thread pushes, the decoder thread drains (a key frame drops what it replaces).

use super::*;

/// Received video messages in arrival order, each with its arrival time (the latency rows' "received").
/// Deltas must all be applied, so nothing is overwritten; a full frame makes everything queued before it obsolete.
#[derive(Default)]
pub struct VideoInbox {
    pub(super) queue: Mutex<VecDeque<(Vec<u8>, Instant)>>,
    /// Signalled on every push and by [`VideoInbox::wake`], so the decoder thread sleeps while idle.
    pub(super) ready: Condvar,
}

impl VideoInbox {
    /// Bound on queued messages; beyond it the oldest are dropped and the sequence gap requests a key frame.
    const MAX: usize = 32;

    /// Longest idle wait of the decoder thread before it re-checks the session state on its own.
    pub const IDLE_WAIT: Duration = Duration::from_millis(50);

    /// Queues one video message; a full frame drops everything queued before it.
    pub fn push(&self, msg: Vec<u8>) {
        let full = !is_framed(&msg) || FrameHeader::parse(&msg).is_some_and(|(h, _)| h.kind == KIND_KEY);
        let mut q = self.queue.lock();
        if full {
            q.clear();
        }
        if q.len() >= Self::MAX {
            q.pop_front();
        }
        q.push_back((msg, Instant::now()));
        self.ready.notify_all();
    }

    /// Takes every queued message in arrival order.
    #[cfg(test)]
    pub fn drain(&self) -> Vec<Vec<u8>> {
        self.queue.lock().drain(..).map(|(m, _)| m).collect()
    }

    /// Like [`VideoInbox::drain`], but waits up to `timeout` for a message when the queue is empty. Returns
    /// an empty batch on timeout or [`VideoInbox::wake`].
    pub fn drain_wait(&self, timeout: Duration) -> Batch {
        let mut q = self.queue.lock();
        if q.is_empty() {
            // A single wait, not a loop: a spurious wake-up only costs the caller one extra state check.
            let _ = self.ready.wait_for(&mut q, timeout);
        }
        let received = q.back().map(|m| m.1);
        Batch { msgs: q.drain(..).map(|(m, _)| m).collect(), received }
    }

    /// Wakes a waiting decoder thread so it re-checks the session's `alive`/`visible` state at once.
    pub fn wake(&self) {
        let _guard = self.queue.lock();
        self.ready.notify_all();
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.queue.lock().is_empty()
    }
}

/// Messages taken from the inbox in one go.
pub struct Batch {
    pub msgs: Vec<Vec<u8>>,
    /// Arrival of the newest message: the one the decoded picture shows.
    pub received: Option<Instant>,
}
