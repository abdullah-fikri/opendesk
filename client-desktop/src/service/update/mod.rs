//! Auto-update with automatic and manual rollback (Windows, run by the SYSTEM service). See
//! `docs/auto-update.md` at the repository root.
//!
//! - [`manifest`]: the signed release manifest and the install decision (pure)
//! - [`health`]: health check after an update and the automatic rollback (pure state machine)
//! - [`store`]: files of the update directory
//! - [`fetch`]: HTTPS download and verification of manifests and installers
//! - `secure_dir`, `runner`, `agent` (Windows): the protected directory, the service side and the agent's
//!   heartbeat

// The pure parts build everywhere and are tested everywhere; only Windows runs them.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
pub mod agent;
pub mod fetch;
pub mod health;
pub mod manifest;
#[cfg(windows)]
pub mod runner;
#[cfg(windows)]
pub mod secure_dir;
pub mod store;

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// False when `OPENDESK_AUTO_UPDATE` is `0`, `false` or `off`.
pub fn auto_update_env() -> bool {
    std::env::var("OPENDESK_AUTO_UPDATE")
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off"))
        .unwrap_or(true)
}

/// `OPENDESK_UPDATE_URL`, when set.
pub fn update_url_env() -> Option<String> {
    std::env::var("OPENDESK_UPDATE_URL").ok().filter(|u| !u.trim().is_empty())
}
