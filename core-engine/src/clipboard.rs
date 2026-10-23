//! Windows clipboard access (text and file lists).
#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;

type BOOL = i32;
type DWORD = u32;
type UINT = u32;
type HWND = *mut c_void;
type HGLOBAL = *mut c_void;
type HDROP = *mut c_void;

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

#[link(name = "user32")]
unsafe extern "system" {
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

/// Changes every time any application writes the clipboard.
pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Current clipboard text (up to 64K UTF-16 units), if any.
pub fn text() -> Option<String> {
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

/// Paths of files on the clipboard (CF_HDROP), e.g. after Ctrl+C in Explorer.
pub fn files() -> Vec<PathBuf> {
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

/// Replaces the clipboard with `text`.
pub fn set_text(text: &str) {
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

/// Replaces the clipboard with a file list Explorer can paste (CF_HDROP plus "Preferred DropEffect" = copy).
pub fn set_files(file_paths: &[String]) {
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
