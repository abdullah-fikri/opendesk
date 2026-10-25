//! Host privacy screen, local input lock and the viewer's system-key hook. Backend selected at compile time;
//! outside Windows these are unavailable and hosts say so through the `host-features` capability.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;

/// True when the viewer forwards system keys (Win, Alt+Tab, Ctrl+Shift+Esc, ...) through a keyboard hook
/// (Windows). Elsewhere the local desktop keeps them and the toolbar offers a shortcut menu.
pub const SYSTEM_KEY_HOOK: bool = cfg!(windows);
