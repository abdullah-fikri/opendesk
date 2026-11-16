//! The video backpressure gate: the host's capture loop encodes only while the previous frame has left the
//! socket (relay flush or P2P acknowledgement). Waiting on it blocks instead of polling. Also the numbers the
//! video senders report back to the capture loop ([`VideoLink`]).

use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use crate::domain::perf_stats::Stage;

/// What the host's video senders (the relay socket, the P2P path) measure, for `host-stats` and the
/// `host.perf` line, and the stream's rate they size the P2P window by. Shared like the gate: senders write the
/// stats and read the rate, the capture loop the other way round.
#[derive(Default)]
pub struct VideoLink {
    pub stats: Mutex<LinkStats>,
    target_fps: AtomicU32,
    /// `f32` bits of the congestion level (0..1).
    congestion: AtomicU32,
    /// Video sent over the last second, kilobits per second.
    kbps: AtomicU32,
}

impl VideoLink {
    /// The capture loop's current target frame rate and congestion level (`CongestionMeter`).
    pub fn set_rate(&self, target_fps: u32, congestion: f32) {
        self.target_fps.store(target_fps, Ordering::Relaxed);
        self.congestion.store(congestion.to_bits(), Ordering::Relaxed);
    }

    /// The target frame rate; 0 before the capture loop ran.
    pub fn target_fps(&self) -> u32 {
        self.target_fps.load(Ordering::Relaxed)
    }

    /// The congestion level, 0..1.
    pub fn congestion(&self) -> f32 {
        f32::from_bits(self.congestion.load(Ordering::Relaxed))
    }

    /// The video bitrate of the last second (the P2P path paces fragments by it).
    pub fn set_kbps(&self, kbps: u64) {
        self.kbps.store(kbps.min(u64::from(u32::MAX)) as u32, Ordering::Relaxed);
    }

    /// The video bitrate of the last second, kilobits per second; 0 before the first second.
    pub fn kbps(&self) -> u64 {
        u64::from(self.kbps.load(Ordering::Relaxed))
    }
}

/// See [`VideoLink`].
#[derive(Default)]
pub struct LinkStats {
    /// From the sender taking a frame until its last byte is in the socket (relay: flushed; P2P: every
    /// fragment sent, including pacing).
    pub send: Stage,
    /// P2P: from sending a frame until the viewer acknowledged it complete.
    pub ack_rtt: Stage,
    /// P2P: frames that may be in flight; None while video takes the relay.
    pub window: Option<usize>,
    /// P2P: video fragments resent on the viewer's request (`video-nack`) since the last perf line.
    pub resent: u64,
}

/// An open/closed flag a thread can wait on until a deadline.
pub struct SendGate {
    flag: AtomicBool,
    cv: Condvar,
    m: Mutex<()>,
}

impl SendGate {
    /// A gate that starts open or closed.
    pub fn new(open: bool) -> Self {
        Self { flag: AtomicBool::new(open), cv: Condvar::new(), m: Mutex::new(()) }
    }

    /// Opens the gate and wakes every waiter.
    pub fn open(&self) {
        self.flag.store(true, Ordering::Release);
        // Taking the lock orders this wake-up after a waiter's check, so it cannot be missed.
        let _guard = self.m.lock();
        self.cv.notify_all();
    }

    /// Closes the gate (a frame is on its way).
    pub fn close(&self) {
        self.flag.store(false, Ordering::Release);
    }

    /// True while a frame may be sent.
    pub fn is_open(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Waits until the gate opens or `deadline` passes; returns whether it is open.
    pub fn wait_open_until(&self, deadline: Instant) -> bool {
        let mut guard = self.m.lock();
        while !self.is_open() {
            if Instant::now() >= deadline {
                return false;
            }
            self.cv.wait_until(&mut guard, deadline);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn open_and_close() {
        let gate = SendGate::new(true);
        assert!(gate.is_open() && gate.wait_open_until(Instant::now()));
        gate.close();
        assert!(!gate.is_open());
        gate.open();
        assert!(gate.is_open());
    }

    #[test]
    fn video_link_carries_the_stream_rate() {
        let link = VideoLink::default();
        assert_eq!((link.target_fps(), link.congestion()), (0, 0.0));
        link.set_rate(144, 0.25);
        assert_eq!((link.target_fps(), link.congestion()), (144, 0.25));
        link.set_kbps(8_000);
        assert_eq!(link.kbps(), 8_000);
    }

    #[test]
    fn closed_gate_times_out() {
        let gate = SendGate::new(false);
        let start = Instant::now();
        assert!(!gate.wait_open_until(start + Duration::from_millis(20)));
        assert!(start.elapsed() >= Duration::from_millis(20));
        assert!(!gate.wait_open_until(start), "a past deadline returns at once");
    }

    #[test]
    fn opening_wakes_a_waiter_early() {
        let gate = Arc::new(SendGate::new(false));
        let opener = Arc::clone(&gate);
        let start = Instant::now();
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            opener.open();
        });
        assert!(gate.wait_open_until(start + Duration::from_secs(5)));
        assert!(start.elapsed() < Duration::from_secs(2));
        t.join().unwrap();
    }
}
