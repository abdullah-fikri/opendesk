//! Mouse and keyboard injection (SendInput).
//!
//! The pointer moves with absolute `SendInput` mouse input over the whole virtual desktop, and a button press or
//! release goes in one batch with a move to its position. `SetCursorPos` is not mouse input: drag tracking in
//! Windows Terminal, the window move/resize loop and Chromium never saw the pointer move while a button was held.
//! `OPENDESK_INPUT_ABSOLUTE=0` restores `SetCursorPos` and position-less buttons.
//!
//! Keys go through SendInput with the scan code and extended-key flag a physical keyboard would produce, so
//! applications that read scan codes (games, terminals, remote desktops) and shell shortcuts (Win, Win+key)
//! see a real key press. A combination is sent as one batch so local typing cannot interleave with it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use super::{LAST_INJECTED_POS, pack_pos};

type BOOL = i32;
type DWORD = u32;
type WORD = u16;
type UINT = u32;
#[allow(non_camel_case_types)]
type ULONG_PTR = usize;
type BYTE = u8;

const MOUSEEVENTF_MOVE: DWORD = 0x0001;
const MOUSEEVENTF_LEFTDOWN: DWORD = 0x0002;
const MOUSEEVENTF_LEFTUP: DWORD = 0x0004;
const MOUSEEVENTF_RIGHTDOWN: DWORD = 0x0008;
const MOUSEEVENTF_RIGHTUP: DWORD = 0x0010;
const MOUSEEVENTF_WHEEL: DWORD = 0x0800;
const MOUSEEVENTF_VIRTUALDESK: DWORD = 0x4000;
const MOUSEEVENTF_ABSOLUTE: DWORD = 0x8000;

const INPUT_MOUSE: DWORD = 0;
const INPUT_KEYBOARD: DWORD = 1;
const KEYEVENTF_EXTENDEDKEY: DWORD = 0x0001;
const KEYEVENTF_KEYUP: DWORD = 0x0002;
const KEYEVENTF_UNICODE: DWORD = 0x0004;

const VK_SHIFT: BYTE = 0x10;
const VK_CONTROL: BYTE = 0x11;
const VK_MENU: BYTE = 0x12; // ALT
const MAPVK_VK_TO_VSC_EX: UINT = 4;

const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;

/// Largest coordinate of absolute mouse input; Windows maps 0..=65535 onto the virtual desktop.
const ABSOLUTE_MAX: i64 = 65535;

/// Win32 error of the last injection Windows refused; 0 when none since [`take_injection_error`].
static LAST_INJECTION_ERROR: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
#[derive(Copy, Clone)]
struct KEYBDINPUT {
    w_vk: WORD,
    w_scan: WORD,
    dw_flags: DWORD,
    time: DWORD,
    dw_extra_info: ULONG_PTR,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct MOUSEINPUT {
    dx: i32,
    dy: i32,
    mouse_data: DWORD,
    dw_flags: DWORD,
    time: DWORD,
    dw_extra_info: ULONG_PTR,
}

/// The payload of `INPUT`; the union gives `INPUT` the size Windows checks against `cbSize`.
#[repr(C)]
#[derive(Copy, Clone)]
union INPUT_DATA {
    mi: MOUSEINPUT,
    ki: KEYBDINPUT,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct INPUT {
    type_: DWORD,
    data: INPUT_DATA,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SetCursorPos(x: i32, y: i32) -> BOOL;
    fn mouse_event(dw_flags: DWORD, dx: DWORD, dy: DWORD, dw_data: DWORD, dw_extra_info: ULONG_PTR);
    fn SendInput(c_inputs: UINT, p_inputs: *const INPUT, cb_size: i32) -> UINT;
    fn MapVirtualKeyW(u_code: UINT, u_map_type: UINT) -> UINT;
    fn GetSystemMetrics(n_index: i32) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> DWORD;
}

/// Returns and clears the error of the last refused keyboard or mouse injection (0 = all were accepted). Windows
/// refuses input while another desktop receives input; input meant for a window of higher integrity (UIPI) is
/// dropped without an error.
pub fn take_injection_error() -> u32 {
    LAST_INJECTION_ERROR.swap(0, Ordering::Relaxed)
}

/// Keys whose scan code carries the E0 prefix; Windows does not always report it for these.
fn is_extended(vk: BYTE) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2C..=0x2E | 0x5B..=0x5D | 0x6F | 0x90 | 0xA3 | 0xA5)
}

