use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::logger;
use crate::protocol::OutgoingMsg;

type HWND = *mut c_void;
type HDC = *mut c_void;
type HBITMAP = *mut c_void;
type HGDIOBJ = *mut c_void;
type HBRUSH = *mut c_void;
type HFONT = *mut c_void;
type HHOOK = *mut c_void;
type BOOL = i32;
type DWORD = u32;
type UINT = u32;

const WS_POPUP: DWORD = 0x80000000;
const WS_EX_TOPMOST: DWORD = 0x00000008;
const WS_EX_TRANSPARENT: DWORD = 0x00000020;
const WS_EX_TOOLWINDOW: DWORD = 0x00000080;
const WS_EX_LAYERED: DWORD = 0x00080000;

const LWA_ALPHA: DWORD = 0x00000002;
const WDA_EXCLUDEFROMCAPTURE: DWORD = 0x00000011;
const WDA_MONITOR: DWORD = 0x00000001;

const SW_HIDE: i32 = 0;
const SW_SHOWNA: i32 = 8;
const HWND_TOPMOST: HWND = -1isize as HWND;
const SWP_NOACTIVATE: UINT = 0x0010;
const SWP_SHOWWINDOW: UINT = 0x0040;

const WM_PAINT: u32 = 0x000F;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_USER_UPDATE_PRIVACY: u32 = 0x0400 + 220;

const SRCCOPY: DWORD = 0x00CC0020;
const TRANSPARENT_BK: i32 = 1;
const DT_CENTER: UINT = 0x00000001;
const DT_VCENTER: UINT = 0x00000004;
const DT_SINGLELINE: UINT = 0x00000020;

const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;

const WH_KEYBOARD_LL: i32 = 13;
const WH_MOUSE_LL: i32 = 14;
const WM_KEYDOWN: usize = 0x0100;
const WM_KEYUP: usize = 0x0101;
const WM_SYSKEYDOWN: usize = 0x0104;
const WM_SYSKEYUP: usize = 0x0105;

const LLKHF_ALTDOWN: DWORD = 0x00000020;
const LLKHF_INJECTED: DWORD = 0x00000010;
const LLMHF_INJECTED: DWORD = 0x00000001;

const VK_TAB: DWORD = 0x09;
const VK_F4: DWORD = 0x73;
const VK_LWIN: DWORD = 0x5B;
const VK_RWIN: DWORD = 0x5C;

/// Privacy modes:
/// 0 = Off (Normal)
/// 1 = Freeze Screen (Snapshot of desktop before privacy activated)
/// 2 = Maintenance Screen ("Sistem sedang dalam pemeliharaan...")
/// 3 = Black Screen (Pitch black monitor)
static HOST_PRIVACY_MODE: AtomicI32 = AtomicI32::new(0);
static HOST_BLOCK_INPUT: AtomicBool = AtomicBool::new(false);
static OVERLAY_HWND: AtomicUsize = AtomicUsize::new(0);

static VIEWER_SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);
static VIEWER_MAIN_HWND: AtomicUsize = AtomicUsize::new(0);
static WIN_KEY_HELD: AtomicBool = AtomicBool::new(false);

struct HookContext {
    tx_out: Sender<OutgoingMsg>,
    partner_id: String,
}

static HOOK_CTX: OnceLock<Mutex<Option<HookContext>>> = OnceLock::new();

