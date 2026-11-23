//! The relay file lane of this device (`file-lane`, see `file_lane.rs`): open while a session partner can take
//! transfers over it, its messages handled exactly like their main-socket counterparts, and its loss cutting (and
//! resuming) only what ran on it.

use super::file_lane::{FileLane, LaneEvent};
use super::*;
use crate::domain::bulk::OnDelivered;
use crate::domain::file_route::Path;

/// A lane no session needs any more stays open this long (a transfer often follows a session closely).
const LANE_LINGER: Duration = Duration::from_secs(120);
/// Incoming lane transfers remembered (a paste of many small files starts many at once).
const MAX_LANE_INCOMING: usize = 256;

impl Connection {
    /// True when new transfers to `target` may take our lane: it is up, and the relay and `target` list `file-lane`.
    pub(super) fn lane_to(&self, target: &str) -> bool {
        self.lane.as_ref().is_some_and(|l| l.is_up())
            && self.ctx.relay.caps.lock().file_lane
            && file_transfer::caps_of(target).file_lane
    }

    /// Opens the lane while a session partner can use it and closes it once none could for [`LANE_LINGER`] and
    /// nothing runs on it. Also tells the senders which partners' transfers start on a fast path.
    pub(super) fn maintain_lane(&mut self) {
        let partners = self.live_sessions();
        let wanted = file_lane::enabled()
            && self.ctx.relay.online.load(Ordering::SeqCst)
            && self.ctx.relay.caps.lock().file_lane
            && partners.iter().any(|p| file_transfer::caps_of(p).file_lane);
        if wanted {
            self.lane_idle = None;
            if self.lane.is_none() {
                self.open_lane();
            }
        } else if self.lane.is_some() {
            let since = *self.lane_idle.get_or_insert_with(Instant::now);
            if since.elapsed() >= LANE_LINGER && !self.file_routes.uses(Path::Lane) && self.lane_incoming.is_empty() {
                logger::info("file_lane", "No session needs the file lane any more; closing it.");
                self.close_lane();
            }
        }
        // The lane paces itself through TCP: the senders' relay spacing does not apply to it (`set_route`).
        for p in &partners {
            file_transfer::set_route(p, self.file_route_now(p));
        }
    }

    fn open_lane(&mut self) {
        self.lane_gen += 1;
        let ctx = Arc::clone(&self.ctx);
        let request: Box<dyn Fn() + Send> = Box::new(move || ctx.send(ClientMessage::FileLaneRequest));
        let lane = FileLane::start(self.lane_gen, &self.url, &self.ctx.local_id, self.lane_in_tx.clone(), request);
        self.lane = Some(lane);
    }

    /// Closes the lane (no longer needed, or the relay connection ended) and cuts what ran on it.
    pub(super) fn close_lane(&mut self) {
        if let Some(lane) = self.lane.take() {
            lane.close();
            // Whatever it still reports is stale.
            self.lane_gen += 1;
            self.on_lane_down();
        }
    }

    /// A lane token from the relay (`file-lane-token`).
    pub(in crate::service::relay) fn on_lane_token(&self, token: String) {
        if let Some(lane) = &self.lane {
            lane.give_token(token);
        }
    }

    /// Handles what the lane delivered.
    pub(super) fn drain_lane(&mut self) {
        while let Ok((generation, event)) = self.lane_in.try_recv() {
            if generation != self.lane_gen {
                continue;
            }
            self.busy = true;
            match event {
                LaneEvent::Up => {}
                LaneEvent::Lost => self.on_lane_down(),
                LaneEvent::Text(text) => {
                    self.on_lane_text(&text);
                    self.lane_consumed(text.len());
                }
                LaneEvent::Binary(bin) => {
                    self.on_lane_binary(&bin);
                    self.lane_consumed(bin.len());
                }
            }
        }
    }

    fn lane_consumed(&self, bytes: usize) {
        if let Some(lane) = &self.lane {
            lane.consumed(bytes);
        }
    }

