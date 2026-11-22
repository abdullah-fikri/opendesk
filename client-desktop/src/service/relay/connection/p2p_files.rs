//! Files over the direct paths: the P2P path (`file-p2p`) and the file channel (`file-channel`). Routing,
//! verdicts and the receiving side; both carry the same file stream messages and are handled alike.

use super::*;
use crate::domain::file_browser::MSG_BROWSE;
use crate::service::ui_port::BrowseUpdate;

/// Key prefix of transfers that arrived over the file channel (P2P ones carry none), so a lost channel cancels
/// only its own and a verdict goes back the way its transfer came.
const VIA_CHANNEL: &str = "fc|";

impl Connection {
    /// True when new transfers to `target` take its P2P path: both sides list `file-p2p` and the path is direct.
    pub(super) fn file_p2p_to(&self, target: &str) -> bool {
        p2p::file_p2p_enabled()
            && file_transfer::caps_of(target).file_p2p
            && self.links.iter().any(|p| p.partner == target && p.is_direct())
    }

    /// The file channel with `target`, when it is up.
    pub(super) fn channel_to(&self, target: &str) -> Option<&Arc<FileChannel>> {
        self.channels.iter().find(|c| c.partner == target && c.is_up())
    }

    /// The path a transfer to `target` starting now takes: the file channel, else the P2P path, else the relay.
    pub(super) fn file_path_to(&self, target: &str) -> Path {
        if self.channel_to(target).is_some() {
            Path::Channel
        } else if self.file_p2p_to(target) {
            Path::P2p
        } else {
            Path::Relay
        }
    }

    /// What senders to `target` need to know about its path (`file_transfer::set_route`).
    pub(super) fn file_route_now(&self, target: &str) -> file_transfer::Route {
        file_transfer::Route { path: self.file_path_to(target), channel_redialing: self.channel_redialing(target) }
    }

    /// The path of file message `payload` to `target` (see `domain::file_route`).
    pub(super) fn route_file(&mut self, target: &str, payload: &FilePayload) -> Path {
        let step = match payload.action {
            FileAction::Start => Step::Start,
            FileAction::Chunk => Step::Chunk,
            FileAction::End => Step::End { group_end: payload.group_end == Some(true) },
            _ => Step::Other,
        };
        let now = self.file_path_to(target);
        self.file_routes.route(target, &payload.id, payload.group.as_deref(), step, || now)
    }

    /// Routes file message `payload` to `target`; true when it goes over a direct path, which is then left in
    /// [`Connection::routed_now`] for the send right after (routing forgets a transfer at its `end`).
    pub(super) fn file_route_ready(&mut self, target: &str, payload: &FilePayload) -> bool {
        self.routed_now = self.route_file(target, payload);
        self.routed_now != Path::Relay
    }

    pub(super) fn routed_path(&self, target: &str, id: &str) -> Path {
        self.file_routes.routed(target, id)
    }

    /// Sends our verdict (ACK/NACK) on a transfer that came over a direct path back over that path. The relay
    /// only forwards an ACK against the grant a relayed `start` opened; a direct transfer has none, so a relayed
    /// ACK was dropped and the sender gave up after two minutes ("no confirmation from the receiver"), holding its
    /// pull slot all that time. False when the transfer did not come over a direct path or that path is down.
    pub(super) fn ack_over_p2p(&mut self, target: &str, payload: &FilePayload) -> bool {
        let key = format!("{target}:{}", payload.id);
        let channel_key = format!("{VIA_CHANNEL}{key}");
        let via = if self.p2p_incoming.contains(&channel_key) || self.p2p_acks.contains(&channel_key) {
            Path::Channel
        } else if self.p2p_incoming.contains(&key) || self.p2p_acks.contains(&key) {
            Path::P2p
        } else {
            return false;
        };
        let key = if via == Path::Channel { channel_key } else { key };
        // One verdict per transfer: an ACK ends it, a NACK (rejected start or chunk) too.
        self.p2p_incoming.retain(|k| *k != key);
        self.p2p_acks.retain(|k| *k != key);
        let Ok(json) = serde_json::to_vec(payload) else { return false };
        match via {
            Path::Channel => self.channel_to(target).is_some_and(|c| c.send(bulk::MSG_JSON, &json, None)),
            _ => self
                .links
                .iter()
                .find(|p| p.partner == target)
                .is_some_and(|p| p.send_bulk(bulk::MSG_JSON, &json, None)),
        }
    }

