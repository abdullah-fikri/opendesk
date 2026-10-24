//! System tray icon and its context menu (show, disconnect, reconnect, restart, exit). Backend selected at
//! compile time: a Win32 notification icon, or a freedesktop StatusNotifierItem elsewhere.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;

/// A tray menu choice.
#[derive(Clone, Copy, Debug)]
pub enum TrayAction {
    ShowWindow,
    DisconnectSession,
    ReconnectRelay,
    RestartApp,
    ExitApp,
}
