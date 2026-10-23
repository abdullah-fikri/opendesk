//! Mouse and keyboard injection (SendInput / mouse_event).

use std::sync::atomic::{AtomicI64, Ordering};

/// Screen position the remote viewer last moved the cursor to; lets capture tell viewer moves from local host moves.
pub static LAST_INJECTED_POS: AtomicI64 = AtomicI64::new(i64::MIN);

/// Packs a screen position into one atomic value (see `LAST_INJECTED_POS`).
pub fn pack_pos(x: i32, y: i32) -> i64 {
    ((x as i64) << 32) | (y as u32 as i64)
}

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
const KEYEVENTF_KEYUP: DWORD = 0x0002;
const KEYEVENTF_UNICODE: DWORD = 0x0004;

const VK_SHIFT: BYTE = 0x10;
const VK_CONTROL: BYTE = 0x11;
const VK_MENU: BYTE = 0x12; // ALT

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
    fn keybd_event(b_vk: BYTE, b_scan: BYTE, dw_flags: DWORD, dw_extra_info: ULONG_PTR);
    fn SendInput(c_inputs: UINT, p_inputs: *const INPUT, cb_size: i32) -> UINT;
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
        unsafe {
            keybd_event(vk, 0, 0, 0);
        }
    }

    /// Releases virtual key `vk`.
    pub fn key_up(&self, vk: u8) {
        unsafe {
            keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
        }
    }

    /// Presses and releases virtual key `vk`.
    pub fn key_tap(&self, vk: u8) {
        unsafe {
            keybd_event(vk, 0, 0, 0);
            keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
        }
    }

    /// Presses `vk` with the given modifiers held, then releases everything.
    pub fn send_key_combo(&self, ctrl: bool, alt: bool, shift: bool, vk: u8) {
        unsafe {
            if ctrl {
                keybd_event(VK_CONTROL, 0, 0, 0);
            }
            if alt {
                keybd_event(VK_MENU, 0, 0, 0);
            }
            if shift {
                keybd_event(VK_SHIFT, 0, 0, 0);
            }
            if vk > 0 {
                keybd_event(vk, 0, 0, 0);
                keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
            }
            if shift {
                keybd_event(VK_SHIFT, 0, KEYEVENTF_KEYUP, 0);
            }
            if alt {
                keybd_event(VK_MENU, 0, KEYEVENTF_KEYUP, 0);
            }
            if ctrl {
                keybd_event(VK_CONTROL, 0, KEYEVENTF_KEYUP, 0);
            }
        }
    }

    /// Types `text` as Unicode characters, independent of the keyboard layout.
    pub fn send_unicode_text(&self, text: &str) {
        unsafe {
            for unit in text.encode_utf16() {
                let inputs = [
                    INPUT {
                        type_: INPUT_KEYBOARD,
                        ki: KEYBDINPUT {
                            w_vk: 0,
                            w_scan: unit,
                            dw_flags: KEYEVENTF_UNICODE,
                            time: 0,
                            dw_extra_info: 0,
                            _pad: [0; 8],
                        },
                    },
                    INPUT {
                        type_: INPUT_KEYBOARD,
                        ki: KEYBDINPUT {
                            w_vk: 0,
                            w_scan: unit,
                            dw_flags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                            time: 0,
                            dw_extra_info: 0,
                            _pad: [0; 8],
                        },
                    },
                ];
                SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
            }
        }
    }
}
