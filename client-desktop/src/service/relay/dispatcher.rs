//! Turns relay messages into service calls. Each handler decodes nothing itself: `ServerMessage` and
//! `InputEvent` are already typed, so handlers only decide which use case runs.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use core_engine::capture::HostSupport;

use crate::domain::capability::{PeerCaps, RelayCaps};
use crate::domain::desk_id;
use crate::infra::e2e::{self, E2e, Verdict};
use crate::infra::identity;
use crate::infra::key_pins::{self, Pin};
use crate::infra::p2p::{self, P2p, Sinks};
use crate::infra::{logger, privacy, virtual_clipboard};
use crate::protocol::{ClientMessage, FileAction, FilePayload, HostEntry, InputEvent, ServerMessage};
use crate::service::file_transfer::{self, offer};
use crate::service::host;
use crate::service::relay::connection::Connection;
use crate::service::ui_port::{RelayStatus, RemoteHost};
use crate::service::viewer::controller;

/// Fallback text when the relay closes a session without a message.
const SESSION_ENDED: &str = "Session ended.";

impl Connection {
    pub(super) fn on_message(&mut self, msg: ServerMessage) {
        match msg {
            ServerMessage::RegisteredAck { caps } => self.on_registered(&caps),
            ServerMessage::HostList { hosts } => self.on_host_list(hosts),
            ServerMessage::SessionConnected { target_clean, role, peer_caps } => {
                self.on_session_connected(&target_clean, &role, &peer_caps)
            }
            ServerMessage::InputCommand { event, from_clean } => self.on_input(&event, from_clean.as_deref()),
            ServerMessage::ClipboardSync { text, from_clean } => self.on_clipboard(text, from_clean.as_deref()),
            ServerMessage::FileTransfer { from_clean, payload } => self.on_file(from_clean, payload),
            ServerMessage::P2pSignal { from_clean, candidate } => self.on_signal(&from_clean, &candidate),
            ServerMessage::UdpRelayOffer { target_clean, addr, token } => {
                if let Some(p) = self.links.iter().find(|p| p.partner == target_clean) {
                    p.add_relay(&addr, &token);
                }
            }
            ServerMessage::SessionClosed { message, target_clean } => {
                self.on_session_closed(message, target_clean.as_deref())
            }
            ServerMessage::Error { message, target_clean } => self.on_error(message, target_clean.as_deref()),
            ServerMessage::MonitorStart { max_width, fps } => {
                host::monitor_stream::start(&self.ctx, host::monitor_stream::Profile::clamped(max_width, fps))
            }
            ServerMessage::MonitorStop => host::monitor_stream::stop(&self.ctx),
            ServerMessage::DeviceCredential { token } => {
                crate::service::settings::store_device_token(&self.ctx, &token)
            }
            #[cfg(feature = "admin")]
            ServerMessage::SecurityState { enabled, managed_by_env, devices } => {
                self.ctx.ui.security_changed(crate::service::ui_port::SecurityView {
                    supported: true,
                    enabled,
                    managed_by_env,
                    devices,
                })
            }
            #[cfg(feature = "admin")]
            ServerMessage::MonitorState { target_clean, state } => {
                crate::service::monitor_wall::on_state(&self.ctx, &target_clean, &state)
            }
            ServerMessage::Unknown => {}
        }
    }

    fn on_registered(&self, caps: &[String]) {
        let ctx = &self.ctx;
        let relay = RelayCaps::from_list(caps);
        *ctx.relay.caps.lock() = relay;
        // Both sides must support it: we advertised it in `register` unless this build cannot view.
        let multi = ctx.edition.can_view() && relay.multi_session;
        ctx.viewers.set_multi(multi);
        logger::info("relay", &format!("Registered. Viewer sessions: {}", if multi { "up to 4" } else { "one" }));
        ctx.ui.relay_status(RelayStatus::Online);
        ctx.relay.online.store(true, Ordering::SeqCst);
        #[cfg(feature = "admin")]
        {
            crate::service::monitor_wall::relay_registered(ctx, relay.monitor);
            ctx.ui.relay_admin_caps(relay.device_security, relay.host_alias);
            if relay.device_security {
                ctx.send(ClientMessage::SecurityGet);
            }
        }
    }

