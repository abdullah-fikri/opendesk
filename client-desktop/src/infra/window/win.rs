//! Win32 helpers for the main window: file drag-and-drop, hiding to the tray, and the "send files" picker.

use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::ptr;

use crate::infra::{storage, system_service};

type HWND = *mut c_void;
type HDROP = *mut c_void;
type BOOL = i32;
type UINT = u32;
type DWORD = u32;
#[allow(non_camel_case_types)]
type LONG_PTR = isize;

const WM_CLOSE: u32 = 0x0010;
const WM_COPYGLOBALDATA: u32 = 0x0049;
const WM_DROPFILES: u32 = 0x0233;
const GWLP_WNDPROC: i32 = -4;
const GWL_EXSTYLE: i32 = -20;
const WS_EX_ACCEPTFILES: LONG_PTR = 0x00000010;
const MSGFLT_ALLOW: DWORD = 1;
const MSGFLT_ADD: DWORD = 1;
const SW_HIDE: i32 = 0;
const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_GENERIC_KEYBOARD: u16 = 0x06;
const RIDEV_REMOVE: DWORD = 0x00000001;

#[repr(C)]
struct RAWINPUTDEVICE {
    us_usage_page: u16,
    us_usage: u16,
    dw_flags: DWORD,
    hwnd_target: HWND,
}

const OFN_FILEMUSTEXIST: DWORD = 0x00001000;
const OFN_PATHMUSTEXIST: DWORD = 0x00000800;
const OFN_ALLOWMULTISELECT: DWORD = 0x00000200;
const OFN_EXPLORER: DWORD = 0x00080000;
const OFN_NOCHANGEDIR: DWORD = 0x00000008;

#[repr(C)]
struct OPENFILENAMEW {
    l_struct_size: DWORD,
    hwnd_owner: HWND,
    h_instance: *mut c_void,
    lpstr_filter: *const u16,
    lpstr_custom_filter: *mut u16,
    n_max_cust_filter: DWORD,
    n_filter_index: DWORD,
    lpstr_file: *mut u16,
    n_max_file: DWORD,
    lpstr_file_title: *mut u16,
    n_max_file_title: DWORD,
    lpstr_initial_dir: *const u16,
    lpstr_title: *const u16,
    flags: DWORD,
    n_file_offset: u16,
    n_file_extension: u16,
    lpstr_def_ext: *const u16,
    l_cust_data: isize,
    lpfn_hook: *mut c_void,
    lp_template_name: *const u16,
    pv_reserved: *mut c_void,
    dw_reserved: DWORD,
    flags_ex: DWORD,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(lp_class_name: *const u16, lp_window_name: *const u16) -> HWND;
    fn GetForegroundWindow() -> HWND;
    fn GetClassNameW(h_wnd: HWND, lp_class_name: *mut u16, n_max_count: i32) -> i32;
    fn GetWindowThreadProcessId(h_wnd: HWND, lpdw_process_id: *mut u32) -> u32;
    fn GetWindowLongPtrW(h_wnd: HWND, n_index: i32) -> LONG_PTR;
    fn SetWindowLongPtrW(h_wnd: HWND, n_index: i32, dw_new_long: LONG_PTR) -> LONG_PTR;
    fn CallWindowProcW(lp_prev_wnd_func: LONG_PTR, h_wnd: HWND, msg: u32, w_param: usize, l_param: isize) -> isize;
    fn ChangeWindowMessageFilter(message: UINT, dw_flag: DWORD) -> BOOL;
    fn ChangeWindowMessageFilterEx(
        h_wnd: HWND,
        message: UINT,
        action: DWORD,
        p_change_filter_struct: *mut c_void,
    ) -> BOOL;
    fn ShowWindow(h_wnd: HWND, n_cmd_show: i32) -> BOOL;
    fn IsWindowVisible(h_wnd: HWND) -> BOOL;
    fn IsIconic(h_wnd: HWND) -> BOOL;
    fn RegisterRawInputDevices(devices: *const RAWINPUTDEVICE, count: UINT, size: UINT) -> BOOL;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn RevokeDragDrop(h_wnd: HWND) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragAcceptFiles(h_wnd: HWND, f_accept: BOOL);
    fn DragQueryFileW(h_drop: HDROP, i_file: UINT, lpsz_file: *mut u16, cch: UINT) -> UINT;
    fn DragFinish(h_drop: HDROP);
}

