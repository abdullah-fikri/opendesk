//! The video backpressure gate: the host's capture loop encodes only while the previous frame has left the
//! socket (relay flush or P2P acknowledgement). Waiting on it blocks instead of polling.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

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
        let _guard = self.m.lock().unwrap();
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
        let mut guard = self.m.lock().unwrap();
        while !self.is_open() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            guard = self.cv.wait_timeout(guard, deadline - now).unwrap().0;
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
