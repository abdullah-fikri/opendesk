//! OpenDesk core engine: the platform building blocks shared by the desktop client.
//!
//! - [`capture`]: screen capture, monitor enumeration, cursor shape. Windows: DXGI Desktop Duplication with GDI
//!   fallback. Linux: X11 (MIT-SHM, RandR, XFixes); Wayland sessions report that they cannot host.
//! - [`diff`]: block-level change detection between frames.
//! - [`codec`]: framed key/delta video, H.264 (Media Foundation on Windows, Cisco OpenH264 when installed on
//!   Linux), JPEG (GDI+ on Windows, pure Rust elsewhere), base64.
//! - [`stream`]: the per-session video encoder that picks between those codecs.
//! - [`input`]: mouse and keyboard injection (SendInput / XTEST).
//! - [`clipboard`]: clipboard text and file lists.
//! - [`desktop`]: following the input desktop (Winlogon, UAC) for capture threads; a no-op elsewhere.
//!
//! Every platform backend lives in a submodule selected with `cfg` and exposes the same functions and types,
//! so callers are compiled against one concrete implementation (static dispatch, no trait objects).
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