/// One key press or release with its scan code, as the physical key would send it.
fn key_input(vk: BYTE, up: bool) -> INPUT {
    let sc = unsafe { MapVirtualKeyW(vk as UINT, MAPVK_VK_TO_VSC_EX) };
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if sc & 0xFF00 == 0xE000 || is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        type_: INPUT_KEYBOARD,
        data: INPUT_DATA {
            ki: KEYBDINPUT {
                w_vk: vk as WORD,
                w_scan: (sc & 0xFF) as WORD,
                dw_flags: flags,
                time: 0,
                dw_extra_info: 0,
            },
        },
    }
}

/// One mouse input with `flags`; `dx`, `dy` are absolute coordinates when the flags carry a position, else 0.
fn mouse_input(flags: DWORD, dx: i32, dy: i32) -> INPUT {
    INPUT {
        type_: INPUT_MOUSE,
        data: INPUT_DATA { mi: MOUSEINPUT { dx, dy, mouse_data: 0, dw_flags: flags, time: 0, dw_extra_info: 0 } },
    }
}

/// False when `OPENDESK_INPUT_ABSOLUTE=0` asks for the old `SetCursorPos` injection; read once.
fn absolute_input() -> bool {
    static ABSOLUTE: OnceLock<bool> = OnceLock::new();
    *ABSOLUTE.get_or_init(|| std::env::var("OPENDESK_INPUT_ABSOLUTE").map(|v| v != "0").unwrap_or(true))
}

/// Maps pixel `pos` onto the 0..=65535 absolute range of an axis that starts at `origin` and is `size` pixels
/// long; positions off the axis are clamped onto its edge. Windows turns an absolute value `n` back into pixel
/// `origin + n * size / 65536` (rounded down), so this returns the smallest `n` that lands exactly on `pos`.
fn to_absolute(pos: i32, origin: i32, size: i32) -> i32 {
    let size = i64::from(size.max(1));
    let offset = (i64::from(pos) - i64::from(origin)).clamp(0, size - 1);
    ((offset * 65536 + size - 1) / size).min(ABSOLUTE_MAX) as i32
}

