//! Outbound: drains the queue onto the socket or a P2P path (input, video, audio, monitor and file messages).

use super::*;

impl Connection {
    /// Sends everything queued: control messages and voice at once, mouse moves coalesced per host, only the
    /// newest video frame. File messages are parked for `pump_files`. Returns true if video went to the socket.
    pub(super) fn pump_outbox(&mut self, socket: &mut Socket, partner: &str) -> bool {
        let mut video: Option<Vec<u8>> = None;
        let mut mouse = PendingMoves::default();
        while let Some(item) = self.held.pop_front().or_else(|| self.outbox.try_recv().ok()) {
            self.busy = true;
            match item {
                Outgoing::Message(ClientMessage::RelayFile { target_clean, payload })
                    if payload.action == FileAction::Ack && self.ack_over_p2p(&target_clean, &payload) => {}
                Outgoing::Message(msg) => {
                    let text = match &msg {
                        ClientMessage::RelayInput { target_clean, event } => {
                            // A move queued before a button event must reach the host first: sent after it, the
                            // pointer would jump back while the button is held and cut the drag's ends.
                            if let Some((x, y)) = mouse.take(target_clean) {
                                self.send_move(socket, target_clean, x, y);
                            }
                            if self.direct(target_clean).is_some_and(|p| p.send_input(event)) || self.quiet {
                                continue;
                            }
                            self.input_text(target_clean, event)
                        }
                        ClientMessage::P2pSignal { target_clean, .. } => {
                            self.signals.push(target_clean.clone(), msg.to_json());
                            continue;
                        }
                        _ => msg.to_json(),
                    };
                    let _ = socket.send(Message::Text(text.into()));
                }
                Outgoing::MouseMove(target, x, y) => mouse.set(target, x, y),
                Outgoing::VideoFrame(frame) => video = Some(frame),
                Outgoing::MonitorFrame(frame) => self.monitor_pending = Some(frame),
                Outgoing::Audio(packet) => self.send_audio(socket, partner, packet),
                Outgoing::File { message: ClientMessage::RelayFile { target_clean, payload }, status, mark }
                    if self.route_file(&target_clean, &payload) =>
                {
                    let json = serde_json::to_vec(&payload).unwrap_or_default();
                    self.send_file_p2p(&target_clean, bulk::MSG_JSON, &json, chunk_units(0), status, mark);
                }
                Outgoing::FileChunk { target, frame, status, mark }
                    if binary::parse_file_chunk(&frame).is_some_and(|c| self.routed_p2p(&target, c.id)) =>
                {
                    let units = chunk_units(frame.len());
                    self.send_file_p2p(&target, bulk::MSG_BINARY, &frame, units, status, mark);
                }
                Outgoing::File { message, status, mark } => {
                    let text = message.to_json();
                    let bytes = text.len();
                    self.files.push_back(QueuedFile {
                        message: Message::Text(text.into()),
                        units: 1,
                        bytes,
                        status,
                        mark,
                    });
                }
                Outgoing::FileChunk { target, frame, status, mark } => {
                    let units = chunk_units(frame.len());
                    // Sealed here, in queue order, so the receiver sees its nonces ascending.
                    let frame = self.seal(&target, frame);
                    // Like voice: a viewer of several hosts names the host; the relay's grant decides anyway.
                    let frame = if self.ctx.viewers.multi() && self.ctx.viewers.get(&target).is_some() {
                        binary::tag(&target, &frame).unwrap_or(frame)
                    } else {
                        frame
                    };
                    let bytes = frame.len();
                    self.files.push_back(QueuedFile {
                        message: Message::Binary(frame.into()),
                        units,
                        bytes,
                        status,
                        mark,
                    });
                }
            }
        }
        for text in self.signals.due(Instant::now()) {
            let _ = socket.send(Message::Text(text.into()));
        }
        for (target, norm_x, norm_y) in mouse.0 {
            self.send_move(socket, &target, norm_x, norm_y);
        }
        let Some(frame) = video else { return false };
        // Over P2P the viewer's frame acknowledgement opens the gate instead of the socket flush.
        if self.direct(partner).is_some_and(|p| p.send_video(&frame)) || self.quiet {
            return false;
        }
        let _ = socket.send(Message::Binary(self.seal(partner, frame).into()));
        true
    }

