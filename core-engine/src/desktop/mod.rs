//! Following the input desktop for capture threads. The platform backend is chosen at compile time.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;
