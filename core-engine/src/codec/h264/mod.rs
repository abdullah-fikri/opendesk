//! H.264 encoding and decoding. The platform backend is chosen at compile time.

#[cfg(windows)]
mod mf;
#[cfg(windows)]
pub use mf::*;
