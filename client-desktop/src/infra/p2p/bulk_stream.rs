//! File streams over the path (`file-p2p`): reliable, flow-controlled bulk segments.

use super::*;

impl P2p {
    /// This side's candidates as gathered for the path (LAN, IPv6, STUN-mapped and port-mapped addresses); the
    /// file channel offers their public addresses with its own port.
    pub fn local_candidates(&self) -> Vec<SocketAddr> {
        self.state.lock().local_cands.clone()
    }

    /// Queues one file stream message (`file-p2p`); `done` runs once the partner acknowledged it, or with false
    /// if the path dropped first. False when the path is down (`done` then ran with false already).
    pub fn send_bulk(&self, kind: u8, payload: &[u8], done: Option<OnDelivered>) -> bool {
        let mut st = self.state.lock();
        if self.closed.load(Ordering::SeqCst) || Self::route_of(&st).is_none() {
            if let Some(done) = done {
                done(false);
            }
            return false;
        }
        st.bulk_tx.push(kind, payload, done);
        self.bulk_wake.notify_all();
        true
    }

    /// The connection processed `bytes` of received file stream payload: the advertised window grows back.
    // `fetch_update` is deprecated (renamed `try_update`) on newer toolchains; kept for older ones.
    #[allow(deprecated)]
    pub fn bulk_consumed(&self, bytes: usize) {
        let _ = self.bulk_pending.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |p| Some(p.saturating_sub(bytes)));
    }

    /// Ends both directions of the file stream after the path dropped: unacknowledged messages fail and the
    /// connection learns that what it was receiving is cut off.
    pub(super) fn drop_bulk(&self, st: &mut State) {
        let sending = !st.bulk_tx.is_idle();
        st.bulk_tx.abort();
        st.bulk_tx = BulkSender::new(BULK_SEG);
        st.bulk_rx = BulkReceiver::default();
        if sending {
            logger::warn("anynet.p2p", "P2P path lost during a file transfer; the transfer is cancelled.");
        }
        if let Some(sink) = &self.sinks.bulk {
            sink(BULK_LOST, Vec::new());
        }
    }

    /// File stream sender: segments leave as the window allows, paced at the window's rate so a burst never
    /// queues ahead of video; overdue ones are resent.
    pub(super) fn run_bulk(&self) {
        let mut st = self.state.lock();
        while !self.closed.load(Ordering::SeqCst) {
            if st.bulk_tx.is_idle() || Self::route_of(&st).is_none() {
                self.bulk_wake.wait_for(&mut st, Duration::from_millis(100));
                continue;
            }
            let now = Instant::now();
            st.bulk_tx.on_timer(now);
            if st.bulk_due > now {
                let wait = st.bulk_due - now;
                MutexGuard::unlocked(&mut st, || std::thread::sleep(wait));
                continue;
            }
            // Up to 1 ms of the window's rate per wake-up; one segment at a time before a round trip is known.
            let burst = st.bulk_tx.rate().map_or(1, |r| ((r * 0.001) / BULK_SEG as f64).ceil().max(1.0) as usize);
            let mut sent = 0;
            while sent < burst {
                let Some((seq, flags, data)) = st.bulk_tx.next(now) else { break };
                let mut pkt = Vec::with_capacity(6 + data.len());
                pkt.push(K_BULK);
                pkt.extend_from_slice(&seq.to_be_bytes());
                pkt.push(flags);
                pkt.extend_from_slice(&data);
                self.send_to_peer(&mut st, &pkt);
                sent += 1;
            }
            if sent == 0 {
                // Window full: an acknowledgement wakes this thread; the timeout catches losses.
                self.bulk_wake.wait_for(&mut st, Duration::from_millis(5));
                continue;
            }
            let rate = st.bulk_tx.rate().unwrap_or(f64::MAX);
            let spacing =
                Duration::from_secs_f64((sent * BULK_SEG) as f64 / (rate * 1.25)).min(Duration::from_millis(20));
            st.bulk_due = now + spacing;
        }
    }
}