#[allow(dead_code)]
struct FrozenSurface {
    mem_dc: usize,
    mem_bmp: usize,
    old_bmp: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

static FROZEN_SURFACE: OnceLock<Mutex<Option<FrozenSurface>>> = OnceLock::new();

#[repr(C)]
#[derive(Clone, Copy)]
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
    hbr_background: HBRUSH,
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

#[repr(C)]
struct KBDLLHOOKSTRUCT {
    vk_code: DWORD,
    scan_code: DWORD,
    flags: DWORD,
    time: DWORD,
    dw_extra_info: usize,
}

#[repr(C)]
struct MSLLHOOKSTRUCT {
    pt_x: i32,
    pt_y: i32,
    mouse_data: DWORD,
    flags: DWORD,
    time: DWORD,
    dw_extra_info: usize,
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
    fn SetWindowPos(
        hwnd: HWND,
        hwnd_insert_after: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        u_flags: UINT,
    ) -> BOOL;
    fn InvalidateRect(hwnd: HWND, lp_rect: *const RECT, b_erase: BOOL) -> BOOL;
    fn BeginPaint(hwnd: HWND, lp_paint: *mut PAINTSTRUCT) -> HDC;
    fn EndPaint(hwnd: HWND, lp_paint: *const PAINTSTRUCT) -> BOOL;
    fn FillRect(hdc: HDC, lprc: *const RECT, hbr: HBRUSH) -> i32;
    fn DrawTextW(
        hdc: HDC,
        lpch_text: *const u16,
        cch_text: i32,
        lprc: *mut RECT,
        format: UINT,
    ) -> i32;
    fn SetLayeredWindowAttributes(hwnd: HWND, cr_key: DWORD, b_alpha: u8, dw_flags: DWORD) -> BOOL;
    fn SetWindowDisplayAffinity(hwnd: HWND, dw_affinity: DWORD) -> BOOL;
    fn GetSystemMetrics(n_index: i32) -> i32;
    fn GetDC(hwnd: HWND) -> HDC;
    fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
    fn GetMessageW(lp_msg: *mut MSG, hwnd: HWND, min: u32, max: u32) -> BOOL;
    fn TranslateMessage(lp_msg: *const MSG) -> BOOL;
    fn DispatchMessageW(lp_msg: *const MSG) -> isize;
    fn PostMessageW(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> BOOL;
    fn FindWindowW(lp_class_name: *const u16, lp_window_name: *const u16) -> HWND;
    fn GetForegroundWindow() -> HWND;
    fn SetWindowsHookExW(
        id_hook: i32,
        lpfn: unsafe extern "system" fn(i32, usize, isize) -> isize,
        hmod: *mut c_void,
        dw_thread_id: DWORD,
    ) -> HHOOK;
    fn CallNextHookEx(hhk: HHOOK, n_code: i32, wparam: usize, lparam: isize) -> isize;
    fn BlockInput(f_block_it: BOOL) -> BOOL;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(hdc: HDC) -> HDC;
    fn CreateCompatibleBitmap(hdc: HDC, cx: i32, cy: i32) -> HBITMAP;
    fn SelectObject(hdc: HDC, h: HGDIOBJ) -> HGDIOBJ;
    fn DeleteObject(ho: HGDIOBJ) -> BOOL;
    fn DeleteDC(hdc: HDC) -> BOOL;
    fn BitBlt(
        hdc: HDC,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        hdc_src: HDC,
        x1: i32,
        y1: i32,
        rop: DWORD,
    ) -> BOOL;
    fn CreateSolidBrush(color: DWORD) -> HBRUSH;
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
    ) -> HFONT;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(lp_module_name: *const u16) -> *mut c_void;
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn capture_freeze_snapshot() {
    unsafe {
        let mut vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let mut vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let mut vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let mut vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if vw <= 0 || vh <= 0 {
            vx = 0;
            vy = 0;
            vw = GetSystemMetrics(0).max(1);
            vh = GetSystemMetrics(1).max(1);
        }

        let screen_dc = GetDC(ptr::null_mut());
        let mem_dc = CreateCompatibleDC(screen_dc);
        let mem_bmp = CreateCompatibleBitmap(screen_dc, vw, vh);
        let old_bmp = SelectObject(mem_dc, mem_bmp);
        BitBlt(mem_dc, 0, 0, vw, vh, screen_dc, vx, vy, SRCCOPY);
        ReleaseDC(ptr::null_mut(), screen_dc);

        let lock = FROZEN_SURFACE.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = lock.lock() {
            if let Some(prev) = guard.take() {
                SelectObject(prev.mem_dc as HDC, prev.old_bmp as HGDIOBJ);
                DeleteObject(prev.mem_bmp as HGDIOBJ);
                DeleteDC(prev.mem_dc as HDC);
            }
            *guard = Some(FrozenSurface {
                mem_dc: mem_dc as usize,
                mem_bmp: mem_bmp as usize,
                old_bmp: old_bmp as usize,
                x: vx,
                y: vy,
                w: vw,
                h: vh,
            });
        }
    }
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    unsafe {
        match msg {
            WM_USER_UPDATE_PRIVACY => {
                let mode = HOST_PRIVACY_MODE.load(Ordering::SeqCst);
                if mode == 0 {
                    ShowWindow(hwnd, SW_HIDE);
                } else {
                    let mut vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
                    let mut vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
                    let mut vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
                    let mut vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
                    if vw <= 0 || vh <= 0 {
                        vx = 0;
                        vy = 0;
                        vw = GetSystemMetrics(0).max(1);
                        vh = GetSystemMetrics(1).max(1);
                    }
                    // Exclude this privacy overlay from OpenDesk's own GDI BitBlt screen capture!
                    if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) == 0 {
                        SetWindowDisplayAffinity(hwnd, WDA_MONITOR);
                    }
                    SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        vx,
                        vy,
                        vw,
                        vh,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    ShowWindow(hwnd, SW_SHOWNA);
                    InvalidateRect(hwnd, ptr::null(), 1);
                }
                0
            }
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mode = HOST_PRIVACY_MODE.load(Ordering::SeqCst);

                let mut vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
                let mut vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
                if vw <= 0 || vh <= 0 {
                    vw = GetSystemMetrics(0).max(1);
                    vh = GetSystemMetrics(1).max(1);
                }
                let full_rc = RECT {
                    left: 0,
                    top: 0,
                    right: vw,
                    bottom: vh,
                };

                if mode == 1 {
                    // Mode 1: Freeze Screen (Paints static snapshot of desktop prior to privacy lock)
                    let mut painted = false;
                    if let Some(lock) = FROZEN_SURFACE.get() {
                        if let Ok(guard) = lock.lock() {
                            if let Some(ref surf) = *guard {
                                BitBlt(
                                    hdc,
                                    0,
                                    0,
                                    surf.w,
                                    surf.h,
                                    surf.mem_dc as HDC,
                                    0,
                                    0,
                                    SRCCOPY,
                                );
                                painted = true;
                            }
                        }
                    }
                    if !painted {
                        let br = CreateSolidBrush(0x000B0909);
                        FillRect(hdc, &full_rc, br);
                        DeleteObject(br);
                    }
                } else if mode == 2 {
                    // Mode 2: Maintenance / System Update Screen
                    let bg_br = CreateSolidBrush(0x000B0909); // #09090b
                    FillRect(hdc, &full_rc, bg_br);
                    DeleteObject(bg_br);

                    // Center card
                    let cw = 520;
                    let ch = 170;
                    let cx = (vw - cw) / 2;
                    let cy = (vh - ch) / 2;

                    let card_rc = RECT {
                        left: cx,
                        top: cy,
                        right: cx + cw,
                        bottom: cy + ch,
                    };
                    let card_br = CreateSolidBrush(0x00141111); // #111114
                    FillRect(hdc, &card_rc, card_br);
                    DeleteObject(card_br);

                    let top_bar = RECT {
                        left: cx,
                        top: cy,
                        right: cx + cw,
                        bottom: cy + 3,
                    };
                    let red_br = CreateSolidBrush(0x002626DC); // BGR for #dc2626
                    FillRect(hdc, &top_bar, red_br);
                    DeleteObject(red_br);

                    SetBkMode(hdc, TRANSPARENT_BK);
                    let font_face = to_wide("Segoe UI");
                    let hfont_title = CreateFontW(
                        -22,
                        0,
                        0,
                        0,
                        700,
                        0,
                        0,
                        0,
                        1,
                        0,
                        0,
                        5,
                        0,
                        font_face.as_ptr(),
                    );
                    let old_f = SelectObject(hdc, hfont_title);
                    SetTextColor(hdc, 0x00FAFAFA);
                    let mut title_rc = RECT {
                        left: cx + 20,
                        top: cy + 36,
                        right: cx + cw - 20,
                        bottom: cy + 80,
                    };
                    let title_txt = to_wide("Pemeliharaan Sistem Sedang Berlangsung");
                    DrawTextW(
                        hdc,
                        title_txt.as_ptr(),
                        -1,
                        &mut title_rc,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );

                    let hfont_sub = CreateFontW(
                        -15,
                        0,
                        0,
                        0,
                        400,
                        0,
                        0,
                        0,
                        1,
                        0,
                        0,
                        5,
                        0,
                        font_face.as_ptr(),
                    );
                    SelectObject(hdc, hfont_sub);
                    SetTextColor(hdc, 0x00A1A1AA);
                    let mut sub_rc = RECT {
                        left: cx + 20,
                        top: cy + 88,
                        right: cx + cw - 20,
                        bottom: cy + 135,
                    };
                    let sub_txt = to_wide("Mohon tunggu sebentar, jangan matikan perangkat Anda...");
                    DrawTextW(
                        hdc,
                        sub_txt.as_ptr(),
                        -1,
                        &mut sub_rc,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );

                    SelectObject(hdc, old_f);
                    DeleteObject(hfont_title);
                    DeleteObject(hfont_sub);
                } else {
                    // Mode 3: Black Screen
                    let blk_br = CreateSolidBrush(0x00000000);
                    FillRect(hdc, &full_rc, blk_br);
                    DeleteObject(blk_br);
                }

                EndPaint(hwnd, &ps);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe extern "system" fn ll_keyboard_proc(n_code: i32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        if n_code >= 0 && lparam != 0 {
            let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
            let is_injected = (kb.flags & LLKHF_INJECTED) != 0;

            // 1. Host Mode: Block physical keyboard input on remote host if enabled
            if HOST_BLOCK_INPUT.load(Ordering::Relaxed) && !is_injected {
                return 1;
            }

            // 2. Viewer Mode: Forward Windows key, Win+Key, Alt+Tab, Alt+F4 directly to remote PC!
            if VIEWER_SESSION_ACTIVE.load(Ordering::Relaxed) && !is_injected {
                let main_hwnd = VIEWER_MAIN_HWND.load(Ordering::Relaxed) as HWND;
                if !main_hwnd.is_null() && GetForegroundWindow() == main_hwnd {
                    let is_down = wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN;
                    let is_up = wparam == WM_KEYUP || wparam == WM_SYSKEYUP;
                    let alt_down = (kb.flags & LLKHF_ALTDOWN) != 0;
                    let vk = kb.vk_code;

                    if vk == VK_LWIN || vk == VK_RWIN {
                        WIN_KEY_HELD.store(is_down, Ordering::Relaxed);
                        send_hook_key_event(if is_down { "keydown" } else { "keyup" }, 0x5B, false);
                        return 1; // Prevent local Start Menu from popping up!
                    }

                    if WIN_KEY_HELD.load(Ordering::Relaxed) && (is_down || is_up) {
                        send_hook_key_event(
                            if is_down { "keydown" } else { "keyup" },
                            vk as u8,
                            false,
                        );
                        return 1; // Prevent local Win+E / Win+R / Win+D from triggering locally!
                    }

                    if alt_down && (vk == VK_TAB || vk == VK_F4) {
                        if is_down {
                            send_hook_key_event("keycombo", vk as u8, true);
                        }
                        return 1; // Prevent local Alt+Tab or Alt+F4 closing OpenDesk!
                    }
                } else {
                    WIN_KEY_HELD.store(false, Ordering::Relaxed);
                }
            }
        }
        CallNextHookEx(ptr::null_mut(), n_code, wparam, lparam)
    }
}

unsafe extern "system" fn ll_mouse_proc(n_code: i32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        if n_code >= 0 && lparam != 0 {
            let ms = &*(lparam as *const MSLLHOOKSTRUCT);
            let is_injected = (ms.flags & LLMHF_INJECTED) != 0;

            // Host Mode: Block physical mouse movement/clicks on remote host if enabled
            if HOST_BLOCK_INPUT.load(Ordering::Relaxed) && !is_injected {
                return 1;
            }
        }
        CallNextHookEx(ptr::null_mut(), n_code, wparam, lparam)
    }
}

fn send_hook_key_event(action: &str, vk: u8, alt_combo: bool) {
    if let Some(lock) = HOOK_CTX.get() {
        if let Ok(guard) = lock.lock() {
            if let Some(ref ctx) = *guard {
                if !ctx.partner_id.is_empty() {
                    let msg = if alt_combo {
                        json!({
                            "type": "relay-input",
                            "targetClean": ctx.partner_id,
                            "event": {
                                "action": "keycombo",
                                "ctrl": false,
                                "alt": true,
                                "shift": false,
                                "vk": vk
                            }
                        })
                    } else {
                        json!({
                            "type": "relay-input",
                            "targetClean": ctx.partner_id,
                            "event": {
                                "action": action,
                                "keyCode": vk
                            }
                        })
                    };
                    let _ = ctx.tx_out.send(OutgoingMsg::Json(msg));
                }
            }
        }
    }
}

/// Initializes the background Win32 thread for Privacy Overlay + Low-Level Input Hooks.
pub fn init_privacy_and_hooks(tx_out: Sender<OutgoingMsg>) {
    let lock = HOOK_CTX.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = lock.lock() {
        *guard = Some(HookContext {
            tx_out,
            partner_id: String::new(),
        });
    }

    thread::spawn(move || unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let class_name = to_wide("OpenDeskPrivacyOverlayClass");
        let win_name = to_wide("OpenDeskPrivacyOverlay");

        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cb_size = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfn_wnd_proc = Some(overlay_wnd_proc);
        wc.h_instance = hinst;
        wc.lpsz_class_name = class_name.as_ptr();
        RegisterClassExW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            win_name.as_ptr(),
            WS_POPUP,
            0,
            0,
            100,
            100,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null_mut(),
        );

        if !hwnd.is_null() {
            SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
            if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) == 0 {
                SetWindowDisplayAffinity(hwnd, WDA_MONITOR);
            }
            OVERLAY_HWND.store(hwnd as usize, Ordering::SeqCst);
        }