    fn on_host_list(&self, hosts: Vec<HostEntry>) {
        let local = desk_id::format(&self.ctx.local_id);
        let hosts: Vec<RemoteHost> = hosts
            .into_iter()
            .filter(|h| !h.desk_id.is_empty() && h.desk_id != local)
            .map(|h| RemoteHost {
                desk_id: h.desk_id,
                device_name: if h.device_name.is_empty() { "Device".into() } else { h.device_name },
                alias: h.alias,
                platform: h.platform,
                online: h.online,
                last_seen: h.last_seen,
            })
            .collect();
        #[cfg(feature = "admin")]
        crate::service::monitor_wall::set_hosts(&self.ctx, &hosts);
        self.ctx.ui.host_list(hosts);
    }

    fn on_session_connected(&mut self, target: &str, role: &str, peer_caps: &[String]) {
        let ctx = Arc::clone(&self.ctx);
        let relay = *ctx.relay.caps.lock();
        let caps = PeerCaps::from_list(peer_caps, relay.file_offer);
        let is_host = role == "host";
        // A session that cannot be captured (Linux on Wayland) is ended at once; normally the relay already
        // refused it because registration said `viewer-only`.
        if is_host && let HostSupport::Unsupported(reason) = core_engine::capture::host_support() {
            logger::warn("relay", &format!("Refusing to host {target}: {reason}"));
            ctx.send(ClientMessage::disconnect(target));
            return;
        }
        // A viewer of several hosts keeps one capability set per host; everything else has one partner.
        let multi_viewer = !is_host && ctx.viewers.multi();
        if !multi_viewer {
            file_transfer::set_partner_caps(caps);
        }
        ctx.mark_clipboard_synced();
        // A direct path belongs to its pair only.
        self.links.retain(|p| {
            let keep = multi_viewer && p.partner != target;
            if !keep {
                p.close();
            }
            keep
        });
        self.e2e.retain(|e| multi_viewer && e.partner != target);
        // Before the P2P path, which uses the end-to-end keys when there are any.
        if !self.start_e2e(is_host, target, caps.e2e, relay.p2p_signal) {
            return;
        }
        let p2p_ok = relay.p2p_signal && caps.p2p && p2p::enabled();
        logger::info("relay", &format!("Session started: partner={target} role={role} p2p={p2p_ok}"));

        privacy::reset_host_privacy();
        ctx.voice.is_session_active.store(true, Ordering::SeqCst);
        // The microphone is on by default in every session.
        ctx.voice.is_mic_muted.store(false, Ordering::SeqCst);
        ctx.host.can_send_frame.open();

        if is_host {
            // A viewer's voice partner is its focused host (see `controller::apply_focus`).
            ctx.voice.use_opus.store(caps.audio_opus, Ordering::SeqCst);
            if p2p_ok {
                let sinks = Sinks {
                    video: Arc::new(|_| {}),
                    voice: Arc::clone(&ctx.voice),
                    input: self.p2p_input_tx.clone(),
                    audio_gate: None,
                };
                self.start_link(true, target, sinks);
            }
            host::start(&ctx, target, caps);
            return;
        }
        let Some(sess) = controller::on_connected(&ctx, target, caps) else { return };
        if p2p_ok {
            let inbox = Arc::clone(&sess.inbox);
            let sinks = Sinks {
                video: Arc::new(move |frame| inbox.push(frame)),
                voice: Arc::clone(&ctx.voice),
                input: self.p2p_input_tx.clone(),
                audio_gate: Some(Arc::clone(&sess.audio_on)),
            };
            self.start_link(false, target, sinks);
        }
    }

    fn start_link(&mut self, is_host: bool, partner: &str, sinks: Sinks) {
        let ctx = &self.ctx;
        let e2e = self.e2e_for(partner).cloned();
        let link = P2p::start(
            is_host,
            &ctx.local_id,
            partner.to_string(),
            Arc::clone(&ctx.host.can_send_frame),
            ctx.outbox.clone(),
            sinks,
            e2e,
        );
        self.links.extend(link);
    }

