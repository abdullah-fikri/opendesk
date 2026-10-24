//! Mouse and keyboard injection. [`InputInjector`] is the platform backend chosen at compile time: SendInput on
//! Windows, XTEST elsewhere. Key codes are Win32 virtual-key codes on every platform (the wire format).

use std::sync::atomic::AtomicI64;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(any(not(windows), test))]
mod keysym;
#[cfg(not(windows))]
mod x11;
#[cfg(not(windows))]
pub use x11::*;

/// Screen position the remote viewer last moved the cursor to; lets capture tell viewer moves from local host moves.
pub static LAST_INJECTED_POS: AtomicI64 = AtomicI64::new(i64::MIN);

/// Packs a screen position into one atomic value (see `LAST_INJECTED_POS`).
pub fn pack_pos(x: i32, y: i32) -> i64 {
    ((x as i64) << 32) | (y as u32 as i64)
}
