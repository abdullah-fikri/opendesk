//! OpenDesk core engine: the Windows-only building blocks shared by the desktop client.
//!
//! - [`capture`]: DXGI Desktop Duplication with GDI fallback, monitor enumeration, cursor shape.
//! - [`diff`]: block-level change detection between frames.
//! - [`codec`]: framed key/delta video, H.264 (Media Foundation), JPEG (GDI+), base64.
//! - [`stream`]: the per-session video encoder that picks between those codecs.
//! - [`input`]: mouse and keyboard injection.
//! - [`clipboard`]: clipboard text and file lists.
//! - [`desktop`]: following the input desktop (Winlogon, UAC) for capture threads.
//!
//! Nothing here knows about the network, the UI or the session protocol.

// Win32 FFI types keep their Windows SDK names (HWND, BOOL, ...) so they can be checked against the docs.
#![allow(clippy::upper_case_acronyms)]
#![warn(missing_docs)]

pub mod capture;
pub mod clipboard;
pub mod codec;
pub mod desktop;
pub mod diff;
pub mod input;
pub mod stream;
