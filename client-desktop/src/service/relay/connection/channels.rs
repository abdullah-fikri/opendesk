//! The direct file channel of each session (`file-channel`, see `infra::file_channel`): started once the session's
//! end-to-end handshake is done, candidates exchanged over `p2p-signal`, closed with the session.

use std::net::IpAddr;

use serde_json::Value;

use parking_lot::Mutex;

use super::*;
use crate::domain::file_channel as wire;
use crate::infra::identity;

/// Candidate lists kept for partners whose channel has not started here yet.
const MAX_PENDING: usize = 8;

impl Connection {
    /// Starts the channel of every session that can have one and has none yet: both sides list `file-channel`
    /// and the end-to-end handshake authenticated the partner's key (the channel checks it again).
    pub(super) fn maintain_channels(&mut self) {
        if !file_channel::enabled() {
            return;
        }
        let Some(id) = identity::local() else { return };
        let ready: Vec<(String, bool, Vec<u8>)> = self
            .e2e
            .iter()
            .filter(|e| e.is_on() && file_transfer::caps_of(&e.partner).file_channel)
            .filter(|e| !self.channels.iter().any(|c| c.partner == e.partner))
            .filter_map(|e| Some((e.partner.clone(), e.is_host(), e.peer_key()?.to_vec())))
            .collect();
        for (partner, is_host, key) in ready {
            let (tx, from) = (Mutex::new(self.channel_in_tx.clone()), partner.clone());
            let sink: file_channel::Sink = Arc::new(move |kind, payload| {
                let _ = tx.lock().send((from.clone(), kind, payload));
            });
            let Some(channel) = FileChannel::start(is_host, &self.ctx.local_id, &partner, id.secret(), &key, sink)
            else {
                logger::warn("file_channel", "Could not open a port for the file channel; files use the other paths.");
                continue;
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
            self.ctx.send(ClientMessage::P2pSignal { target_clean: partner.clone(), candidate: wire::signal(&cands) });
            if let Some(pos) = self.channel_pending.iter().position(|(p, _)| *p == partner) {
                let (_, peers) = self.channel_pending.remove(pos);
                channel.connect_to(&peers);
            }
            self.channels.push(channel);
        }
    }

    /// The partner's channel candidates; kept until our side of the channel starts.
    pub(in crate::service::relay) fn on_channel_signal(&mut self, from: &str, v: &Value) {
        let peers = wire::parse_signal(v);
        if peers.is_empty() || !file_channel::enabled() {
            return;
        }
        match self.channels.iter().find(|c| c.partner == from) {
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
