//! Windows service that runs OpenDesk as SYSTEM in the logged-on user's session, so hosting reaches the UAC
//! prompt and the lock screen. Windows only; other platforms have no secure desktop.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;
