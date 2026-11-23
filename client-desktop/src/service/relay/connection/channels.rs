//! The direct file channel of each session (`file-channel`, see `infra::file_channel`): started once the session's
//! end-to-end handshake is done, candidates exchanged over `p2p-signal`, set up again when it is lost (a new
//! instance on a fresh port, new candidates, the same `Noise_XX` and key check), closed with the session.

use std::net::IpAddr;
use std::time::Instant;

use serde_json::Value;

use parking_lot::Mutex;

use super::*;
use crate::domain::file_channel::{self as wire, Redial};
use crate::infra::identity;

/// Candidate lists kept for partners whose channel has not started here yet.
const MAX_PENDING: usize = 8;

impl Connection {
    /// Starts the channel of every session that can have one and has none yet: both sides list `file-channel`
    /// and the end-to-end handshake authenticated the partner's key (the channel checks it again). A lost channel
    /// is replaced once its back-off ran out (see `domain::file_channel::Redial`).
    pub(super) fn maintain_channels(&mut self) {
        if !file_channel::enabled() {
            return;
        }
        let ready: Vec<String> = self
            .e2e
            .iter()
            .filter(|e| e.is_on() && file_transfer::caps_of(&e.partner).file_channel)
            .filter(|e| !self.channels.iter().any(|c| c.partner == e.partner))
            .map(|e| e.partner.clone())
            .collect();
        for partner in ready {
            let peers = self
                .channel_pending
                .iter()
                .position(|(p, _)| *p == partner)
                .map(|pos| self.channel_pending.remove(pos).1);
            self.open_channel(&partner, Redial::default(), peers.unwrap_or_default());
        }
        let now = Instant::now();
        let due: Vec<String> = self.channels.iter().filter(|c| c.redial_due(now)).map(|c| c.partner.clone()).collect();
        for partner in due {
            self.redial_channel(&partner);
        }
    }

    /// Replaces `partner`'s lost channel by a new one, which connects to the partner's latest candidates too (a
    /// partner that set its side up first already sent them).
    fn redial_channel(&mut self, partner: &str) {
        let Some(pos) = self.channels.iter().position(|c| c.partner == partner) else { return };
        let old = self.channels.remove(pos);
        old.close();
        let redial = old.next_redial();
        logger::info("file_channel", &format!("Setting the file channel with {partner} up again."));
        self.open_channel(partner, redial, old.partner_peers());
    }

    /// Starts a channel with `partner`, offers its candidates and connects to `peers` (the partner's, if known).
    fn open_channel(&mut self, partner: &str, redial: Redial, peers: Vec<std::net::SocketAddr>) {
        let Some(id) = identity::local() else { return };
        let Some((is_host, key)) = self
            .e2e
            .iter()
            .find(|e| e.partner == partner && e.is_on())
            .and_then(|e| Some((e.is_host(), e.peer_key()?.to_vec())))
        else {
            return;
        };
        let (tx, from) = (Mutex::new(self.channel_in_tx.clone()), partner.to_string());
        let sink: file_channel::Sink = Arc::new(move |kind, payload| {
            let _ = tx.lock().send((from.clone(), kind, payload));
        });
        let Some(channel) = FileChannel::start(is_host, &self.ctx.local_id, partner, id.secret(), &key, sink, redial)
        else {
            logger::warn("file_channel", "Could not open a port for the file channel; files use the other paths.");
            return;
        };
        // Public IPv4 addresses the P2P path learned (STUN, port mapping), offered with the channel's port.
        let public: Vec<IpAddr> = self
            .links
            .iter()
            .filter(|p| p.partner == partner)
            .flat_map(|p| p.local_candidates())
            .map(|a| a.ip())
            .filter(is_public_v4)
            .collect();
        let cands = channel.candidates(&public);
        let list: Vec<String> = cands.iter().map(|a| a.to_string()).collect();
        logger::info("file_channel", &format!("File channel with {partner}: offering [{}].", list.join(", ")));
        self.ctx.send(ClientMessage::P2pSignal { target_clean: partner.to_string(), candidate: wire::signal(&cands) });
        if !peers.is_empty() {
            channel.connect_to(&peers);
        }
        self.channels.push(channel);
    }

    /// The partner's channel candidates; kept until our side of the channel starts. A partner that sends new ones
    /// while ours is lost set its side up again: ours follows at once instead of waiting out its back-off.
    pub(in crate::service::relay) fn on_channel_signal(&mut self, from: &str, v: &Value) {
        let peers = wire::parse_signal(v);
        if peers.is_empty() || !file_channel::enabled() {
            return;
        }
        match self.channels.iter().find(|c| c.partner == from) {
            Some(c) if c.may_answer() => {
                c.connect_to(&peers);
                self.redial_channel(from);
            }
            Some(c) => c.connect_to(&peers),
            None => {
                self.channel_pending.retain(|(p, _)| p != from);
                if self.channel_pending.len() >= MAX_PENDING {
                    self.channel_pending.remove(0);
                }
                self.channel_pending.push((from.to_string(), peers));
            }
        }
    }

    /// True while `partner`'s file channel was lost and is being set up again.
    pub(super) fn channel_redialing(&self, partner: &str) -> bool {
        self.channels.iter().any(|c| c.partner == partner && c.redialing())
    }

    /// Closes the channels `keep` rejects (their session ended).
    pub(super) fn prune_channels(&mut self, keep: impl Fn(&str) -> bool) {
        self.channels.retain(|c| {
            let k = keep(&c.partner);
            if !k {
                c.close();
            }
            k
        });
        self.channel_pending.retain(|(p, _)| keep(p));
    }
}

/// A public IPv4 address (the channel's IPv6 candidate comes from the interface itself).
fn is_public_v4(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()),
        IpAddr::V6(_) => false,
    }
}