    /// Starts the end-to-end handshake with a partner that lists `e2e` (the viewer sends the first message).
    /// False when the session was refused: a host with a pinned key that no longer offers `e2e` (or a relay that
    /// no longer forwards the handshake) is treated like a changed key, since a relay or an impostor holding the
    /// host's ID can strip the capability. Turning end-to-end off on this machine (`OPENDESK_E2E=0`) is a local
    /// choice and refuses nothing.
    fn start_e2e(&mut self, is_host: bool, partner: &str, peer_e2e: bool, relay_signal: bool) -> bool {
        let ctx = &self.ctx;
        let id = e2e::enabled().then(identity::local).flatten();
        let (true, true, Some(id)) = (peer_e2e, relay_signal, id) else {
            if !is_host && e2e::enabled() && key_pins::pinned(partner).is_some() {
                let msg = format!(
                    "Security warning: {} used end-to-end encryption before but does not offer it now. The session was closed. If that is expected (for example the host turned it off), run `opendesk trust {partner}` and connect again.",
                    desk_id::format(partner)
                );
                logger::warn("e2e", &msg);
                if ctx.viewers.get(partner).is_some() {
                    controller::close(ctx, partner, Some(msg.clone()));
                } else {
                    ctx.send(ClientMessage::disconnect(partner));
                }
                ctx.ui.status(msg);
                return false;
            }
            return true;
        };
        let Some((channel, first)) = E2e::start(is_host, &ctx.local_id, partner, id) else { return true };
        if let Some(candidate) = first {
            ctx.send(ClientMessage::P2pSignal { target_clean: partner.to_string(), candidate });
        }
        self.e2e.push(channel);
        true
    }

    pub(super) fn e2e_for(&self, partner: &str) -> Option<&Arc<E2e>> {
        self.e2e.iter().find(|e| e.partner == partner)
    }

    /// A `p2p-signal`: the end-to-end handshake, or the partner's P2P path.
    fn on_signal(&mut self, from: &str, candidate: &str) {
        let parsed = serde_json::from_str::<serde_json::Value>(candidate).ok();
        let Some(v) = parsed.filter(e2e::is_signal) else {
            if let Some(p) = self.links.iter().find(|p| p.partner == from) {
                p.on_signal(candidate);
            }
            return;
        };
        let Some(channel) = self.e2e_for(from).cloned() else { return };
        let ctx = Arc::clone(&self.ctx);
        let mut rejected = None;
        let reply = channel.on_signal(&v, |key| {
            let fp = identity::fingerprint(key);
            if channel.is_host() {
                // Hosts accept every viewer (unattended access); the key is only logged.
                logger::info("e2e", &format!("Viewer {from} key {fp}."));
                return Verdict::Trust;
            }
            match key_pins::check(from, key) {
                Pin::New => logger::info("e2e", &format!("Pinned key {fp} of host {from}.")),
                Pin::Match => {}
                Pin::Mismatch { pinned } => {
                    rejected = Some((fp, identity::fingerprint(&pinned)));
                    return Verdict::Reject;
                }
            }
            Verdict::Trust
        });
        if let Some(candidate) = reply {
            ctx.send(ClientMessage::P2pSignal { target_clean: from.to_string(), candidate });
        }
        if let Some((fp, old)) = rejected {
            let msg = format!(
                "Security warning: the key of {} changed (now {fp}, was {old}). The session was closed. If the host was reinstalled, run `opendesk trust {from}` and connect again.",
                desk_id::format(from)
            );
            logger::warn("e2e", &msg);
            controller::close(&ctx, from, Some(msg.clone()));
            ctx.ui.status(msg);
        }
    }

    /// An event from a host this machine views (tagged with `fromClean`, or the single session), else from the
    /// viewer this machine hosts.
    fn on_input(&self, event: &serde_json::Value, from: Option<&str>) {
        let ctx = &self.ctx;
        // The same sender the routing below picks: the tagged host, the single viewed host, or the hosted viewer.
        let sender = match from {
            Some(id) => id.to_string(),
            None if !ctx.is_hosting() => ctx.viewers.single().map(|s| s.id.clone()).unwrap_or_default(),
            None => ctx.partner(),
        };
        let opened;
        let event = match self.e2e_for(&sender) {
            Some(e) => match e.open_event(event) {
                Some(v) => {
                    opened = v;
                    &opened
                }
                None => return,
            },
            None => event,
        };
        let Some(event) = InputEvent::parse(event) else { return };
        let viewed = match from {
            Some(id) => ctx.viewers.get(id),
            None if !ctx.is_hosting() => ctx.viewers.single(),
            None => None,
        };
        match viewed {
            Some(sess) => controller::on_host_event(ctx, &sess, event),
            None => host::input::apply(ctx, event),
        }
    }

