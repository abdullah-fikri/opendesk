//! Transfer badge through Win32: a layered, click-through, never-activated tool window above the taskbar's
//! corner, painted with GDI on its own thread. It is created on first use, on the desktop that thread starts
//! on (the user's desktop for the service's agent too).

use std::ffi::c_void;
use std::ptr;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;

type HWND = *mut c_void;
type HDC = *mut c_void;
type HGDIOBJ = *mut c_void;
type BOOL = i32;
type DWORD = u32;
type UINT = u32;

const WS_POPUP: DWORD = 0x8000_0000;
const WS_EX_TOPMOST: DWORD = 0x0000_0008;
const WS_EX_TRANSPARENT: DWORD = 0x0000_0020;
const WS_EX_TOOLWINDOW: DWORD = 0x0000_0080;
const WS_EX_LAYERED: DWORD = 0x0008_0000;
const WS_EX_NOACTIVATE: DWORD = 0x0800_0000;
const LWA_ALPHA: DWORD = 0x0000_0002;
const SW_HIDE: i32 = 0;
const SW_SHOWNA: i32 = 8;
const HWND_TOPMOST: HWND = -1isize as HWND;
const SWP_NOACTIVATE: UINT = 0x0010;
const WM_PAINT: u32 = 0x000F;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_TIMER: u32 = 0x0113;
/// Apply [`WANT_VISIBLE`], [`TEXT`] and [`LINGER_MS`].
const WM_USER_SYNC: u32 = 0x0400 + 230;
const SPI_GETWORKAREA: UINT = 0x0030;
const HIDE_TIMER: usize = 1;
const TRANSPARENT_BK: i32 = 1;
const DT_LEFT_VCENTER_SINGLE_ELLIPSIS: UINT = 0x0000_0004 | 0x0000_0020 | 0x0000_8000;

const WIDTH: i32 = 380;
const HEIGHT: i32 = 40;
const MARGIN: i32 = 12;

/// The badge window once created (HWND).
static HWND_BADGE: AtomicUsize = AtomicUsize::new(0);
static START: Once = Once::new();
static TEXT: Mutex<String> = Mutex::new(String::new());
static WANT_VISIBLE: AtomicBool = AtomicBool::new(false);
/// Hide this many ms after the last `show`; 0 keeps it up.
static LINGER_MS: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct PAINTSTRUCT {
    hdc: HDC,
    f_erase: BOOL,
    rc_paint: RECT,
    f_restore: BOOL,
    f_inc_update: BOOL,
    rgb_reserved: [u8; 32],
}