        let _kb_hook = SetWindowsHookExW(WH_KEYBOARD_LL, ll_keyboard_proc, hinst, 0);
        let _ms_hook = SetWindowsHookExW(WH_MOUSE_LL, ll_mouse_proc, hinst, 0);

        logger::info(
            "desk_rt.privacy",
            "Win32 Privacy Overlay (WDA_EXCLUDEFROMCAPTURE) & Low-Level Keyboard/Mouse Hooks active.",
        );

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

/// Updates the active viewer state so the Low-Level Keyboard Hook knows when to forward Win/Alt+Tab keys.
pub fn set_viewer_hook_state(active: bool, partner_clean: &str, window_title: &str) {
    if let Some(lock) = HOOK_CTX.get() {
        if let Ok(mut guard) = lock.lock() {
            if let Some(ref mut ctx) = *guard {
                ctx.partner_id = partner_clean.to_string();
            }
        }
    }
    if active && !window_title.is_empty() {
        unsafe {
            let wide = to_wide(window_title);
            let hwnd = FindWindowW(ptr::null(), wide.as_ptr());
            if !hwnd.is_null() {
                VIEWER_MAIN_HWND.store(hwnd as usize, Ordering::SeqCst);
            }
        }
    }
    if !active {
        WIN_KEY_HELD.store(false, Ordering::SeqCst);
    }
    VIEWER_SESSION_ACTIVE.store(active, Ordering::SeqCst);
}

/// Applies privacy mode (0=Off, 1=Freeze, 2=Maintenance, 3=Black) on the Host machine.
pub fn set_host_privacy_mode(mode: i32) {
    unsafe {
        let prev = HOST_PRIVACY_MODE.load(Ordering::SeqCst);
        if mode == 1 && prev != 1 {
            // Hide overlay briefly if another mode was active so we capture the true live desktop
            let hwnd = OVERLAY_HWND.load(Ordering::SeqCst) as HWND;
            if prev != 0 && !hwnd.is_null() {
                ShowWindow(hwnd, SW_HIDE);
                thread::sleep(Duration::from_millis(25));
            }
            capture_freeze_snapshot();
        }
        HOST_PRIVACY_MODE.store(mode, Ordering::SeqCst);
        let hwnd = OVERLAY_HWND.load(Ordering::SeqCst) as HWND;
        if !hwnd.is_null() {
            PostMessageW(hwnd, WM_USER_UPDATE_PRIVACY, 0, 0);
        }
        logger::info(
            "desk_rt.privacy",
            &format!("Host privacy screen mode set to {}", mode),
        );
    }
}

/// Enables or disables blocking physical mouse & keyboard on the Host machine.
pub fn set_host_block_input(blocked: bool) {
    HOST_BLOCK_INPUT.store(blocked, Ordering::SeqCst);
    unsafe {
        let _ = BlockInput(if blocked { 1 } else { 0 });
    }
    logger::info(
        "desk_rt.privacy",
        &format!("Host physical input lock set to {}", blocked),
    );
}

/// Resets all Host privacy overlays and input locks (called automatically when session ends).
pub fn reset_host_privacy() {
    set_host_block_input(false);
    set_host_privacy_mode(0);
}