#[link(name = "comdlg32")]
unsafe extern "system" {
    fn GetOpenFileNameW(lpofn: *mut OPENFILENAMEW) -> BOOL;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcessId() -> u32;
}

static mut ORIG_WNDPROC: LONG_PTR = 0;
static mut DROP_CALLBACK: Option<Box<dyn Fn(Vec<PathBuf>) + Send + Sync>> = None;

unsafe extern "system" fn subclass_wndproc(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        // Intercept window [X] close button -> hide to System Tray so background service stays online 24/7
        if msg == WM_CLOSE {
            ShowWindow(hwnd, SW_HIDE);
            return 0;
        }

        if msg == WM_DROPFILES {
            let hdrop = wparam as HDROP;
            let count = DragQueryFileW(hdrop, 0xFFFFFFFF, ptr::null_mut(), 0);
            let mut paths = Vec::with_capacity(count as usize);

            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, ptr::null_mut(), 0);
                if len > 0 {
                    let mut buf = vec![0u16; (len + 1) as usize];
                    DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len + 1);
                    if let Some(pos) = buf.iter().position(|&c| c == 0) {
                        buf.truncate(pos);
                    }
                    let path_str = String::from_utf16_lossy(&buf);
                    paths.push(PathBuf::from(path_str));
                }
            }
            DragFinish(hdrop);

            if !paths.is_empty()
                && let Some(ref cb) = *ptr::addr_of!(DROP_CALLBACK)
            {
                cb(paths);
            }
            return 0;
        }

        let prev = ORIG_WNDPROC;
        if prev != 0 { CallWindowProcW(prev, hwnd, msg, wparam, lparam) } else { 0 }
    }
}

/// Hides the window titled `window_title` (it stays alive in the tray).
pub fn hide_window_by_title(window_title: &str) {
    unsafe {
        let title_wide: Vec<u16> = window_title.encode_utf16().chain(std::iter::once(0)).collect();
        let hwnd = FindWindowW(ptr::null(), title_wide.as_ptr());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Whether the window titled `window_title` is on screen (visible and not minimized).
pub fn is_window_shown(window_title: &str) -> bool {
    unsafe {
        let title_wide: Vec<u16> = window_title.encode_utf16().chain(std::iter::once(0)).collect();
        let hwnd = FindWindowW(ptr::null(), title_wide.as_ptr());
        !hwnd.is_null() && IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0
    }
}

/// Accepts files dropped on the window titled `window_title` and calls `callback` with their paths. Must run on the UI thread once the window exists; returns false until it does.
pub fn register_window_file_drop<F>(window_title: &str, callback: F) -> bool
where
    F: Fn(Vec<PathBuf>) + Send + Sync + 'static,
{
    unsafe {
        let title_wide: Vec<u16> = window_title.encode_utf16().chain(std::iter::once(0)).collect();
        let mut hwnd = FindWindowW(ptr::null(), title_wide.as_ptr());

        if hwnd.is_null() {
            let fg = GetForegroundWindow();
            if !fg.is_null() {
                let mut pid = 0u32;
                GetWindowThreadProcessId(fg, &mut pid);
                if pid == GetCurrentProcessId() {
                    hwnd = fg;
                }
            }
        }

        if hwnd.is_null() {
            return false;
        }

        if ORIG_WNDPROC == 0 {
            DROP_CALLBACK = Some(Box::new(callback));
            // Must be called on the UI thread that created the window so OLE RevokeDragDrop succeeds!
            let _ = RevokeDragDrop(hwnd);
            let _ = ChangeWindowMessageFilter(WM_DROPFILES, MSGFLT_ADD);
            let _ = ChangeWindowMessageFilter(WM_COPYGLOBALDATA, MSGFLT_ADD);
            let _ = ChangeWindowMessageFilterEx(hwnd, WM_DROPFILES, MSGFLT_ALLOW, ptr::null_mut());
            let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYGLOBALDATA, MSGFLT_ALLOW, ptr::null_mut());
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style | WS_EX_ACCEPTFILES);
            DragAcceptFiles(hwnd, 1);
            ORIG_WNDPROC = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, subclass_wndproc as *const () as LONG_PTR);
            release_raw_keyboard();
        }
        true
    }
}

