//! Tells a viewer listing `text-focus` when keyboard focus on this machine enters or leaves a text field, so a
//! touch viewer raises its keyboard by itself. Runs beside the capture loop (UI Automation may block on a hung
//! application; the video must not).
//!
//! Where focus is read: as the service's SYSTEM agent, the user helper watches it (it has the user's token and
//! session) and reports changes; with no helper (sign-in screen) the agent reads the caret on the input desktop.
//! As a plain application, this thread reads it directly.

use std::time::Instant;

use crate::domain::text_focus::{FocusTracker, TextFocus, normalize};
use crate::infra::text_focus::{POLL, Probe};
use crate::infra::{logger, thread};
use crate::protocol::InputEvent;
use crate::service::context::Ctx;

/// Starts watching for the capture generation `generation`, if the viewer wants it.
pub(super) fn spawn(ctx: Ctx, partner: String, generation: u64) {
    if !ctx.host.peer_caps.lock().text_focus {
        return;
    }
    thread::spawn_named("text-focus", move || {
        logger::info("host.text_focus", "Watching keyboard focus for the viewer's on-screen keyboard.");
        let mut source = Source::new();
        let mut tracker = FocusTracker::default();
        while ctx.host.is_current(generation) {
            let reading = source.read();
            let b = *ctx.host.monitor_bounds.lock();
            if let Some(state) = tracker.update(Instant::now(), normalize(reading, b.left, b.top, b.width, b.height)) {
                ctx.send_input(&partner, event(state));
            }
            std::thread::sleep(POLL);
        }
        source.stop();
    });
}

fn event(s: TextFocus) -> InputEvent {
    InputEvent::TextFocus {
        editable: s.editable,
        norm_x: s.norm_x,
        norm_y: s.norm_y,
        norm_w: s.norm_w,
        norm_h: s.norm_h,
        password: s.password,
    }
}

/// Where readings come from (see the module docs).
struct Source {
    /// A UI Automation reader (plain application) or a caret reader (SYSTEM agent without its helper).
    probe: Option<Probe>,
    #[cfg(windows)]
    agent: bool,
    #[cfg(windows)]
    last_ask: Option<Instant>,
}

impl Source {
    fn new() -> Self {
        #[cfg(windows)]
        {
            let agent = crate::infra::system_service::is_local_system();
            if agent {
                crate::infra::text_focus::reset_helper_reading();
            }
            Self { probe: (!agent).then(|| Probe::new(true)), agent, last_ask: None }
        }
        #[cfg(not(windows))]
        Self { probe: Some(Probe::new(true)) }
    }

    fn read(&mut self) -> crate::domain::text_focus::FocusReading {
        #[cfg(windows)]
        if self.agent {
            use crate::infra::clipboard_helper::{self, ToHelper};
            use crate::infra::text_focus;
            if clipboard_helper::ready() {
                if self.last_ask.is_none_or(|t| t.elapsed() >= text_focus::KEEPALIVE) {
                    self.last_ask = Some(Instant::now());
                    clipboard_helper::send(&ToHelper::TextFocus { on: true });
                }
                return text_focus::helper_reading().unwrap_or_default();
            }
            self.last_ask = None;
            // Caret only: it creates no windows, so the thread can follow the input desktop.
            core_engine::desktop::switch_to_input_desktop();
            return self.probe.get_or_insert_with(|| Probe::new(false)).read();
        }
        self.probe.get_or_insert_with(|| Probe::new(true)).read()
    }

    fn stop(&mut self) {
        #[cfg(windows)]
        if self.agent && self.last_ask.is_some() {
            crate::infra::clipboard_helper::send(&crate::infra::clipboard_helper::ToHelper::TextFocus { on: false });
        }
    }
}
