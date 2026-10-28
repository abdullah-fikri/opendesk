//! Viewer role: this machine controls up to four remote hosts.
//!
//! - [`registry`]: open sessions, focus and layout.
//! - [`decoder`]: per-session video decoding.
//! - [`controller`]: everything the user does with a session.
//! - [`stats`]: the session stats overlay.

pub mod controller;
pub mod decoder;
pub mod registry;
pub mod stats;

pub use registry::Registry;

#[cfg(test)]
mod relay_e2e;
