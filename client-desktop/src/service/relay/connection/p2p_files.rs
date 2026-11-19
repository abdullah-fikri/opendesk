//! Files over the P2P path (`file-p2p`): routing, verdicts and the receiving side.

use super::*;

impl Connection {
    /// True when new transfers to `target` take its P2P path: both sides list `file-p2p` and the path is direct.
    pub(super) fn file_p2p_to(&self, target: &str) -> bool {
        p2p::file_p2p_enabled()
            && file_transfer::caps_of(target).file_p2p
            && self.links.iter().any(|p| p.partner == target && p.is_direct())
    }

    /// Whether file message `payload` to `target` goes over the P2P path (see `domain::file_route`).
    pub(super) fn route_file(&mut self, target: &str, payload: &FilePayload) -> bool {
        let step = match payload.action {
            FileAction::Start => Step::Start,
            FileAction::Chunk => Step::Chunk,
            FileAction::End => Step::End { group_end: payload.group_end == Some(true) },
            _ => Step::Other,
        };
        let p2p_now = self.file_p2p_to(target);
        self.file_routes.route(target, &payload.id, payload.group.as_deref(), step, || p2p_now)
    }

    pub(super) fn routed_p2p(&self, target: &str, id: &str) -> bool {
        self.file_routes.routed(target, id)
    }

    /// Sends our verdict (ACK/NACK) on a transfer that came over `target`'s P2P path back over that path. The
    /// relay only forwards an ACK against the grant a relayed `start` opened; a P2P transfer has none, so a
    /// relayed ACK was dropped and the sender gave up after two minutes ("no confirmation from the receiver"),
    /// holding its pull slot all that time. False when the transfer did not come over P2P or the path is down.
    pub(super) fn ack_over_p2p(&mut self, target: &str, payload: &FilePayload) -> bool {
        let key = format!("{target}:{}", payload.id);
        let came_p2p = self.p2p_incoming.contains(&key) || self.p2p_acks.contains(&key);
        if !came_p2p {
            return false;
        }
        // One verdict per transfer: an ACK ends it, a NACK (rejected start or chunk) too.
        self.p2p_incoming.retain(|k| *k != key);
        self.p2p_acks.retain(|k| *k != key);
        let Ok(json) = serde_json::to_vec(payload) else { return false };
        self.links.iter().find(|p| p.partner == target).is_some_and(|p| p.send_bulk(bulk::MSG_JSON, &json, None))
    }

    /// Sends one file message over `target`'s P2P path; its in-flight units are released and its progress mark
    /// recorded once the partner acknowledged it. A path that just dropped loses the message: the transfer is
    /// cancelled with it (see `on_bulk`).
    pub(super) fn send_file_p2p(
        &mut self,
        target: &str,
        kind: u8,
        payload: &[u8],
        units: usize,
        status: Option<String>,
        mark: Option<FileMark>,
    ) {
        let units = units.max(1);
        let done: bulk::OnDelivered = Box::new(move |ok| {
            IN_FLIGHT_CHUNKS.fetch_sub(units, Ordering::Relaxed);
            if ok && let Some(mark) = &mark {
                file_transfer::set_delivered(mark);
            }
        });
        let sent = match self.links.iter().find(|p| p.partner == target) {
            Some(p) => p.send_bulk(kind, payload, Some(done)),
            None => {
                done(false);
                false
            }
        };
        if !sent {
            file_transfer::abort_pending_sends();
        }
        if let Some(status) = status {
            self.ctx.ui.status(status);
        }
    }

    /// A file stream message from `from`'s P2P path, handled like its relay counterpart; [`p2p::BULK_LOST`]
    /// cancels what was arriving and leaving that way.
    pub(super) fn on_bulk(&mut self, from: &str, kind: u8, payload: &[u8]) {
        match kind {
            p2p::BULK_LOST => {
                let prefix = format!("{from}:");
                let incoming: Vec<String> =
                    self.p2p_incoming.iter().filter(|k| k.starts_with(&prefix)).cloned().collect();
                self.p2p_incoming.retain(|k| !k.starts_with(&prefix));
                self.p2p_acks.retain(|k| !k.starts_with(&prefix));
                if !incoming.is_empty() {
                    let ctx = Arc::clone(&self.ctx);
                    if let Some(status) = self.receiver.abort_keys(ctx.ui.as_ref(), &incoming) {
                        ctx.ui.status(status);
                    }
                    virtual_clipboard::abort_pulls();
                }
                if self.file_routes.drop_p2p(from) {
                    file_transfer::abort_pending_sends();
                }
            }
            bulk::MSG_JSON => {
                if let Ok(file) = serde_json::from_slice::<FilePayload>(payload) {
                    let key = format!("{from}:{}", file.id);
                    match file.action {
                        FileAction::Start if !self.p2p_incoming.contains(&key) => self.p2p_incoming.push(key),
                        FileAction::End => {
                            self.p2p_incoming.retain(|k| *k != key);
                            // Its verdict goes back over this path (see `ack_over_p2p`); bounded in case none comes.
                            if self.p2p_acks.len() >= MAX_P2P_ACKS {
                                self.p2p_acks.pop_front();
                            }
                            self.p2p_acks.push_back(key);
                        }
                        _ => {}
                    }
                    self.on_file(Some(from.to_string()), file);
                }
            }
            bulk::MSG_BINARY => {
                if let Some(chunk) = binary::parse_file_chunk(payload) {
                    self.on_file_chunk(from, &chunk);
                }
            }
            _ => {}
        }
        if let Some(p) = self.links.iter().find(|p| p.partner == from) {
            p.bulk_consumed(payload.len());
        }
    }

    /// A binary file chunk; `from` is the relay's tag, like `fromClean` on `file-transfer`. A rejected chunk is
    /// answered with a NACK so the sender stops at once.
    pub(super) fn on_file_chunk(&mut self, from: &str, chunk: &binary::FileChunk) {
        let ctx = Arc::clone(&self.ctx);
        // A deflated chunk is inflated first, so every reader below (receiver, paste stream) sees plain bytes.
        let inflated;
        let chunk = if chunk.offset & file_squeeze::OFFSET_DEFLATED != 0 {
            match file_squeeze::unpack(chunk.data, file_transfer::BINARY_CHUNK_SIZE) {
                Some(data) => {
                    inflated = data;
                    &binary::FileChunk {
                        id: chunk.id,
                        offset: chunk.offset & !file_squeeze::OFFSET_DEFLATED,
                        data: &inflated,
                    }
                }
                None => {
                    logger::warn("file_transfer", &format!("Corrupt deflated chunk for id={}; rejecting.", chunk.id));
                    ctx.send(ClientMessage::file(from, FilePayload::ack(chunk.id, false)));
                    return;
                }
            }
        } else {
            chunk
        };
        let (status, ack) = self.receiver.handle_chunk(&ctx, chunk, from);
        if let Some(ok) = ack {
            ctx.send(ClientMessage::file(from, FilePayload::ack(chunk.id, ok)));
        }
        if let Some(status) = status {
            ctx.ui.status(status);
        }
    }
}
