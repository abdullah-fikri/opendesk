//! OpenDesk desktop client: remote desktop host and viewer for Windows and Linux.
//!
//! Layers (see `docs/architecture.md`), each depending only on the ones listed after it:
//! - [`ui`]: Slint window, tray and callbacks (presentation)
//! - [`service`]: use cases: relay connection, viewer and host roles, file transfer, clipboard
//! - [`infra`]: OS (Windows, Linux), file system and network adapters
//! - [`protocol`]: wire messages (DTOs)
//! - [`domain`]: types and rules without I/O

#![windows_subsystem = "windows"]
// Win32 FFI types keep their Windows SDK names (HWND, BOOL, ...) so they can be checked against the docs.
#![allow(clippy::upper_case_acronyms)]

mod app;
mod cli;
mod domain;
mod infra;
mod protocol;
mod service;
mod ui;

fn main() -> Result<(), slint::PlatformError> {
    infra::logger::install_panic_hook();
    cli::run_if_requested();
    app::run()
}
