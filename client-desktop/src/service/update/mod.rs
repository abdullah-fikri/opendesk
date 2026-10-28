//! Auto-update with automatic and manual rollback (Windows, run by the SYSTEM service). See
//! `docs/auto-update.md` at the repository root.
//!
//! - [`manifest`]: the signed release manifest and the install decision (pure)

// The pure parts build everywhere and are tested everywhere; only Windows runs them.
#![cfg_attr(not(windows), allow(dead_code))]
// Not wired into the service yet.
#![allow(dead_code)]

pub mod manifest;
