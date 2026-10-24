//! JPEG encoding. The platform backend is chosen at compile time: GDI+ on Windows, a pure-Rust encoder elsewhere.

#[cfg(windows)]
mod gdiplus;
#[cfg(windows)]
pub use gdiplus::*;

#[cfg(not(windows))]
mod portable;
#[cfg(not(windows))]
pub use portable::*;
