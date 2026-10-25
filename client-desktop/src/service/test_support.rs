//! Test helpers for the service layer.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use crate::domain::edition::Edition;
use crate::infra::audio::VoiceEngine;
use crate::protocol::Outgoing;
use crate::service::context::{AppContext, Ctx};
use crate::service::ui_port::NoUi;

/// A context without UI whose outbox the test can read.
pub fn context_with_outbox() -> (Ctx, Receiver<Outgoing>) {
    let (tx, rx) = mpsc::channel();
    let voice = VoiceEngine::new(tx.clone());
    let ctx =
        AppContext::new(Edition::Standard, "111111111".into(), "ws://127.0.0.1:8088".into(), tx, Arc::new(NoUi), voice);
    (Arc::new(ctx), rx)
}

/// A context without UI; its outbox is dropped.
pub fn context() -> Ctx {
    context_with_outbox().0
}
