use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicI64, Ordering};

/// Screen position the remote viewer last moved the cursor to; lets capture tell viewer moves from local host moves.
pub static LAST_INJECTED_POS: AtomicI64 = AtomicI64::new(i64::MIN);

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
type HWND = *mut c_void;
type HGLOBAL = *mut c_void;
type HDROP = *mut c_void;

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

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const GMEM_MOVEABLE: u32 = 0x0002;
const GMEM_ZEROINIT: u32 = 0x0040;

#[repr(C)]
struct DROPFILES {
    p_files: DWORD,
    pt_x: i32,
    pt_y: i32,
    f_nc: BOOL,
    f_wide: BOOL,
}

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
    fn OpenClipboard(h_wnd: HWND) -> BOOL;
    fn CloseClipboard() -> BOOL;
    fn EmptyClipboard() -> BOOL;
    fn SetClipboardData(u_format: u32, h_mem: HGLOBAL) -> HGLOBAL;
    fn GetClipboardData(u_format: u32) -> HGLOBAL;
    fn IsClipboardFormatAvailable(format: u32) -> BOOL;
    fn GetClipboardSequenceNumber() -> DWORD;
    fn RegisterClipboardFormatW(lpsz_format: *const u16) -> UINT;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragQueryFileW(h_drop: HDROP, i_file: UINT, lpsz_file: *mut u16, cch: UINT) -> UINT;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(u_flags: u32, dw_bytes: usize) -> HGLOBAL;
    fn GlobalLock(h_mem: HGLOBAL) -> *mut c_void;
    fn GlobalUnlock(h_mem: HGLOBAL) -> BOOL;
}

pub struct InputInjector;

impl InputInjector {
    pub fn new() -> Self {
        Self
    }

    pub fn set_cursor_pos(&self, x: i32, y: i32) {
        LAST_INJECTED_POS.store(pack_pos(x, y), Ordering::Relaxed);
        unsafe {
            SetCursorPos(x, y);
        }
    }

    pub fn mouse_down(&self, button: i32) {
        unsafe {
            if button == 0 {
                mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
            } else if button == 2 {
                mouse_event(MOUSEEVENTF_RIGHTDOWN, 0, 0, 0, 0);
            }
        }
    }

    pub fn mouse_up(&self, button: i32) {
        unsafe {
            if button == 0 {
                mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
            } else if button == 2 {
                mouse_event(MOUSEEVENTF_RIGHTUP, 0, 0, 0, 0);
            }
        }
    }

    pub fn mouse_wheel(&self, delta_y: i32) {
        unsafe {
            let win_delta = if delta_y.abs() < 40 {
                delta_y * 120
            } else {
                delta_y
            };
            mouse_event(MOUSEEVENTF_WHEEL, 0, 0, win_delta as u32, 0);
        }
    }

    pub fn key_down(&self, vk: u8) {
        unsafe {
            keybd_event(vk, 0, 0, 0);
        }
    }

    pub fn key_up(&self, vk: u8) {
        unsafe {
            keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
        }
    }

    pub fn key_tap(&self, vk: u8) {
        unsafe {
            keybd_event(vk, 0, 0, 0);
            keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
        }
    }

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

    pub fn get_clipboard_sequence_number(&self) -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    pub fn get_clipboard_text(&self) -> Option<String> {
        unsafe {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
                return None;
            }
            if OpenClipboard(ptr::null_mut()) == 0 {
                return None;
            }
            let mut result = None;
            let h_mem = GetClipboardData(CF_UNICODETEXT);
            if !h_mem.is_null() {
                let ptr = GlobalLock(h_mem) as *const u16;
                if !ptr.is_null() {
                    let mut len = 0usize;
                    while len < 65536 && *ptr.add(len) != 0 {
                        len += 1;
                    }
                    let slice = std::slice::from_raw_parts(ptr, len);
                    result = Some(String::from_utf16_lossy(slice));
                    GlobalUnlock(h_mem);
                }
            }
            CloseClipboard();
            result
        }
    }

    pub fn get_clipboard_files(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 {
                return paths;
            }
            if OpenClipboard(ptr::null_mut()) == 0 {
                return paths;
            }
            let h_drop = GetClipboardData(CF_HDROP);
            if !h_drop.is_null() {
                let count = DragQueryFileW(h_drop, 0xFFFFFFFF, ptr::null_mut(), 0);
                for i in 0..count {
                    let len = DragQueryFileW(h_drop, i, ptr::null_mut(), 0);
                    if len > 0 {
                        let mut buf = vec![0u16; (len + 1) as usize];
                        DragQueryFileW(h_drop, i, buf.as_mut_ptr(), len + 1);
                        if let Some(pos) = buf.iter().position(|&c| c == 0) {
                            buf.truncate(pos);
                        }
                        paths.push(PathBuf::from(String::from_utf16_lossy(&buf)));
                    }
                }
            }
            CloseClipboard();
        }
        paths
    }

    pub fn set_clipboard_text(&self, text: &str) {
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                EmptyClipboard();
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = wide.len() * 2;
                let h_mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
                if !h_mem.is_null() {
                    let ptr = GlobalLock(h_mem) as *mut u16;
                    if !ptr.is_null() {
                        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                        GlobalUnlock(h_mem);
                        SetClipboardData(CF_UNICODETEXT, h_mem);
                    }
                }
                CloseClipboard();
            }
        }
    }

    pub fn set_clipboard_files(&self, file_paths: &[String]) {
        if file_paths.is_empty() {
            return;
        }
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                EmptyClipboard();

                let mut wide_files: Vec<u16> = Vec::new();
                for path in file_paths {
                    wide_files.extend(path.encode_utf16());
                    wide_files.push(0); // null-terminated per file
                }
                wide_files.push(0); // double-null terminated list

                let header_size = std::mem::size_of::<DROPFILES>();
                let total_bytes = header_size + wide_files.len() * 2;
                let h_mem = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total_bytes);
                if !h_mem.is_null() {
                    let ptr = GlobalLock(h_mem) as *mut u8;
                    if !ptr.is_null() {
                        let df = ptr as *mut DROPFILES;
                        (*df).p_files = header_size as DWORD;
                        (*df).pt_x = 0;
                        (*df).pt_y = 0;
                        (*df).f_nc = 0;
                        (*df).f_wide = 1; // TRUE (Unicode UTF-16)

                        let dest_str = ptr.add(header_size) as *mut u16;
                        std::ptr::copy_nonoverlapping(wide_files.as_ptr(), dest_str, wide_files.len());
                        GlobalUnlock(h_mem);
                        SetClipboardData(CF_HDROP, h_mem);
                    }
                }

                // Also set "Preferred DropEffect" = DROPEFFECT_COPY (1) so Windows Explorer always enables Paste
                let fmt_name: Vec<u16> = "Preferred DropEffect\0".encode_utf16().collect();
                let cf_effect = RegisterClipboardFormatW(fmt_name.as_ptr());
                if cf_effect != 0 {
                    let h_eff = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, 4);
                    if !h_eff.is_null() {
                        let p_eff = GlobalLock(h_eff) as *mut u32;
                        if !p_eff.is_null() {
                            *p_eff = 1; // DROPEFFECT_COPY
                            GlobalUnlock(h_eff);
                            SetClipboardData(cf_effect, h_eff);
                        }
                    }
                }

                CloseClipboard();
            }
        }
    }
}
