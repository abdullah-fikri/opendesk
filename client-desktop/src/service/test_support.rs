//! Test helpers for the service layer.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use parking_lot::Mutex;

use crate::domain::edition::Edition;
use crate::domain::history::HistoryRecord;
use crate::infra::audio::VoiceEngine;
use crate::protocol::Outgoing;
use crate::service::context::{AppContext, Ctx};
use crate::service::ui_port::{Frame, NoUi, RelayStatus, RemoteHost, SessionChange, UiPort};

/// A context reporting to `ui` whose outbox the test can read.
pub fn context_with_ui(ui: Arc<dyn UiPort>) -> (Ctx, Receiver<Outgoing>) {
    let (tx, rx) = mpsc::channel();
    let voice = VoiceEngine::new(tx.clone());
    let ctx = AppContext::new(Edition::Standard, "111111111".into(), "ws://127.0.0.1:8088".into(), tx, ui, voice);
    (Arc::new(ctx), rx)
}

/// A context without UI whose outbox the test can read.
pub fn context_with_outbox() -> (Ctx, Receiver<Outgoing>) {
    context_with_ui(Arc::new(NoUi))
}

/// A context without UI; its outbox is dropped.
pub fn context() -> Ctx {
    context_with_outbox().0
}

/// A UI that records the microphone states the services show: `("hosting", muted)` for the host panel and
/// `(session ID, muted)` for a viewer session's toolbar.
#[derive(Default)]
pub struct MicRecorder {
    pub shown: Mutex<Vec<(String, bool)>>,
}

impl UiPort for MicRecorder {
    fn status(&self, _: String) {}
    fn relay_status(&self, _: RelayStatus) {}
    fn gateway_changed(&self, _: String) {}
    fn host_list(&self, _: Vec<RemoteHost>) {}
    fn history(&self, _: Vec<HistoryRecord>) {}
    fn hosting_started(&self, _: String) {}
    fn hosting_ended(&self) {}
    fn hosting_mic(&self, muted: bool) {
        self.shown.lock().push(("hosting".into(), muted));
    }
    fn session_opened(&self, _: String, _: String, _: String, _: i32, _: i32) {}
    fn session_connected(&self, _: String, _: String) {}
    fn session_frame(&self, _: String, _: Frame) {}
    fn session_changed(&self, id: String, change: SessionChange) {
        if let SessionChange::MicMuted(muted) = change {
            self.shown.lock().push((id, muted));
        }
    }
    fn session_closed(&self, _: String, _: Option<String>) {}
    fn all_sessions_closed(&self) {}
    fn focus_changed(&self, _: String) {}
    fn layout_changed(&self, _: i32) {}
}
