//! Windows clipboard access (text, file lists and images).
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
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;
/// Largest image read from the clipboard (an 8K screenshot as 32-bit DIB is ~130 MB).
const MAX_IMAGE_BYTES: usize = 160 << 20;
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
    fn GlobalSize(h_mem: HGLOBAL) -> usize;
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
        if !open_clipboard_retrying() {
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
        if !open_clipboard_retrying() {
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

/// Replaces the clipboard with `text`. False when the clipboard stayed busy or Windows refused the data.
pub fn set_text(text: &str) -> bool {
    let mut stored = false;
    unsafe {
        if open_clipboard_retrying() {
            EmptyClipboard();
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = wide.len() * 2;
            let h_mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if !h_mem.is_null() {
                let ptr = GlobalLock(h_mem) as *mut u16;
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                    GlobalUnlock(h_mem);
                    stored = !SetClipboardData(CF_UNICODETEXT, h_mem).is_null();
                }
            }
            CloseClipboard();
        }
    }
    stored
}

/// Opens the clipboard, retrying for up to half a second while another program (clipboard history, a
/// clipboard manager, Explorer) holds it.
fn open_clipboard_retrying() -> bool {
    for _ in 0..25 {
        if unsafe { OpenClipboard(ptr::null_mut()) } != 0 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// Replaces the clipboard with a file list Explorer can paste (CF_HDROP plus "Preferred DropEffect" = copy).
/// False when the clipboard stayed busy or Windows refused the data: it then still holds the previous content.
pub fn set_files(file_paths: &[String]) -> bool {
    if file_paths.is_empty() {
        return false;
    }
    let mut stored = false;
    unsafe {
        if open_clipboard_retrying() {
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
                    stored = !SetClipboardData(CF_HDROP, h_mem).is_null();
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
    stored
}

/// The registered "PNG" clipboard format (browsers, Office and Snipping Tool put it next to CF_DIB; it keeps
/// transparency).
fn cf_png() -> UINT {
    let name: Vec<u16> = "PNG\0".encode_utf16().collect();
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// The image on the clipboard, preferring the "PNG" format. None without an image.
pub fn image() -> Option<super::ClipImage> {
    let png = cf_png();
    unsafe {
        let has_png = png != 0 && IsClipboardFormatAvailable(png) != 0;
        if !has_png && IsClipboardFormatAvailable(CF_DIB) == 0 {
            return None;
        }
        if !open_clipboard_retrying() {
            return None;
        }
        let read = |format: UINT| -> Option<Vec<u8>> {
            let h = GetClipboardData(format);
            if h.is_null() {
                return None;
            }
            let len = GlobalSize(h);
            let ptr = GlobalLock(h) as *const u8;
            if ptr.is_null() {
                return None;
            }
            let bytes = (len > 0 && len <= MAX_IMAGE_BYTES).then(|| std::slice::from_raw_parts(ptr, len).to_vec());
            GlobalUnlock(h);
            bytes
        };
        let result =
            if has_png { read(png).map(super::ClipImage::Png) } else { read(CF_DIB).map(super::ClipImage::Dib) };
        CloseClipboard();
        result
    }
}

/// Replaces the clipboard with an image: `dib` as CF_DIB (Windows derives CF_BITMAP and CF_DIBV5 from it) and,
/// when given, `png` as "PNG" for applications that keep transparency. False when the clipboard stayed busy or
/// Windows refused the data.
pub fn set_image(dib: &[u8], png: Option<&[u8]>) -> bool {
    let mut stored = false;
    unsafe {
        if !open_clipboard_retrying() {
            return false;
        }
        EmptyClipboard();
        let put = |format: UINT, bytes: &[u8]| -> bool {
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if h.is_null() {
                return false;
            }
            let ptr = GlobalLock(h) as *mut u8;
            if ptr.is_null() {
                return false;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            GlobalUnlock(h);
            !SetClipboardData(format, h).is_null()
        };
        if !dib.is_empty() {
            stored = put(CF_DIB, dib);
        }
        if let Some(png_bytes) = png {
            let format = cf_png();
            if format != 0 {
                stored |= put(format, png_bytes);
            }
        }
        CloseClipboard();
    }
    stored
}