/// Absolute move to (`x`, `y`) in virtual-desktop pixels. The process is per-monitor DPI aware (see
/// `capture::enable_per_monitor_dpi_awareness`), so these and the system metrics are physical pixels, the same
/// space `SetCursorPos` and the capture's monitor bounds use.
fn absolute_move(x: i32, y: i32) -> INPUT {
    // SAFETY: GetSystemMetrics only reads system state.
    let (mut vx, mut vy, mut vw, mut vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if vw <= 0 || vh <= 0 {
        // SAFETY: as above.
        (vx, vy, vw, vh) = unsafe { (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    }
    mouse_input(
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        to_absolute(x, vx, vw),
        to_absolute(y, vy, vh),
    )
}

/// The flag that presses (`up` false) or releases `button` (0 left, 2 right); None for other buttons.
fn button_flag(button: i32, up: bool) -> Option<DWORD> {
    match (button, up) {
        (0, false) => Some(MOUSEEVENTF_LEFTDOWN),
        (0, true) => Some(MOUSEEVENTF_LEFTUP),
        (2, false) => Some(MOUSEEVENTF_RIGHTDOWN),
        (2, true) => Some(MOUSEEVENTF_RIGHTUP),
        _ => None,
    }
}

/// Presses or releases `button`. On the absolute path the button goes in one batch with a move to the last
/// injected position, so it lands where the viewer pointed even if the local mouse moved in between, and the end
/// of a drag is a real move with the button still held.
fn press(button: i32, up: bool) {
    let Some(flag) = button_flag(button, up) else { return };
    if !absolute_input() {
        // SAFETY: mouse_event takes plain values.
        unsafe { mouse_event(flag, 0, 0, 0, 0) };
        return;
    }
    let packed = LAST_INJECTED_POS.load(Ordering::Relaxed);
    if packed == i64::MIN {
        send(&[mouse_input(flag, 0, 0)]);
    } else {
        send(&[absolute_move((packed >> 32) as i32, packed as i32), mouse_input(flag, 0, 0)]);
    }
}

/// Injects `inputs` in one batch and records a refusal for [`take_injection_error`]. Returns false if Windows
/// refused part of it.
fn send(inputs: &[INPUT]) -> bool {
    if inputs.is_empty() {
        return true;
    }
    let sent = unsafe { SendInput(inputs.len() as UINT, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if (sent as usize) < inputs.len() {
        LAST_INJECTION_ERROR.store(unsafe { GetLastError() }.max(1), Ordering::Relaxed);
        return false;
    }
    true
}

/// Injects mouse and keyboard input into the current input desktop.
pub struct InputInjector;

impl Default for InputInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInjector {
    /// Stateless; cheap to create.
    pub fn new() -> Self {
        Self
    }

    /// Moves the pointer to (`x`, `y`) in virtual-desktop coordinates (physical pixels).
    pub fn set_cursor_pos(&self, x: i32, y: i32) {
        LAST_INJECTED_POS.store(pack_pos(x, y), Ordering::Relaxed);
        if absolute_input() {
            // A refused move (another desktop receives input) still places the cursor the old way.
            if !send(&[absolute_move(x, y)]) {
                // SAFETY: SetCursorPos takes plain values.
                unsafe { SetCursorPos(x, y) };
            }
            return;
        }
        unsafe {
            SetCursorPos(x, y);
            // SetCursorPos is no mouse input, so a cursor Windows hid for keyboard use (Start, search, Alt+Tab,
            // "hide pointer while typing") stayed hidden. A zero move is mouse input that leaves it in place.
            mouse_event(MOUSEEVENTF_MOVE, 0, 0, 0, 0);
        }
    }

    /// Presses mouse button `button` at the last position given to [`Self::set_cursor_pos`]: 0 left, 2 right;
    /// other values are ignored.
    pub fn mouse_down(&self, button: i32) {
        press(button, false);
    }

    /// Releases mouse button `button` at the last position given to [`Self::set_cursor_pos`]: 0 left, 2 right;
    /// other values are ignored.
    pub fn mouse_up(&self, button: i32) {
        press(button, true);
    }

    /// Scrolls by `delta_y` (120 = one notch up).
    pub fn mouse_wheel(&self, delta_y: i32) {
        unsafe {
            let win_delta = if delta_y.abs() < 40 { delta_y * 120 } else { delta_y };
            mouse_event(MOUSEEVENTF_WHEEL, 0, 0, win_delta as u32, 0);
        }
    }

    /// Presses virtual key `vk`.
    pub fn key_down(&self, vk: u8) {
        send(&[key_input(vk, false)]);
    }

    /// Releases virtual key `vk`.
    pub fn key_up(&self, vk: u8) {
        send(&[key_input(vk, true)]);
    }

    /// Presses and releases virtual key `vk`.
    pub fn key_tap(&self, vk: u8) {
        send(&[key_input(vk, false), key_input(vk, true)]);
    }

    /// Presses `vk` with the given modifiers held, then releases everything.
    pub fn send_key_combo(&self, ctrl: bool, alt: bool, shift: bool, vk: u8) {
        let mods: Vec<BYTE> =
            [(ctrl, VK_CONTROL), (alt, VK_MENU), (shift, VK_SHIFT)].iter().filter(|m| m.0).map(|m| m.1).collect();
        let mut inputs: Vec<INPUT> = mods.iter().map(|&m| key_input(m, false)).collect();
        if vk > 0 {
            inputs.extend([key_input(vk, false), key_input(vk, true)]);
        }
        inputs.extend(mods.iter().rev().map(|&m| key_input(m, true)));
        send(&inputs);
    }

    /// Types `text` as Unicode characters, independent of the keyboard layout.
    pub fn send_unicode_text(&self, text: &str) {
        let unicode = |unit: u16, up: bool| INPUT {
            type_: INPUT_KEYBOARD,
            data: INPUT_DATA {
                ki: KEYBDINPUT {
                    w_vk: 0,
                    w_scan: unit,
                    dw_flags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dw_extra_info: 0,
                },
            },
        };
        for unit in text.encode_utf16() {
            send(&[unicode(unit, false), unicode(unit, true)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_keys_carry_the_flag_and_a_scan_code() {
        for vk in [0x5B, 0x25, 0x2E, 0x24] {
            // Win, Left, Delete, Home
            // SAFETY: key_input fills the keyboard member.
            let i = unsafe { key_input(vk, false).data.ki };
            assert_ne!(i.dw_flags & KEYEVENTF_EXTENDEDKEY, 0, "vk {vk:#x}");
            assert_ne!(i.w_scan, 0, "vk {vk:#x}");
        }
        // SAFETY: as above.
        let a = unsafe { key_input(b'A', true).data.ki };
        assert_eq!(a.dw_flags, KEYEVENTF_KEYUP);
        assert_eq!(a.w_scan, 0x1E);
    }

    #[test]
    fn input_has_the_size_windows_expects() {
        let expected = if cfg!(target_pointer_width = "64") { 40 } else { 28 };
        assert_eq!(std::mem::size_of::<INPUT>(), expected);
    }

    #[test]
    fn absolute_coordinates_cover_the_edges() {
        assert_eq!(to_absolute(0, 0, 1920), 0);
        assert_eq!(to_absolute(1919, 0, 1920), 65502);
        assert_eq!(to_absolute(-5, 0, 1920), 0, "left of the desktop clamps to its edge");
        assert_eq!(to_absolute(5000, 0, 1920), 65502, "right of the desktop clamps to its last pixel");
        assert_eq!(to_absolute(0, 0, 1), 0);
        assert_eq!(to_absolute(0, 0, 0), 0, "an empty axis does not divide by zero");
        assert_eq!(to_absolute(65535, 0, 65536), 65535);
    }

    #[test]
    fn absolute_coordinates_handle_a_negative_origin() {
        // A left monitor at -1920 and the primary at 0: the virtual desktop is 3840 wide from -1920.
        assert_eq!(to_absolute(-1920, -1920, 3840), 0);
        assert_eq!(to_absolute(0, -1920, 3840), 32768, "the primary's left edge sits in the middle");
        assert_eq!(to_absolute(-1, -1920, 3840), 32751);
    }

    #[test]
    fn absolute_coordinates_land_on_the_exact_pixel() {
        // Windows maps n back to origin + n * size / 65536 (rounded down); every pixel must survive the trip.
        for (origin, size) in [(0, 1920), (-1920, 3840), (-1280, 2560 + 1366), (0, 7680), (-3, 7), (0, 65536)] {
            for pos in origin..origin + size {
                let n = i64::from(to_absolute(pos, origin, size));
                assert!((0..=ABSOLUTE_MAX).contains(&n));
                assert_eq!(i64::from(origin) + n * i64::from(size) / 65536, i64::from(pos), "{pos} on {origin}+{size}");
            }
        }
    }

    #[test]
    fn only_left_and_right_buttons_map_to_flags() {
        assert_eq!(button_flag(0, false), Some(MOUSEEVENTF_LEFTDOWN));
        assert_eq!(button_flag(0, true), Some(MOUSEEVENTF_LEFTUP));
        assert_eq!(button_flag(2, false), Some(MOUSEEVENTF_RIGHTDOWN));
        assert_eq!(button_flag(2, true), Some(MOUSEEVENTF_RIGHTUP));
        assert_eq!(button_flag(1, false), None);
    }
}
