//! Platform backend selected at compile time.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;