    /// A `file-transfer` from the lane: handled like one from the main socket, and remembered while it runs so a
    /// lost lane cuts it. Nothing else is taken from a lane.
    pub(super) fn on_lane_text(&mut self, text: &str) {
        let Some(ServerMessage::FileTransfer { from_clean: Some(from), payload }) = ServerMessage::parse(text) else {
            return;
        };
        let key = format!("{from}:{}", payload.id);
        match payload.action {
            FileAction::Start if !self.lane_incoming.contains(&key) => {
                if self.lane_incoming.len() >= MAX_LANE_INCOMING {
                    self.lane_incoming.remove(0);
                }
                self.lane_incoming.push(key);
            }
            FileAction::End => self.lane_incoming.retain(|k| *k != key),
            _ => {}
        }
        self.on_file(Some(from), *payload);
    }

    /// A file chunk from the lane, tagged with its sender like on the main socket; anything else is dropped.
    pub(super) fn on_lane_binary(&mut self, bin: &[u8]) {
        if let Some((from, payload)) = binary::untag(bin)
            && binary::parse_file_chunk(payload).is_some()
            && let Some(payload) = self.open(from, payload)
            && let Some(chunk) = binary::parse_file_chunk(&payload)
        {
            self.on_file_chunk(from, &chunk);
        }
    }

    /// Sends one file message to `target` over our lane: JSON as `relay-file`, a chunk sealed in the lane's nonce
    /// domain and tagged with its receiver (a lane serves every session). `done` runs once it is in the socket, or
    /// with false when it could not be sent.
    pub(super) fn send_lane(&mut self, target: &str, kind: u8, payload: &[u8], done: OnDelivered) -> bool {
        let msg = match kind {
            bulk::MSG_JSON => Some(Message::Text(lane_file_text(target, payload).into())),
            _ => {
                let sealed =
                    self.e2e_for(target).and_then(|e| e.seal_lane_file(payload)).unwrap_or_else(|| payload.to_vec());
                binary::tag(target, &sealed).map(|m| Message::Binary(m.into()))
            }
        };
        match (self.lane.as_ref(), msg) {
            (Some(lane), Some(msg)) => lane.send(msg, Some(done)),
            _ => {
                done(false);
                false
            }
        }
    }

    /// Our lane dropped or closed: what arrived over it is kept for its sender to resume, and what left over it is
    /// cut (and resumes on whatever path is up then).
    pub(super) fn on_lane_down(&mut self) {
        let incoming = std::mem::take(&mut self.lane_incoming);
        if !incoming.is_empty() {
            let ctx = Arc::clone(&self.ctx);
            if let Some(status) = self.receiver.abort_keys(ctx.ui.as_ref(), &incoming) {
                ctx.ui.status(status);
            }
            virtual_clipboard::abort_pulls();
        }
        if self.file_routes.drop_everywhere(Path::Lane) {
            file_transfer::abort_pending_sends();
        }
    }

    /// `file-lane-lost`: the relay cut transfers `ids` between us and `from` because a lane (ours or its) dropped.
    /// Ours that arrive are kept for resuming; if we send any of them, our sends are cut and resume.
    pub(in crate::service::relay) fn on_file_lane_lost(&mut self, from: &str, ids: &[String]) {
        let ctx = Arc::clone(&self.ctx);
        let mut ours = false;
        let mut theirs = false;
        for id in ids {
            let key = format!("{from}:{id}");
            self.lane_incoming.retain(|k| *k != key);
            if self.file_routes.routed(from, id) == Path::Lane {
                ours = true;
            } else if let Some(status) = self.receiver.abort_keys(ctx.ui.as_ref(), std::slice::from_ref(&key)) {
                theirs = true;
                ctx.ui.status(status);
            }
        }
        if theirs {
            virtual_clipboard::abort_pulls();
        }
        logger::info("file_lane", &format!("Relay cut {} transfer(s) with {from} on a lost file lane.", ids.len()));
        if ours {
            self.file_routes.drop_path(from, Path::Lane);
            file_transfer::abort_pending_sends();
        }
    }
}

/// The `relay-file` text of one file payload (`json`, already serialized) for `target`.
fn lane_file_text(target: &str, json: &[u8]) -> String {
    let target = serde_json::to_string(target).unwrap_or_default();
    format!(r#"{{"type":"relay-file","targetClean":{target},"payload":{}}}"#, String::from_utf8_lossy(json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_file_text_is_the_relay_file_message() {
        let payload = FilePayload::ack("ft_1", true);
        let json = serde_json::to_vec(&payload).unwrap();
        let text = lane_file_text("222222222", &json);
        let expected = ClientMessage::file("222222222", payload).to_json();
        let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(parse(&text), parse(&expected));
    }
}
