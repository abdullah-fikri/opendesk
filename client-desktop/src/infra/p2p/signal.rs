//! The path's setup over the relay: signals, candidate refresh, port mapping and the Noise NN handshake.

use super::*;

impl P2p {
    /// Handles a `p2p-signal` payload from the session partner (the relay already checked the pairing).
    pub fn on_signal(&self, candidate: &str) {
        let mut st = self.state.lock();
        if let Some(cands) = refresh_candidates(candidate) {
            if matches!(st.hs, Some(Handshake::Done(_))) {
                logger::info("anynet.p2p", &format!("Peer refreshed its candidates ({}); probing.", cands.len()));
                st.peer_cands = cands;
                st.probe_started = Some(Instant::now());
            }
            return;
        }
        if st.signal_handled {
            return;
        }
        if st.hs.is_none() {
            st.pending_signal = Some(candidate.to_string());
            return;
        }
        st.signal_handled = true;
        self.handle_signal(&mut st, candidate);
    }

    pub(super) fn handle_signal(&self, st: &mut State, candidate: &str) {
        let Ok(v) = serde_json::from_str::<Value>(candidate) else { return };
        let hs_msg = v.get("hs").and_then(|h| h.as_str()).and_then(from_base64);
        // v1: Noise_NN message and candidates; v2 (with e2e): candidates only.
        let version = if self.e2e.is_some() { 2 } else { 1 };
        if v.get("v").and_then(|x| x.as_u64()) != Some(version)
            || (version == 1 && hs_msg.as_ref().is_none_or(|m| m.len() > 256))
        {
            return;
        }
        st.peer_cands = parse_candidates(&v);
        let list: Vec<String> = st.peer_cands.iter().map(|a| a.to_string()).collect();
        logger::info("anynet.p2p", &format!("Peer offered {} candidates: [{}].", list.len(), list.join(", ")));
        if version == 2 {
            return; // probing starts once the shared handshake is done (`tick`)
        }
        let Some(Handshake::Pending(mut hs)) = st.hs.take() else { return };
        let mut buf = [0u8; 256];
        let msg = hs_msg.unwrap();
        let reply = if self.is_host {
            hs.read_message(&msg, &mut buf).ok().and_then(|_| hs.write_message(&[], &mut buf).ok())
        } else {
            hs.read_message(&msg, &mut buf).ok().map(|_| 0)
        };
        let reply_bytes = reply.map(|n| buf[..n].to_vec());
        st.hs = match (reply_bytes, hs.into_stateless_transport_mode()) {
            (Some(r), Ok(t)) => {
                if self.is_host {
                    self.send_signal(&r, &st.local_cands, false);
                }
                st.probe_started = Some(Instant::now());
                logger::info("anynet.p2p", &format!("Handshake complete; probing {} candidates.", st.peer_cands.len()));
                Some(Handshake::Done(Arc::new(t)))
            }
            _ => {
                logger::warn("anynet.p2p", "Handshake failed; staying on relay.");
                Some(Handshake::Failed)
            }
        };
    }

    pub(super) fn send_signal(&self, hs_msg: &[u8], cands: &[SocketAddr], refresh: bool) {
        let candidate = signal_json(self.e2e.is_some(), hs_msg, cands, refresh);
        let msg = ClientMessage::P2pSignal { target_clean: self.partner.clone(), candidate };
        let _ = self.tx_out.send(Outgoing::Message(msg));
    }

    /// Gathers the candidates again (the network may have changed while the relay connection was down) and
    /// sends them as a refresh. The path is down meanwhile, so the datagrams STUN skips cost nothing.
    pub(super) fn regather_candidates(&self) {
        let cands = gather_candidates(&self.sock);
        let _ = self.sock.set_read_timeout(Some(Duration::from_millis(50)));
        let mut st = self.state.lock();
        if !matches!(st.hs, Some(Handshake::Done(_))) {
            return;
        }
        logger::info("anynet.p2p", &format!("Session resumed: sending {} fresh candidates.", cands.len()));
        self.send_signal(&[], &cands, true);
        st.local_cands = cands;
        st.refreshed = true;
        st.signal_resends = 0;
        st.probe_started = Some(Instant::now());
    }

    /// Asks the router to forward this path's port and offers the public address as one more candidate (a
    /// refresh, once the handshake is done). The mapping is renewed while the path lives and removed after.
    pub(super) fn keep_port_mapping(&self, local: Ipv4Addr) {
        let Some(mut mapping) = port_map::map(self.sock.port(), local) else { return };
        let external = SocketAddr::V4(mapping.external);
        let started = Instant::now();
        let mut offered = false;
        let mut renewed = Instant::now();
        while !self.closed.load(Ordering::SeqCst) {
            if !offered {
                let mut st = self.state.lock();
                if matches!(Self::route_of(&st), Some(Route::Direct(_))) {
                    // Hole punching already worked; the forwarded port is not needed.
                    break;
                }
                if matches!(st.hs, Some(Handshake::Done(_))) {
                    if !st.local_cands.contains(&external) {
                        st.local_cands.insert(0, external);
                        st.local_cands.truncate(MAX_CANDIDATES);
                    }
                    logger::info("anynet.p2p", &format!("Offering the mapped port {external} as a candidate."));
                    self.send_signal(&[], &st.local_cands, true);
                    st.refreshed = true;
                    st.signal_resends = 0;
                    st.probe_started = Some(Instant::now());
                    offered = true;
                } else if started.elapsed() > PORT_MAP_OFFER_WAIT {
                    break; // the handshake never finished: no path to offer it to
                }
            }
            if renewed.elapsed() >= port_map::LEASE / 2 {
                renewed = Instant::now();
                if !mapping.renew() {
                    logger::warn("anynet.p2p", &format!("The router stopped forwarding {external}."));
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        mapping.remove();
    }

    /// Starts this path's own `Noise_NN` handshake (partners without `e2e`). False if it cannot start.
    pub(super) fn start_nn(&self, cands: &[SocketAddr]) -> bool {
        let builder = || snow::Builder::new(NOISE.parse().unwrap()).prologue(&self.prologue);
        let hs = builder().and_then(|b| if self.is_host { b.build_responder() } else { b.build_initiator() });
        let mut st = self.state.lock();
        st.local_cands = cands.to_vec();
        match hs {
            Ok(mut hs) => {
                if !self.is_host {
                    let mut buf = [0u8; 256];
                    match hs.write_message(&[], &mut buf) {
                        Ok(n) => self.send_signal(&buf[..n], cands, false),
                        Err(_) => return false,
                    }
                }
                st.hs = Some(Handshake::Pending(hs));
            }
            Err(_) => return false,
        }
        if let Some(sig) = st.pending_signal.take() {
            st.signal_handled = true;
            self.handle_signal(&mut st, &sig);
        }
        true
    }
}
