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

/// Whole wheel notches to send for a wire `delta` (120 = one notch), keeping the fraction in `rest` for the next
/// event. Values under 40 are notch counts (older viewers); 40 and more are Windows wheel units, so a phone that
/// scrolls a line (40) at a time moves one notch every third event instead of one per event.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn wheel_notches(rest: &mut i32, delta: i32) -> i32 {
    if delta.abs() < 40 {
        return delta;
    }
    if (*rest > 0) != (delta > 0) {
        *rest = 0;
    }
    *rest += delta;
    let notches = *rest / 120;
    *rest -= notches * 120;
    notches
}

#[cfg(test)]
mod tests {
    use super::wheel_notches;

    #[test]
    fn wheel_units_accumulate_into_notches() {
        let mut rest = 0;
        assert_eq!(wheel_notches(&mut rest, 3), 3);
        assert_eq!(wheel_notches(&mut rest, 120), 1);
        assert_eq!([40, 40, 40].map(|d| wheel_notches(&mut rest, d)), [0, 0, 1]);
        assert_eq!(wheel_notches(&mut rest, 80), 0);
        // Turning around drops the fraction left from the other direction.
        assert_eq!(wheel_notches(&mut rest, -40), 0);
        assert_eq!(wheel_notches(&mut rest, -80), -1);
        assert_eq!(wheel_notches(&mut rest, -360), -3);
    }
}
