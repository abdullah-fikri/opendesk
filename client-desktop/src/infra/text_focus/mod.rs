//! Reads where keyboard focus is on this machine and whether it takes typed text (`text-focus`, see
//! `domain::text_focus`). Windows: UI Automation, with the caret of classic Win32 controls as a fallback. Other
//! systems report nothing focused (the capability is not used there yet).
//!
//! The service's SYSTEM agent asks its user helper (`infra::clipboard_helper`) to watch focus: UI Automation
//! needs the user's own token to see into every application. The helper reports each change back over its pipe
//! ([`from_helper`]); without a helper (nobody signed in: sign-in screen) the agent reads the caret itself.

pub use crate::domain::text_focus::FocusReading;

#[cfg(windows)]
mod win;

/// How often focus is read.
pub const POLL: std::time::Duration = std::time::Duration::from_millis(150);

/// A focus reader; it holds COM objects, so it stays on the thread that made it.
pub struct Probe {
    #[cfg(windows)]
    inner: win::Probe,
}

impl Probe {
    /// `uia`: use UI Automation (a thread that never switches desktops); without it only the caret is read,
    /// which keeps the thread free to follow the input desktop.
    pub fn new(uia: bool) -> Self {
        #[cfg(not(windows))]
        let _ = uia;
        Self {
            #[cfg(windows)]
            inner: win::Probe::new(uia),
        }
    }

    /// The current focus.
    pub fn read(&mut self) -> FocusReading {
        #[cfg(windows)]
        return self.inner.read();
        #[cfg(not(windows))]
        FocusReading::default()
    }
}

#[cfg(windows)]
pub use win::{KEEPALIVE, from_helper, helper_reading, helper_watch, reset_helper_reading};
