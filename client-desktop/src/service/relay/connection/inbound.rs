//! Inbound: reads every readable message and routes binary frames.

use super::*;

impl Connection {
    /// Drains P2P input and every readable socket message. Returns false when the connection is gone.
    pub(super) fn read_all(&mut self, socket: &mut Socket) -> bool {
        while let Ok(event) = self.p2p_input.try_recv() {
            self.busy = true;
            crate::service::host::input::apply(&self.ctx, event);
        }
        while let Ok((from, kind, payload)) = self.bulk_in.try_recv() {
            self.busy = true;
            self.on_bulk(&from, kind, &payload);
        }
        loop {
            let read = socket.read();
            if read.is_ok() {
                self.last_rx = Instant::now();
            }
            // Keep-alive pings are not traffic: an idle connection stays idle.
            if matches!(read, Ok(Message::Binary(_) | Message::Text(_))) {
                self.busy = true;
            }
            match read {
                Ok(Message::Ping(payload)) => {
                    let _ = socket.send(Message::Pong(payload));
                    let _ = socket.flush();
                }
                Ok(Message::Pong(payload)) => {
                    self.pacer.on_pong(&payload, Instant::now());
                }
                Ok(Message::Binary(bin)) => self.on_binary(&bin),
                Ok(Message::Text(text)) => {
                    if let Some(msg) = ServerMessage::parse(&text) {
                        self.on_message(msg);
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(ref e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    return true;
                }
                Err(e) => {
                    logger::warn("relay", &format!("Socket disconnected: {e}"));
                    return false;
                }
            }
        }
    }

    /// Voice and video from the relay: tagged per host for a multi-session viewer, else the single session.
    /// File chunks are always tagged with their sender.
    pub(super) fn on_binary(&mut self, bin: &[u8]) {
        if let Some((from, payload)) = binary::untag(bin)
            && binary::parse_file_chunk(payload).is_some()
        {
            // A sealed chunk keeps its routing header, so it is recognized before it is opened.
            if let Some(payload) = self.open(from, payload)
                && let Some(chunk) = binary::parse_file_chunk(&payload)
            {
                self.on_file_chunk(from, &chunk);
            }
            return;
        }
        let ctx = &self.ctx;
        if binary::parse_file_chunk(bin).is_some() {
            return; // an untagged chunk has no stamped sender
        }
        if binary::is_monitor(bin) {
            // Monitor-wall thumbnails never belong to a viewer session (and reach only the admin edition).
            #[cfg(feature = "admin")]
            if let Some((host, jpeg)) = binary::untag_monitor(bin) {
                crate::service::monitor_wall::on_frame(ctx, host, jpeg);
            }
            return;
        }
        match binary::untag(bin) {
            // Frames for a closed tab are dropped.
            Some((host, payload)) => {
                if let Some(s) = ctx.viewers.get(host)
                    && let Some(payload) = self.open(host, payload)
                {
                    if binary::is_audio(&payload) {
                        if s.audio_on.load(Ordering::Relaxed) {
                            ctx.voice.play_binary_packet(&payload);
                        }
                    } else {
                        s.inbox.push(payload.into_owned());
                    }
                }
            }
            None if binary::is_audio(bin) => {
                // Untagged voice comes from the hosted viewer, or from the single viewed host.
                let from = if ctx.is_hosting() { ctx.partner() } else { single_id(ctx) };
                if let Some(packet) = self.open(&from, bin) {
                    ctx.voice.play_binary_packet(&packet);
                }
            }
            None => {
                if let Some(s) = ctx.viewers.single()
                    && let Some(frame) = self.open(&s.id, bin)
                {
                    s.inbox.push(frame.into_owned());
                }
            }
        }
    }
}