    /// Clipboard follows focus: copies on a host in a background tab are ignored.
    fn on_clipboard(&self, text: Option<String>, from: Option<&str>) {
        let ctx = &self.ctx;
        let viewed = from.and_then(|id| ctx.viewers.get(id));
        if viewed.as_ref().is_some_and(|s| !ctx.viewers.is_focused(&s.id)) {
            logger::info("clipboard", "Ignored copied text from a session in the background.");
            return;
        }
        if let Some(text) = text {
            if !core_engine::clipboard::set_text(&text) {
                logger::warn("clipboard", "Received copied text, but the clipboard stayed busy.");
                return;
            }
            logger::info("clipboard", &format!("Received copied text ({} chars).", text.len()));
            let seq = ctx.mark_clipboard_synced();
            if let Some(s) = viewed {
                s.clip_seq.store(seq, Ordering::SeqCst);
            }
        }
    }

    /// `fromClean` is stamped by the relay, never taken from the payload. Outside a session it is the only
    /// sender identity, so a payload without it goes to the current partner (or is dropped without one).
    fn on_file(&mut self, from: Option<String>, payload: FilePayload) {
        let ctx = Arc::clone(&self.ctx);
        let from = from.unwrap_or_else(|| ctx.partner());
        if from.is_empty() {
            return;
        }
        match payload.action {
            FileAction::Ack => file_transfer::deliver_ack(&payload.id, payload.ok == Some(true)),
            FileAction::Offer if ctx.viewers.get(&from).is_some() && !ctx.viewers.is_focused(&from) => {
                logger::info("clipboard", &format!("Ignored file offer from unfocused session {from}."));
            }
            FileAction::Offer => {
                if let Some(status) = virtual_clipboard::set_offer(&from, &payload, ctx.outbox.clone()) {
                    ctx.ui.status(status);
                }
            }
            FileAction::Pull => offer::serve_pull(&ctx, &from, &payload),
            _ => {
                let allow_auto_paste = !ctx.partner().is_empty();
                let (status, ack) = self.receiver.handle(&ctx, &payload, &from, allow_auto_paste);
                if let Some(ok) = ack {
                    ctx.send(ClientMessage::file(&from, FilePayload::ack(&payload.id, ok)));
                }
                if let Some(status) = status {
                    ctx.ui.status(status);
                }
            }
        }
    }

    fn on_session_closed(&self, message: Option<String>, target: Option<&str>) {
        let ctx = &self.ctx;
        if let Some(id) = target.filter(|id| ctx.viewers.get(id).is_some()) {
            // Multi-session: only that tab closes.
            let message = message.unwrap_or_else(|| SESSION_ENDED.into());
            controller::end(ctx, id, Some(format!("{message} ({})", desk_id::format(id))));
            return;
        }
        if target.is_some() && ctx.viewers.multi() && !ctx.is_hosting() {
            // A tab that is already gone (closed here first): nothing else ends.
            return;
        }
        // Host role or the single session: everything ends.
        crate::service::sessions::end_all(ctx);
        logger::info("relay", "Session closed.");
    }

    fn on_error(&self, message: Option<String>, target: Option<&str>) {
        let ctx = &self.ctx;
        let message = message.unwrap_or_else(|| "Error".into());
        logger::warn("relay", &format!("Relay error: {message}"));
        // A failed connect closes its pending tab: named by targetClean (multi-session), or the single pending
        // session.
        let failed: Vec<String> = match target {
            Some(id) => ctx.viewers.get(id).filter(|s| !s.is_connected()).map(|s| s.id.clone()).into_iter().collect(),
            None if !ctx.viewers.multi() => {
                ctx.viewers.all().into_iter().filter(|s| !s.is_connected()).map(|s| s.id.clone()).collect()
            }
            None => Vec::new(),
        };
        for id in failed {
            controller::end(ctx, &id, Some(message.clone()));
        }
        ctx.ui.status(message);
    }
}
