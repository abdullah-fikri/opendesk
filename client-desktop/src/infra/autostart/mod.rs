//! Process lifecycle: single instance, start at login, stopping and the console output of the CLI. Backend
//! selected at compile time.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;
