//! Mouse and keyboard injection (SendInput / mouse_event).
//!
//! Keys go through SendInput with the scan code and extended-key flag a physical keyboard would produce, so
//! applications that read scan codes (games, terminals, remote desktops) and shell shortcuts (Win, Win+key)
//! see a real key press. A combination is sent as one batch so local typing cannot interleave with it.

use std::sync::atomic::{AtomicU32, Ordering};

use super::{LAST_INJECTED_POS, pack_pos};

type BOOL = i32;
type DWORD = u32;
type WORD = u16;
type UINT = u32;
#[allow(non_camel_case_types)]
type ULONG_PTR = usize;
type BYTE = u8;

const MOUSEEVENTF_LEFTDOWN: DWORD = 0x0002;
const MOUSEEVENTF_LEFTUP: DWORD = 0x0004;
const MOUSEEVENTF_RIGHTDOWN: DWORD = 0x0008;
const MOUSEEVENTF_RIGHTUP: DWORD = 0x0010;
const MOUSEEVENTF_WHEEL: DWORD = 0x0800;

const INPUT_KEYBOARD: DWORD = 1;
const KEYEVENTF_EXTENDEDKEY: DWORD = 0x0001;
const KEYEVENTF_KEYUP: DWORD = 0x0002;
const KEYEVENTF_UNICODE: DWORD = 0x0004;

const VK_SHIFT: BYTE = 0x10;
const VK_CONTROL: BYTE = 0x11;
const VK_MENU: BYTE = 0x12; // ALT
const MAPVK_VK_TO_VSC_EX: UINT = 4;

/// Win32 error of the last keyboard injection Windows refused; 0 when none since [`take_injection_error`].
static LAST_KEY_ERROR: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
#[derive(Copy, Clone)]
struct KEYBDINPUT {
    w_vk: WORD,
    w_scan: WORD,
    dw_flags: DWORD,
    time: DWORD,
    dw_extra_info: ULONG_PTR,
    _pad: [u8; 8], // Pad union to match MOUSEINPUT size on 64-bit Windows (40 bytes total INPUT)
}

#[repr(C)]
#[derive(Copy, Clone)]
struct INPUT {
    type_: DWORD,
    ki: KEYBDINPUT,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SetCursorPos(x: i32, y: i32) -> BOOL;
    fn mouse_event(dw_flags: DWORD, dx: DWORD, dy: DWORD, dw_data: DWORD, dw_extra_info: ULONG_PTR);
    fn SendInput(c_inputs: UINT, p_inputs: *const INPUT, cb_size: i32) -> UINT;
    fn MapVirtualKeyW(u_code: UINT, u_map_type: UINT) -> UINT;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> DWORD;
}

/// Returns and clears the error of the last refused keyboard injection (0 = all were accepted). Windows
/// refuses input meant for a window of higher integrity, or while another desktop receives input.
pub fn take_injection_error() -> u32 {
    LAST_KEY_ERROR.swap(0, Ordering::Relaxed)
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
        ki: KEYBDINPUT {
            w_vk: vk as WORD,
            w_scan: (sc & 0xFF) as WORD,
            dw_flags: flags,
            time: 0,
            dw_extra_info: 0,
            _pad: [0; 8],
        },
    }
}

/// Injects `inputs` in one batch and records a refusal for [`take_injection_error`].
fn send(inputs: &[INPUT]) {
    if inputs.is_empty() {
        return;
    }
    let sent = unsafe { SendInput(inputs.len() as UINT, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if (sent as usize) < inputs.len() {
        LAST_KEY_ERROR.store(unsafe { GetLastError() }.max(1), Ordering::Relaxed);
    }
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

    /// Moves the pointer to (`x`, `y`) in virtual-desktop coordinates.
    pub fn set_cursor_pos(&self, x: i32, y: i32) {
        LAST_INJECTED_POS.store(pack_pos(x, y), Ordering::Relaxed);
        unsafe {
            SetCursorPos(x, y);
        }
    }

    /// Presses mouse button `button`: 0 left, 2 right; other values are ignored.
    pub fn mouse_down(&self, button: i32) {
        unsafe {
            if button == 0 {
                mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
            } else if button == 2 {
                mouse_event(MOUSEEVENTF_RIGHTDOWN, 0, 0, 0, 0);
            }
        }
    }

    /// Releases mouse button `button`: 0 left, 2 right; other values are ignored.
    pub fn mouse_up(&self, button: i32) {
        unsafe {
            if button == 0 {
                mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
            } else if button == 2 {
                mouse_event(MOUSEEVENTF_RIGHTUP, 0, 0, 0, 0);
            }
        }
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
            ki: KEYBDINPUT {
                w_vk: 0,
                w_scan: unit,
                dw_flags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dw_extra_info: 0,
                _pad: [0; 8],
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
            let i = key_input(vk, false);
            assert_ne!(i.ki.dw_flags & KEYEVENTF_EXTENDEDKEY, 0, "vk {vk:#x}");
            assert_ne!(i.ki.w_scan, 0, "vk {vk:#x}");
        }
        let a = key_input(b'A', true);
        assert_eq!(a.ki.dw_flags, KEYEVENTF_KEYUP);
        assert_eq!(a.ki.w_scan, 0x1E);
    }
}
