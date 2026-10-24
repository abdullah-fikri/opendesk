//! Clipboard text and file lists. The platform backend is chosen at compile time.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;
