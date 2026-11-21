//! Split between the SYSTEM agent and the clipboard helper (see `infra::clipboard_helper`).

use super::*;

/// Offers the agent passed to the helper, newest last; the helper may pull only from these.
pub(super) static AGENT_OFFERS: Mutex<Vec<Offer>> = Mutex::new(Vec::new());
/// The offer last placed on the user's clipboard through the helper, so a new helper (the user switched between
/// the console and a remote desktop session, or logged on again) gets it back and the copy is not lost.
pub(super) static LAST_SET: Mutex<Option<LastSet>> = Mutex::new(None);
/// Partner, offer ID, entries and when it was placed.
pub(super) type LastSet = (String, String, Vec<OfferEntry>, std::time::Instant);
/// A copy older than this is not put back: the user may have copied something else since.
pub(super) const RESTORE_WITHIN: Duration = Duration::from_secs(120);
/// Offers kept for pulls (same as the sender's `KEPT_OFFERS`).
pub(super) const KEPT_AGENT_OFFERS: usize = 4;
/// Helper process: paste streams waiting for bytes from the agent, by transfer ID.
pub(super) static HELPER_STREAMS: Mutex<Option<HashMap<String, Sender<SinkMsg>>>> = Mutex::new(None);

pub(super) fn helper_streams<R>(f: impl FnOnce(&mut HashMap<String, Sender<SinkMsg>>) -> R) -> R {
    f(HELPER_STREAMS.lock().get_or_insert_with(HashMap::new))
}

/// Agent: the helper asks to pull or closes a paste stream. Every pull is checked against our own offers.
pub fn helper_request(msg: ToAgent) {
    use crate::infra::clipboard_helper::send;
    match msg {
        ToAgent::Pull { from, offer_id, index, tid } => {
            if !is_safe_id(&tid) || !tid.starts_with("fp_") {
                return;
            }
            let offers = AGENT_OFFERS.lock();
            let offer = offers.iter().find(|o| o.from == from && o.offer_id == offer_id);
            let entry = offer.and_then(|o| o.files.get(index)).filter(|f| !f.dir);
            let (Some(offer), Some(entry)) = (offer, entry) else {
                logger::warn("clipbrd.capture", "Clipboard helper asked for an unknown offer entry; refused.");
                send(&ToHelper::End { tid, ok: false });
                return;
            };
            let Route::Local(tx_out) = &offer.route else { return };
            logger::info(
                "app.ft_sink_session",
                &format!("Helper pastes {} from offer {offer_id} ({tid}).", entry.name),
            );
            let (name, size, out) = (entry.name.clone(), entry.size, SinkOut::Helper(tid.clone()));
            let sink = Sink { from: from.clone(), name, size, received: 0, hash: Sha256::new(), out };
            with_sinks(|sinks| sinks.insert(tid.clone(), sink));
            let pull = FilePayload::pull(&offer_id, entry.remote as u64, &tid);
            let _ = tx_out.send(Outgoing::Message(ClientMessage::file(&from, pull)));
        }
        ToAgent::Close { tid } => {
            with_sinks(|sinks| sinks.remove(&tid));
        }
        // Not clipboard work: the helper's pipe also carries focus readings.
        ToAgent::TextFocus(reading) => crate::infra::text_focus::from_helper(reading),
    }
}

/// Agent: the helper is gone; its paste streams and offers went with it.
pub fn helper_lost() {
    // Running pastes died with the helper, but the offers are still the partner's: they come back with the next one.
    with_sinks(|sinks| sinks.retain(|_, s| matches!(s.out, SinkOut::Local(_))));
}

/// Agent: a helper connected; it gets the last offer back if the partner still holds it.
pub fn helper_connected() {
    let last = LAST_SET.lock().clone();
    let Some((from, offer_id, files, _)) = last.filter(|l| l.3.elapsed() < RESTORE_WITHIN) else { return };
    if !AGENT_OFFERS.lock().iter().any(|o| o.from == from && o.offer_id == offer_id) {
        return;
    }
    logger::info("clipbrd.capture", &format!("New clipboard helper: putting offer {offer_id} back on the clipboard."));
    crate::infra::clipboard_helper::send(&ToHelper::Set { from, offer_id, files, drop: false });
}

/// Helper: a command from the agent.
pub fn helper_command(msg: ToHelper) {
    match msg {
        ToHelper::Set { from, offer_id, files, drop } => match parse_offer_files(&files) {
            Ok(files) if !files.is_empty() => {
                let offer = Offer { from, offer_id, files, route: Route::Agent, prefetch: Default::default() };
                post(if drop { Cmd::Drop(offer) } else { Cmd::Set(offer) })
            }
            _ => logger::warn("clipbrd.capture", "Agent sent an unusable offer; ignored."),
        },
        ToHelper::Clear { partner } => post(Cmd::Clear(partner)),
        ToHelper::End { tid, ok } => {
            if let Some(tx) = helper_streams(|m| m.get(&tid).cloned()) {
                let _ = tx.send(SinkMsg::End(ok));
            }
        }
        // Not clipboard work: the helper also watches keyboard focus.
        ToHelper::TextFocus { on } => crate::infra::text_focus::helper_watch(on),
    }
}

/// Helper: verified bytes for paste stream `tid`.
pub fn helper_data(tid: &str, bytes: Vec<u8>) {
    if let Some(tx) = helper_streams(|m| m.get(tid).cloned()) {
        let _ = tx.send(SinkMsg::Data(bytes));
    }
}
