//! Native window helpers: hiding to the tray, file drop and the file picker. Backend selected at compile time.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;

/// True when [`hide_window_by_title`] hides the native window itself (Windows). Elsewhere Slint hides it.
pub const NATIVE_HIDE: bool = cfg!(windows);

/// True when files dropped on the window can be sent (Windows OLE drop target).
pub const FILE_DROP_SUPPORTED: bool = cfg!(windows);
