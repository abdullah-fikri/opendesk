//! Outside Windows there is no privacy overlay, no local input lock and no low-level keyboard hook. Hosts
//! advertise `host-features` without `privacy-screen` / `block-input`, so viewers hide those controls; requests
//! that arrive anyway (old viewers) are logged and ignored.

use std::sync::mpsc::Sender;

use crate::infra::logger;
use crate::protocol::Outgoing;

/// Nothing to install.
pub fn init_privacy_and_hooks(_tx_out: Sender<Outgoing>) {}

/// System keys (Super, Alt+Tab) stay with the local desktop while viewing.
pub fn set_viewer_hook_state(_active: bool, _partner_clean: &str) {}

/// Privacy screens are not supported here.
pub fn set_host_privacy_mode(mode: i32) {
    if mode != 0 {
        logger::warn("privacy", "The viewer asked for a privacy screen; not supported on this platform.");
    }
}

/// Locking local input is not supported here.
pub fn set_host_block_input(blocked: bool) {
    if blocked {
        logger::warn("privacy", "The viewer asked to lock local input; not supported on this platform.");
    }
}

/// Nothing was changed, so nothing to undo.
pub fn reset_host_privacy() {}