/// Drops winit's raw keyboard registration. While a window of a process registered for raw keyboard input is in
/// front, Windows calls no low-level keyboard hook (ours included), so Win and Alt+Tab went to this PC. Slint reads
/// keys from window messages, which raw input removal leaves alone. UI thread, once the event loop exists.
fn release_raw_keyboard() {
    let keyboard = RAWINPUTDEVICE {
        us_usage_page: HID_USAGE_PAGE_GENERIC,
        us_usage: HID_USAGE_GENERIC_KEYBOARD,
        dw_flags: RIDEV_REMOVE,
        hwnd_target: ptr::null_mut(),
    };
    let ok = unsafe { RegisterRawInputDevices(&keyboard, 1, std::mem::size_of::<RAWINPUTDEVICE>() as UINT) } != 0;
    if !ok {
        crate::infra::logger::warn("ui", "Raw keyboard input still registered: Win and Alt+Tab may stay local.");
    }
}

/// Window class of the focused window ("CabinetWClass" = Explorer folder, "Progman" = desktop), for diagnostics.
pub fn foreground_class() -> String {
    let mut buf = [0u16; 128];
    let len = unsafe { GetClassNameW(GetForegroundWindow(), buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 0 { "(none)".into() } else { String::from_utf16_lossy(&buf[..len as usize]) }
}

/// Shows the multi-select "Choose Files to Send" dialog; empty when cancelled.
pub fn pick_files_dialog() -> Vec<PathBuf> {
    if system_service::is_local_system() {
        return pick_files_as_session_user();
    }
    show_open_dialog()
}

/// The open dialog is a small Explorer: shown by a SYSTEM process it would start any program as SYSTEM. The
/// service's agent shows it from `OpenDesk.exe --pick-files <out>` running as the logged-on user instead.
fn pick_files_as_session_user() -> Vec<PathBuf> {
    let out = storage::app_dir().join("picked-files.txt");
    let _ = fs::remove_file(&out);
    if !system_service::run_as_session_user(&format!("--pick-files \"{}\"", out.display())) {
        return Vec::new();
    }
    let paths =
        fs::read_to_string(&out).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
    let _ = fs::remove_file(&out);
    paths
}

fn show_open_dialog() -> Vec<PathBuf> {
    unsafe {
        let mut file_buf = vec![0u16; 8192];
        let filter: Vec<u16> = "All Files (*.*)\0*.*\0\0".encode_utf16().collect();
        let title: Vec<u16> = "Choose Files to Send\0".encode_utf16().collect();

        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.l_struct_size = std::mem::size_of::<OPENFILENAMEW>() as DWORD;
        ofn.lpstr_filter = filter.as_ptr();
        ofn.lpstr_file = file_buf.as_mut_ptr();
        ofn.n_max_file = file_buf.len() as DWORD;
        ofn.lpstr_title = title.as_ptr();
        ofn.flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_ALLOWMULTISELECT | OFN_NOCHANGEDIR;

        if GetOpenFileNameW(&mut ofn) == 0 {
            return Vec::new();
        }

        // Parse double-null terminated list (either single path or directory + multiple filenames)
        let mut parts: Vec<String> = Vec::new();
        let mut start = 0usize;
        for i in 0..file_buf.len() {
            if file_buf[i] == 0 {
                if i == start {
                    break;
                }
                parts.push(String::from_utf16_lossy(&file_buf[start..i]));
                start = i + 1;
            }
        }

        if parts.is_empty() {
            Vec::new()
        } else if parts.len() == 1 {
            vec![PathBuf::from(&parts[0])]
        } else {
            let dir = PathBuf::from(&parts[0]);
            parts[1..].iter().map(|name| dir.join(name)).collect()
        }
    }
}