#[repr(C)]
struct WNDCLASSEXW {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: Option<unsafe extern "system" fn(HWND, u32, usize, isize) -> isize>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: *mut c_void,
    h_icon: *mut c_void,
    h_cursor: *mut c_void,
    hbr_background: *mut c_void,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: *mut c_void,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(
        dw_ex_style: DWORD,
        lp_class_name: *const u16,
        lp_window_name: *const u16,
        dw_style: DWORD,
        x: i32,
        y: i32,
        n_width: i32,
        n_height: i32,
        h_wnd_parent: HWND,
        h_menu: *mut c_void,
        h_instance: *mut c_void,
        lp_param: *mut c_void,
    ) -> HWND;
    fn DefWindowProcW(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn ShowWindow(hwnd: HWND, n_cmd_show: i32) -> BOOL;
    fn SetWindowPos(hwnd: HWND, hwnd_insert_after: HWND, x: i32, y: i32, cx: i32, cy: i32, u_flags: UINT) -> BOOL;
    fn InvalidateRect(hwnd: HWND, lp_rect: *const RECT, b_erase: BOOL) -> BOOL;
    fn BeginPaint(hwnd: HWND, lp_paint: *mut PAINTSTRUCT) -> HDC;
    fn EndPaint(hwnd: HWND, lp_paint: *const PAINTSTRUCT) -> BOOL;
    fn FillRect(hdc: HDC, lprc: *const RECT, hbr: HGDIOBJ) -> i32;
    fn DrawTextW(hdc: HDC, lpch_text: *const u16, cch_text: i32, lprc: *mut RECT, format: UINT) -> i32;
    fn SetLayeredWindowAttributes(hwnd: HWND, cr_key: DWORD, b_alpha: u8, dw_flags: DWORD) -> BOOL;
    fn GetMessageW(lp_msg: *mut MSG, hwnd: HWND, min: u32, max: u32) -> BOOL;
    fn DispatchMessageW(lp_msg: *const MSG) -> isize;
    fn PostMessageW(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> BOOL;
    fn SetTimer(hwnd: HWND, id: usize, elapse_ms: UINT, func: *mut c_void) -> usize;
    fn KillTimer(hwnd: HWND, id: usize) -> BOOL;
    fn SystemParametersInfoW(action: UINT, param: UINT, pv_param: *mut c_void, win_ini: UINT) -> BOOL;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateSolidBrush(color: DWORD) -> HGDIOBJ;
    fn DeleteObject(ho: HGDIOBJ) -> BOOL;
    fn SelectObject(hdc: HDC, h: HGDIOBJ) -> HGDIOBJ;
    fn SetTextColor(hdc: HDC, color: DWORD) -> DWORD;
    fn SetBkMode(hdc: HDC, mode: i32) -> i32;
    fn CreateFontW(
        c_height: i32,
        c_width: i32,
        c_escapement: i32,
        c_orientation: i32,
        c_weight: i32,
        b_italic: DWORD,
        b_underline: DWORD,
        b_strike_out: DWORD,
        i_char_set: DWORD,
        i_out_precision: DWORD,
        i_clip_precision: DWORD,
        i_quality: DWORD,
        i_pitch_and_family: DWORD,
        psz_face_name: *const u16,
    ) -> HGDIOBJ;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(lp_module_name: *const u16) -> *mut c_void;
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Shows `text` (replacing what is shown); with `hide_after` the badge disappears that long after this call.
/// Cheap: call it on every progress change from any thread.
pub fn show(text: &str, hide_after: Option<Duration>) {
    *TEXT.lock() = text.to_string();
    LINGER_MS.store(hide_after.map_or(0, |d| d.as_millis().clamp(1, u32::MAX as u128) as u32), Ordering::SeqCst);
    WANT_VISIBLE.store(true, Ordering::SeqCst);
    START.call_once(|| {
        crate::infra::thread::spawn_named("transfer-badge", run);
    });
    sync();
}

/// Hides the badge.
pub fn hide() {
    WANT_VISIBLE.store(false, Ordering::SeqCst);
    sync();
}

fn sync() {
    let hwnd = HWND_BADGE.load(Ordering::SeqCst) as HWND;
    if !hwnd.is_null() {
        // SAFETY: posting to a window handle is safe even if the window is gone; it then fails.
        unsafe { PostMessageW(hwnd, WM_USER_SYNC, 0, 0) };
    }
}

fn run() {
    // SAFETY: plain Win32 window creation and a message loop on this thread; every pointer passed lives on the
    // stack for the duration of the call.
    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let class_name = to_wide("OpenDeskTransferBadge");
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cb_size = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfn_wnd_proc = Some(wnd_proc);
        wc.h_instance = hinst;
        wc.lpsz_class_name = class_name.as_ptr();
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_POPUP,
            0,
            0,
            WIDTH,
            HEIGHT,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null_mut(),
        );
        if hwnd.is_null() {
            crate::infra::logger::warn("file_transfer", "Could not create the transfer badge window.");
            return;
        }
        SetLayeredWindowAttributes(hwnd, 0, 235, LWA_ALPHA);
        HWND_BADGE.store(hwnd as usize, Ordering::SeqCst);
        PostMessageW(hwnd, WM_USER_SYNC, 0, 0);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: called by Windows on the badge thread with a valid window; GDI objects are created and released
    // within each paint.
    unsafe {
        match msg {
            WM_USER_SYNC => {
                if !WANT_VISIBLE.load(Ordering::SeqCst) {
                    KillTimer(hwnd, HIDE_TIMER);
                    ShowWindow(hwnd, SW_HIDE);
                    return 0;
                }
                // The work area (above the taskbar) of the primary monitor; may change between transfers.
                let mut area = RECT::default();
                SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut area as *mut RECT).cast(), 0);
                let (x, y) = (area.right - WIDTH - MARGIN, area.bottom - HEIGHT - MARGIN);
                SetWindowPos(hwnd, HWND_TOPMOST, x, y, WIDTH, HEIGHT, SWP_NOACTIVATE);
                ShowWindow(hwnd, SW_SHOWNA);
                InvalidateRect(hwnd, ptr::null(), 1);
                match LINGER_MS.load(Ordering::SeqCst) {
                    0 => {
                        KillTimer(hwnd, HIDE_TIMER);
                    }
                    ms => {
                        SetTimer(hwnd, HIDE_TIMER, ms, ptr::null_mut());
                    }
                }
                0
            }
            WM_TIMER if wparam == HIDE_TIMER => {
                KillTimer(hwnd, HIDE_TIMER);
                WANT_VISIBLE.store(false, Ordering::SeqCst);
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rect = RECT { left: 0, top: 0, right: WIDTH, bottom: HEIGHT };
                let bg = CreateSolidBrush(0x0014_1111); // #111114
                FillRect(hdc, &rect, bg);
                DeleteObject(bg);
                let accent = CreateSolidBrush(0x00EB_6325); // #2563eb
                FillRect(hdc, &RECT { left: 0, top: 0, right: 4, bottom: HEIGHT }, accent);
                DeleteObject(accent);
                let face = to_wide("Segoe UI");
                let font = CreateFontW(-15, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
                let old = SelectObject(hdc, font);
                SetBkMode(hdc, TRANSPARENT_BK);
                SetTextColor(hdc, 0x00FA_FAFA);
                let text: Vec<u16> = TEXT.lock().encode_utf16().collect();
                rect.left = 16;
                rect.right -= 12;
                DrawTextW(hdc, text.as_ptr(), text.len() as i32, &mut rect, DT_LEFT_VCENTER_SINGLE_ELLIPSIS);
                SelectObject(hdc, old);
                DeleteObject(font);
                EndPaint(hwnd, &ps);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
