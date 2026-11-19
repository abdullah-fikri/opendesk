//! Outgoing media and input: sealing, fragmenting and pacing video, audio, and input with retransmission.

use super::*;

impl P2p {
    pub(super) fn send_plain(&self, st: &mut State, to: SocketAddr, plain: &[u8]) -> bool {
        let nonce = st.tx_nonce;
        st.tx_nonce += 1;
        let Some(Handshake::Done(t)) = &st.hs else { return false };
        match seal(t, nonce, plain) {
            Some(pkt) => self.sock.send_to(&pkt, to).is_ok(),
            None => false,
        }
    }

    pub(super) fn send_to_peer(&self, st: &mut State, plain: &[u8]) -> bool {
        match Self::route_of(st) {
            Some(route) => self.send_plain(st, route.addr(), plain),
            None => false,
        }
    }

    /// Sends one JPEG over P2P. Returns false (caller uses the relay) if the path is down or the frame is too large.
    pub fn send_video(&self, jpeg: &[u8]) -> bool {
        let started = Instant::now();
        let mut st = self.state.lock();
        let count = jpeg.len().div_ceil(FRAG_DATA);
        if !Self::alive(&st) || count == 0 || count > MAX_FRAGS {
            return false;
        }
        // The viewer keeps only the newest frame: the previous frame's paced rest must arrive before this one.
        self.send_paced(&mut st, None);
        st.frame_id = st.frame_id.wrapping_add(1);
        let id = st.frame_id;
        let plan = if self.pacing {
            pacing::plan(count, MAX_DATAGRAM, self.video_link.target_fps(), self.video_link.kbps())
        } else {
            Pacing::burst(count)
        };
        let mut paced = VecDeque::new();
        let mut kept = Vec::with_capacity(count);
        let mut pkt = Vec::with_capacity(MAX_PLAIN);
        for (idx, chunk) in jpeg.chunks(FRAG_DATA).enumerate() {
            pkt.clear();
            pkt.push(K_VIDEO);
            pkt.extend_from_slice(&id.to_be_bytes());
            pkt.extend_from_slice(&(idx as u16).to_be_bytes());
            pkt.extend_from_slice(&(count as u16).to_be_bytes());
            pkt.extend_from_slice(chunk);
            kept.push(pkt.clone());
            if idx < plan.burst {
                self.send_to_peer(&mut st, &pkt);
            } else {
                paced.push_back((started + plan.offset(idx), pkt.clone()));
            }
        }
        // Up to `window` frames in flight; past that the viewer's frame-ack (or a timeout) releases the capture
        // loop. The window follows the stream's rate and the measured round trip, and the capture loop's
        // congestion level shrinks it back to 2.
        if self.dynamic_window {
            let link = &self.video_link;
            st.window = udp_window::frames_in_flight(st.ack_rtt.get(), link.target_fps(), link.congestion());
        }
        // Kept for retransmits; a viewer that does not ask never costs more than these few frames.
        st.sent_frames.push_back((id, kept));
        if st.sent_frames.len() > NACK_HISTORY {
            st.sent_frames.pop_front();
        }
        st.frame_wait.push_back((id, started));
        if st.frame_wait.len() < st.window {
            self.can_send_frame.open();
        }
        let mut link = self.video_link.stats.lock();
        link.window = Some(st.window);
        if paced.is_empty() {
            link.send.record(started.elapsed());
        } else {
            st.paced = Some(Paced { started, pending: paced });
            self.pace_wake.notify_one();
        }
        true
    }

    /// Sends the paced fragments that are due by `until` (all with None); records the frame's send time once
    /// its last fragment left.
    pub(super) fn send_paced(&self, st: &mut State, until: Option<Instant>) {
        let Some(mut p) = st.paced.take() else { return };
        while let Some((at, _)) = p.pending.front() {
            if until.is_some_and(|u| *at > u) {
                break;
            }
            if let Some((_, pkt)) = p.pending.pop_front() {
                self.send_to_peer(st, &pkt);
            }
        }
        if p.pending.is_empty() {
            self.video_link.stats.lock().send.record(p.started.elapsed());
        } else {
            st.paced = Some(p);
        }
    }

    /// The pacer thread (host side, pacing on): sends each paced fragment when it is due. It sleeps with the
    /// state unlocked, so input, audio and acknowledgements are not held up; `std` sleeps on a high-resolution
    /// timer, and fragments that came due meanwhile leave together.
    pub(super) fn pace(&self) {
        let mut st = self.state.lock();
        while !self.closed.load(Ordering::SeqCst) {
            let Some(due) = st.paced.as_ref().and_then(|p| p.pending.front()).map(|f| f.0) else {
                // The timeout notices a closed path.
                self.pace_wake.wait_for(&mut st, Duration::from_millis(100));
                continue;
            };
            let now = Instant::now();
            if due > now {
                MutexGuard::unlocked(&mut st, || std::thread::sleep(due - now));
                continue;
            }
            self.send_paced(&mut st, Some(now));
        }
    }

    /// Sends a `[0x02, rate_khz, pcm...]` packet, split at even byte boundaries to fit one datagram each; an Opus
    /// packet (voice or host sound) goes whole in one datagram.
    pub fn send_audio(&self, pkt: &[u8]) -> bool {
        let mut st = self.state.lock();
        if !Self::alive(&st) {
            return false;
        }
        use crate::infra::audio::opus;
        if opus::parse(pkt).is_some() || opus::parse_host_sound(pkt).is_some() {
            if pkt.len() + 1 > MAX_PLAIN {
                return false;
            }
            let mut plain = Vec::with_capacity(1 + pkt.len());
            plain.push(K_AUDIO);
            plain.extend_from_slice(pkt);
            self.send_to_peer(&mut st, &plain);
            return true;
        }
        if pkt.len() < 4 || !pkt.len().is_multiple_of(2) || pkt[0] != 0x02 {
            return false;
        }
        for chunk in pkt[2..].chunks((MAX_PLAIN - 3) & !1) {
            let mut plain = Vec::with_capacity(3 + chunk.len());
            plain.extend_from_slice(&[K_AUDIO, 0x02, pkt[1]]);
            plain.extend_from_slice(chunk);
            self.send_to_peer(&mut st, &plain);
        }
        true
    }

    /// Queues one input event on the reliable, ordered channel. Returns false if it must go over the relay instead.
    pub fn send_input(&self, event: &InputEvent) -> bool {
        // The input channel only runs viewer -> host (the viewer drops K_INPUT); host events such as
        // cursor-shape or monitors must take the relay.
        if self.is_host {
            return false;
        }
        let Ok(body) = serde_json::to_vec(event) else { return false };
        let mut st = self.state.lock();
        if !Self::alive(&st) || body.len() + 5 > MAX_PLAIN || st.unacked.len() >= MAX_UNACKED {
            return false;
        }
        let seq = st.next_seq;
        st.next_seq += 1;
        st.unacked.push_back((seq, body.clone(), Instant::now()));
        self.send_input_packet(&mut st, seq, &body);
        true
    }

    pub(super) fn send_input_packet(&self, st: &mut State, seq: u32, body: &[u8]) {
        let mut plain = Vec::with_capacity(5 + body.len());
        plain.push(K_INPUT);
        plain.extend_from_slice(&seq.to_be_bytes());
        plain.extend_from_slice(body);
        self.send_to_peer(st, &plain);
    }
}
