//! JPEG encoding. The platform backend is chosen at compile time.

#[cfg(windows)]
mod gdiplus;
#[cfg(windows)]
pub use gdiplus::*;
