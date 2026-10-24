//! Mouse and keyboard injection. The platform backend is chosen at compile time.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;
