//! Screen capture. The platform backend is chosen at compile time; see [`DesktopCapturer`].

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;