    /// Sends a mouse move to host `target` (P2P when up, else the relay unless offline); an empty target is dropped.
    pub(super) fn send_move(&self, socket: &mut Socket, target: &str, norm_x: f64, norm_y: f64) {
        if target.is_empty() {
            return;
        }
        let event = InputEvent::MouseMove { norm_x, norm_y };
        if !self.direct(target).is_some_and(|p| p.send_input(&event)) && !self.quiet {
            let _ = socket.send(Message::Text(self.input_text(target, &event).into()));
        }
    }

    /// Seals a binary message for `partner` on the relay path when the session is end-to-end encrypted.
    pub(super) fn seal(&self, partner: &str, msg: Vec<u8>) -> Vec<u8> {
        self.e2e_for(partner).and_then(|e| e.seal(&msg)).unwrap_or(msg)
    }

    /// Opens a binary message from `partner`; None drops it (see `E2e::open`).
    pub(super) fn open<'a>(&self, partner: &str, msg: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        match self.e2e_for(partner) {
            Some(e) => e.open(msg),
            None => Some(Cow::Borrowed(msg)),
        }
    }

    /// `relay-input` text for `target`; the event is sealed when the session is end-to-end encrypted. The
    /// relay only needs `targetClean` to authorize it.
    pub(super) fn input_text(&self, target: &str, event: &InputEvent) -> String {
        let sealed = self.e2e_for(target).and_then(|e| serde_json::to_value(event).ok().and_then(|v| e.seal_event(&v)));
        match sealed {
            Some(event) => {
                serde_json::json!({ "type": "relay-input", "targetClean": target, "event": event }).to_string()
            }
            None => ClientMessage::input(target, event.clone()).to_json(),
        }
    }

    /// Sends the newest monitor-wall thumbnail once the previous one has left the socket, so a slow link holds
    /// at most one thumbnail here and older ones are dropped, never queued.
    pub(super) fn pump_monitor(&mut self, socket: &mut Socket) {
        if self.monitor_in_flight {
            return;
        }
        if let Some(frame) = self.monitor_pending.take() {
            let _ = socket.send(Message::Binary(frame.into()));
            self.monitor_in_flight = true;
            self.busy = true;
        }
    }

    /// Voice goes to the partner; a viewer of several hosts talks to the focused one only (tagged).
    pub(super) fn send_audio(&self, socket: &mut Socket, partner: &str, packet: Vec<u8>) {
        if partner.is_empty() || self.direct(partner).is_some_and(|p| p.send_audio(&packet)) || self.quiet {
            return;
        }
        let packet = self.seal(partner, packet);
        let out = if self.ctx.viewers.multi() && self.ctx.viewers.get(partner).is_some() {
            binary::tag(partner, &packet)
        } else {
            Some(packet)
        };
        if let Some(out) = out {
            let _ = socket.send(Message::Binary(out.into()));
        }
    }

    /// Sends a burst of queued file messages, within the per-tick budget and the paced file rate, and returns
    /// the newest status line among them.
    pub(super) fn pump_files(&mut self, socket: &mut Socket, idle: bool) -> Option<String> {
        let mut budget = if idle { FILE_BURST_IDLE } else { FILE_BURST_IN_SESSION };
        let mut latest = None;
        let now = Instant::now();
        while budget > 0 {
            if !self.files.front().is_some_and(|f| self.pacer.allow(f.bytes, now)) {
                break;
            }
            let Some(file) = self.files.pop_front() else { break };
            // WouldBlock means tungstenite buffered the frame; any other error leaves it queued until the read
            // loop notices the dead socket and fails the transfer. Messages are reference counted: the clone
            // copies no data.
            match socket.send(file.message.clone()) {
                Ok(()) => {}
                Err(tungstenite::Error::Io(ref e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => {
                    self.files.push_front(file);
                    break;
                }
            }
            IN_FLIGHT_CHUNKS.fetch_sub(file.units, Ordering::Relaxed);
            self.pacer.on_written(file.bytes, file.mark);
            budget = budget.saturating_sub(file.units);
            latest = file.status.or(latest);
        }
        latest
    }
}
