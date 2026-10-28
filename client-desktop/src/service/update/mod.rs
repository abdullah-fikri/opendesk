//! Auto-update with automatic and manual rollback (Windows, run by the SYSTEM service). See
//! `docs/auto-update.md` at the repository root.
//!
//! - [`manifest`]: the signed release manifest and the install decision (pure)
//! - [`health`]: health check after an update and the automatic rollback (pure state machine)
//! - [`store`]: files of the update directory
//! - [`fetch`]: HTTPS download and verification of manifests and installers

// The pure parts build everywhere and are tested everywhere; only Windows runs them.
#![cfg_attr(not(windows), allow(dead_code))]
// Not wired into the service yet.
#![allow(dead_code)]

pub mod fetch;
pub mod health;
pub mod manifest;
pub mod store;
