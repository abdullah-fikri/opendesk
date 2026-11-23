//! Session lifetime across connection loss: suspend, resume countdowns and ending sessions.

use super::*;

impl Connection {
    /// The socket failed: relay grants died with it, so every transfer on it has ended. Sessions that can
    /// resume wait for the next connection (see `resume.rs`); every other session has ended.
    pub(super) fn on_connection_lost(&mut self) {
        let ctx = Arc::clone(&self.ctx);
        file_transfer::abort_pending_sends();
        virtual_clipboard::abort_pulls();
        virtual_clipboard::clear_offer("");
        for file in self.files.drain(..) {
            IN_FLIGHT_CHUNKS.fetch_sub(file.units, Ordering::Relaxed);
        }
        // The next connection may take another route: its pacing starts over.
        self.pacer = FilePacer::new();
        let reason = self.receiver.abort_all(ctx.ui.as_ref());
        let waiting = self.resumable_sessions();
        self.resume.suspend(waiting.clone(), Instant::now());
        if waiting.is_empty() {
            crate::service::sessions::end_all(&ctx);
            ctx.ui.relay_status(RelayStatus::Offline { reason });
            return;
        }
        for id in self.live_sessions().into_iter().filter(|id| !waiting.contains(id)) {
            self.end_session(&id, None);
        }
        logger::info("relay", &format!("Connection lost; {} session(s) wait for it to come back.", waiting.len()));
        ctx.ui.relay_status(RelayStatus::Offline { reason: self.reconnecting_status() });
    }

    /// Partners of the sessions this machine holds: the viewed hosts and the hosted viewer.
    pub(super) fn live_sessions(&self) -> Vec<String> {
        let ctx = &self.ctx;
        let mut ids: Vec<String> = ctx.viewers.all().iter().map(|s| s.id.clone()).collect();
        let partner = ctx.partner();
        if ctx.is_hosting() && !partner.is_empty() && !ids.contains(&partner) {
            ids.push(partner);
        }
        ids
    }

    /// Sessions that may wait for a reconnect: connected, with a partner and relay that list `session-resume`,
    /// and no end-to-end handshake still running (its messages may be lost, so it could never finish).
    pub(super) fn resumable_sessions(&self) -> Vec<String> {
        let connected = |id: &str| self.ctx.viewers.get(id).is_none_or(|s| s.is_connected());
        self.live_sessions()
            .into_iter()
            .filter(|id| connected(id) && self.resume.can_wait(id) && self.e2e_for(id).is_none_or(|e| e.is_on()))
            .collect()
    }

    /// Ends the session with `id` locally (the relay ended it, or it cannot continue).
    pub(in crate::service::relay) fn end_session(&mut self, id: &str, status: Option<String>) {
        let ctx = Arc::clone(&self.ctx);
        self.resume.ended(id);
        if ctx.viewers.get(id).is_some() {
            crate::service::viewer::controller::end(&ctx, id, status.clone());
        }
        if ctx.is_hosting() && ctx.partner() == id {
            crate::service::sessions::end_hosting_locally(&ctx);
        }
        if let Some(status) = status {
            ctx.ui.status(status);
        }
    }

    /// A waiting session that did not come back.
    pub(in crate::service::relay) fn end_waiting(&mut self, id: &str, status: &str) {
        logger::info("relay", &format!("Session with {id} ended: {status}"));
        self.end_session(id, Some(status.to_string()));
    }

    /// Updates the "Reconnecting… (N s)" overlay of the viewed hosts once a second; one whose direct path is
    /// still alive keeps its live picture and shows none.
    pub(super) fn show_countdown(&mut self) {
        let Some(secs) = self.resume.countdown(Instant::now()) else { return };
        for s in self.ctx.viewers.all() {
            let shown = if self.direct(&s.id).is_some() { 0 } else { secs as i32 };
            self.ctx.ui.session_changed(s.id.clone(), SessionChange::Reconnecting(shown));
        }
    }

    /// Updates the "Reconnecting… (N s)" overlay of viewed hosts that lost their own connection; one whose
    /// direct path is still alive keeps its live picture and shows none.
    pub(super) fn show_partner_countdown(&mut self) {
        let now = Instant::now();
        // The relay closes a host that does not come back; past that, drop the entry if no close arrived.
        self.partner_waits.retain(|(_, deadline, _)| now < *deadline + Duration::from_secs(5));
        let mut waits = std::mem::take(&mut self.partner_waits);
        for (id, deadline, shown) in &mut waits {
            let secs = if self.direct(id).is_some() { 0 } else { partner_seconds_left(*deadline, now) };
            if *shown != secs {
                *shown = secs;
                self.ctx.ui.session_changed(id.clone(), SessionChange::Reconnecting(secs));
            }
        }
        self.partner_waits = waits;
    }

    /// Host `id` lost its connection: its tab counts down the relay's grace.
    pub(in crate::service::relay) fn partner_suspended(&mut self, id: &str) {
        if self.ctx.viewers.get(id).is_none() {
            return;
        }
        self.partner_waits.retain(|w| w.0 != id);
        self.partner_waits.push((id.to_string(), Instant::now() + resume::GRACE, -1));
    }

    /// Host `id` is back or gone: its overlay clears.
    pub(in crate::service::relay) fn partner_back(&mut self, id: &str) {
        if let Some(i) = self.partner_waits.iter().position(|w| w.0 == id) {
            self.partner_waits.swap_remove(i);
            self.ctx.ui.session_changed(id.to_string(), SessionChange::Reconnecting(0));
        }
    }

    /// "Reconnecting… (N s)" while sessions wait.
    pub(super) fn reconnecting_status(&self) -> Option<String> {
        self.resume
            .any_waiting()
            .then(|| format!("Reconnecting… ({} s)", self.resume.seconds_left(Instant::now()).max(1)))
    }

    /// The connection ended for any reason: the relay owns session authorization, so direct paths and viewer
    /// sessions end with it (a reconnect starts without them), except the sessions that wait for a resume,
    /// which keep their direct path and end-to-end keys.
    pub(super) fn on_connection_ended(&mut self) {
        // The lane's token and grants belonged to this connection; the next one opens a new lane.
        self.close_lane();
        let resume = &self.resume;
        self.links.retain(|p| {
            let keep = resume.is_waiting(&p.partner);
            if !keep {
                p.close();
            }
            keep
        });
        self.e2e.retain(|e| resume.is_waiting(&e.partner));
        let resume = &self.resume;
        let waiting: Vec<String> =
            self.e2e.iter().map(|e| e.partner.clone()).filter(|p| resume.is_waiting(p)).collect();
        self.prune_channels(|p| waiting.iter().any(|w| w == p));
        self.signals.clear();
        let waiting = self.resume.any_waiting();
        let ctx = &self.ctx;
        // Watches live in the relay: a new connection starts without them until the relay asks again.
        crate::service::host::monitor_stream::stop(ctx);
        (self.monitor_pending, self.monitor_in_flight) = (None, false);
        #[cfg(feature = "admin")]
        crate::service::monitor_wall::relay_lost(ctx);
        if !ctx.viewers.is_empty() && !waiting {
            crate::service::viewer::controller::end_all(ctx);
            if !ctx.is_hosting() {
                ctx.set_partner("");
            }
            ctx.voice.is_session_active.store(ctx.is_hosting(), Ordering::SeqCst);
        }
    }
}