    /// Sends one file message over `target`'s direct `path`; its in-flight units are released and its progress
    /// mark recorded once the path took it (P2P: the partner acknowledged it; channel: it is in the socket). A path
    /// that just dropped loses the message: the transfer is cancelled with it (see `on_direct_file`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn send_file_p2p(
        &mut self,
        target: &str,
        path: Path,
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
        let sent = match path {
            Path::Channel => match self.channels.iter().find(|c| c.partner == target) {
                Some(c) => c.send(kind, payload, Some(done)),
                None => {
                    done(false);
                    false
                }
            },
            _ => match self.links.iter().find(|p| p.partner == target) {
                Some(p) => p.send_bulk(kind, payload, Some(done)),
                None => {
                    done(false);
                    false
                }
            },
        };
        if !sent {
            file_transfer::abort_pending_sends();
        }
        if let Some(status) = status {
            self.ctx.ui.status(status);
        }
    }

    /// A file browser message for `target`: over the file channel, else the P2P path, else the relay as a sealed
    /// `browse` event (the relay only forwards it). Fails only while offline.
    pub(super) fn send_browse(&mut self, target: &str, payload: &[u8]) {
        let direct = match self.channel_to(target) {
            Some(c) => c.send(MSG_BROWSE, payload, None),
            None => self.direct(target).is_some_and(|p| p.is_direct() && p.send_bulk(MSG_BROWSE, payload, None)),
        };
        if direct {
            return;
        }
        match serde_json::from_slice::<serde_json::Value>(payload) {
            Ok(msg) if !self.quiet => self.ctx.send_input(target, InputEvent::Browse { msg }),
            _ => self.ctx.ui.browse(BrowseUpdate::Failed {
                partner: target.to_string(),
                message: "Not connected to the server right now; try again in a moment.".into(),
            }),
        }
    }

    /// A file stream message from `from`'s P2P path.
    pub(super) fn on_bulk(&mut self, from: &str, kind: u8, payload: &[u8]) {
        self.on_direct_file(from, Path::P2p, kind, payload);
    }

    /// A file stream message from `from`'s P2P path or file channel (`via`), handled like its relay counterpart;
    /// [`p2p::BULK_LOST`] cancels what was arriving and leaving that way.
    pub(super) fn on_direct_file(&mut self, from: &str, via: Path, kind: u8, payload: &[u8]) {
        let tag = if via == Path::Channel { VIA_CHANNEL } else { "" };
        match kind {
            p2p::BULK_LOST => {
                let prefix = format!("{tag}{from}:");
                let incoming: Vec<String> = self
                    .p2p_incoming
                    .iter()
                    .filter(|k| k.starts_with(&prefix))
                    .map(|k| k.trim_start_matches(VIA_CHANNEL).to_string())
                    .collect();
                self.p2p_incoming.retain(|k| !k.starts_with(&prefix));
                self.p2p_acks.retain(|k| !k.starts_with(&prefix));
                if !incoming.is_empty() {
                    let ctx = Arc::clone(&self.ctx);
                    if let Some(status) = self.receiver.abort_keys(ctx.ui.as_ref(), &incoming) {
                        ctx.ui.status(status);
                    }
                    virtual_clipboard::abort_pulls();
                }
                if self.file_routes.drop_path(from, via) {
                    file_transfer::abort_pending_sends();
                }
            }
            bulk::MSG_JSON => {
                if let Ok(file) = serde_json::from_slice::<FilePayload>(payload) {
                    let key = format!("{tag}{from}:{}", file.id);
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
            MSG_BROWSE => crate::service::file_browser::on_message(&self.ctx, from, payload),
            _ => {}
        }
        match via {
            Path::Channel => {
                if let Some(c) = self.channels.iter().find(|c| c.partner == from) {
                    c.consumed(payload.len());
                }
            }
            _ => {
                if let Some(p) = self.links.iter().find(|p| p.partner == from) {
                    p.bulk_consumed(payload.len());
                }
            }
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
