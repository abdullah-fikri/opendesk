//! Incoming datagrams: probes, video fragments, audio, input, acks and file segments.

use super::*;

impl P2p {
    pub(super) fn on_datagram(&self, pkt: &[u8], from: SocketAddr) {
        let mut st = self.state.lock();
        let via_relay = st.relay.as_ref().is_some_and(|r| r.addr == from);
        if via_relay && pkt.first() == Some(&RELAY_TAG) {
            let Some(r) = st.relay.as_mut() else { return };
            if pkt == [RELAY_TAG, RELAY_BOUND] && !r.bound {
                r.bound = true;
                logger::info("anynet.p2p", &format!("Bound to the UDP relay at {from}."));
            } else if pkt.len() == 2 + RELAY_COOKIE && pkt[1] == RELAY_CHALLENGE {
                r.cookie = pkt[2..].try_into().ok();
                r.last_bind = None; // answer on the next tick
            }
            return;
        }
        st.rx_datagrams += 1;
        st.first_from.get_or_insert(from);
        let plain = {
            let State { hs, replay, .. } = &mut *st;
            let opened = match hs {
                Some(Handshake::Done(t)) => open(t, replay, pkt),
                _ => None,
            };
            match opened {
                Some(p) => p,
                None => {
                    // unauthenticated, replayed, malformed or early: never affects state
                    st.rx_rejected += 1;
                    return;
                }
            }
        };
        let Some((&kind, body)) = plain.split_first() else { return };
        if via_relay {
            st.relay_rx = Some(Instant::now());
        } else {
            st.peer_addr = Some(from);
            st.direct_rx = Some(Instant::now());
        }
        match kind {
            K_PROBE => {
                self.send_plain(&mut st, from, &[K_PROBE_ACK]);
            }
            K_VIDEO if !self.is_host && body.len() > 8 => {
                let id = u32::from_be_bytes(body[0..4].try_into().unwrap());
                let idx = u16::from_be_bytes([body[4], body[5]]) as usize;
                let count = u16::from_be_bytes([body[6], body[7]]) as usize;
                if let Some(loss) = &self.sinks.loss
                    && count > 0
                    && count <= MAX_FRAGS
                    && idx < count
                {
                    loss.lock().fragment(id, count);
                }
                if self.sinks.nack {
                    let pushed = st.nack_reasm.push(Instant::now(), id, idx, count, &body[8..], restarts_chain);
                    self.deliver(&mut st, pushed);
                } else if let Some(frame) = Reasm::push(&mut st.reasm, id, idx, count, &body[8..]) {
                    (self.sinks.video)(frame);
                    let mut ack = vec![K_FRAME_ACK];
                    ack.extend_from_slice(&id.to_be_bytes());
                    self.send_to_peer(&mut st, &ack);
                }
            }
            K_NACK if self.is_host => {
                let Some((id, missing)) = nack::decode_nack(body) else { return };
                let frame =
                    st.sent_frames.iter().find(|(f, _)| *f == id).map(|(_, pkts)| {
                        missing.iter().filter_map(|&i| pkts.get(i as usize).cloned()).collect::<Vec<_>>()
                    });
                if let Some(pkts) = frame {
                    self.video_link.stats.lock().resent += pkts.len() as u64;
                    for pkt in pkts {
                        self.send_to_peer(&mut st, &pkt);
                    }
                }
            }
            K_FRAME_ACK if self.is_host && body.len() == 4 => {
                let id = u32::from_be_bytes(body.try_into().unwrap());
                // An acknowledgement also covers every older frame (their acks may have been lost).
                if let Some(&(_, sent)) = st.frame_wait.iter().find(|(w, _)| *w == id) {
                    let rtt = sent.elapsed();
                    self.video_link.stats.lock().ack_rtt.record(rtt);
                    st.ack_rtt.sample(rtt);
                    st.frame_wait.retain(|(w, _)| (w.wrapping_sub(id) as i32) > 0);
                    if st.frame_wait.len() < st.window {
                        self.can_send_frame.open();
                    }
                }
            }
            K_AUDIO => {
                if self.sinks.audio_gate.as_ref().is_none_or(|g| g.load(Ordering::Relaxed)) {
                    self.sinks.voice.play_binary_packet(body);
                }
            }
            K_INPUT if self.is_host && body.len() > 4 => {
                let seq = u32::from_be_bytes(body[0..4].try_into().unwrap());
                if seq >= st.rx_next && seq - st.rx_next < MAX_UNACKED as u32 {
                    st.rx_buf.insert(seq, body[4..].to_vec());
                }
                while let Some(ev) = {
                    let next = st.rx_next;
                    st.rx_buf.remove(&next)
                } {
                    st.rx_next += 1;
                    // Decoded locally, so a peer can only ever produce input events.
                    if let Some(event) = serde_json::from_slice::<Value>(&ev).ok().as_ref().and_then(InputEvent::parse)
                    {
                        let _ = self.sinks.input.send(event);
                    }
                }
                let mut ack = vec![K_INPUT_ACK];
                ack.extend_from_slice(&st.rx_next.to_be_bytes());
                self.send_to_peer(&mut st, &ack);
            }
            K_BULK if body.len() > 5 => {
                let Some(sink) = self.sinks.bulk.as_ref() else { return };
                let seq = u32::from_be_bytes(body[0..4].try_into().unwrap());
                let got = st.bulk_rx.on_segment(seq, body[4], &body[5..]);
                for (kind, payload) in got.messages {
                    self.bulk_pending.fetch_add(payload.len(), Ordering::SeqCst);
                    sink(kind, payload);
                }
                if got.ack {
                    let room = BULK_BUFFER.saturating_sub(self.bulk_pending.load(Ordering::SeqCst)) / BULK_SEG;
                    let mut ack = vec![K_BULK_ACK];
                    ack.extend_from_slice(&st.bulk_rx.ack(room.min(bulk::MAX_REORDER as usize) as u32));
                    self.send_to_peer(&mut st, &ack);
                }
            }
            K_BULK_ACK => {
                if let Some((cum, rwnd, sack)) = bulk::parse_ack(body) {
                    st.bulk_tx.on_ack(Instant::now(), cum, rwnd, sack);
                    self.bulk_wake.notify_all();
                }
            }
            K_INPUT_ACK if !self.is_host && body.len() == 4 => {
                let next = u32::from_be_bytes(body.try_into().unwrap());
                while st.unacked.front().is_some_and(|(s, _, _)| *s < next) {
                    st.unacked.pop_front();
                }
            }
            _ => {} // probe-ack, keepalive, or a kind not meant for this role
        }
    }

    /// Viewer: hands reassembled frames to the decoder in order, acknowledges the newest, and asks for lost
    /// fragments.
    pub(super) fn deliver(&self, st: &mut State, pushed: Pushed) {
        for frame in pushed.frames {
            (self.sinks.video)(frame);
        }
        if let Some(id) = pushed.last_id {
            let mut ack = vec![K_FRAME_ACK];
            ack.extend_from_slice(&id.to_be_bytes());
            self.send_to_peer(st, &ack);
        }
        for (id, missing) in pushed.nacks {
            let mut pkt = vec![K_NACK];
            pkt.extend_from_slice(&nack::encode_nack(id, &missing));
            self.send_to_peer(st, &pkt);
        }
    }
}
