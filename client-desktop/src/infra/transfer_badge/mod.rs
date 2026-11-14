//! A small always-on-top line in the corner of the host's screen ("Receiving 2 files... 45%", "Pasting into
//! Documents"), so the person at a host without a window (headless edition, the service's agent, a minimized
//! app) sees files arrive. It never takes focus or clicks, and screen capture includes it, so the viewer sees
//! it too. Backend selected at compile time: a Win32 layered window, or nothing elsewhere.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;
