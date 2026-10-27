//! System tray icon and its context menu (show, disconnect, reconnect, restart, exit).

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

type HWND = *mut c_void;
type HICON = *mut c_void;
type HMENU = *mut c_void;
type BOOL = i32;

const NIM_ADD: u32 = 0x00000000;
const NIM_DELETE: u32 = 0x00000002;

const NIF_MESSAGE: u32 = 0x00000001;
const NIF_ICON: u32 = 0x00000002;
const NIF_TIP: u32 = 0x00000004;

const WM_NULL: u32 = 0x0000;
const WM_USER: u32 = 0x0400;
/// Window message the tray icon sends; a second app instance posts it to wake the first.
pub const WM_TRAYICON: u32 = WM_USER + 101;

const WM_LBUTTONUP: u32 = 0x0202;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_CONTEXTMENU: u32 = 0x007B;

const MF_STRING: u32 = 0x00000000;
const MF_SEPARATOR: u32 = 0x00000800;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_NONOTIFY: u32 = 0x0080;
const TPM_RETURNCMD: u32 = 0x0100;

const SW_RESTORE: i32 = 9;
const SW_SHOW: i32 = 5;

const CMD_SHOW_WINDOW: usize = 1001;
const CMD_DISCONNECT_SESSION: usize = 1002;
const CMD_RECONNECT_RELAY: usize = 1003;
const CMD_RESTART_APP: usize = 1004;
const CMD_EXIT_APP: usize = 1005;

pub use super::TrayAction;

/// Handler for tray menu choices.
type TrayHandler = Box<dyn Fn(TrayAction) + Send + Sync>;

static TRAY_CALLBACK: OnceLock<Mutex<Option<TrayHandler>>> = OnceLock::new();

fn dispatch_tray_action(action: TrayAction) {
    if let Some(lock) = TRAY_CALLBACK.get()
        && let Ok(guard) = lock.lock()
        && let Some(cb) = guard.as_ref()
    {
        cb(action);
    }
}

#[repr(C)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
struct WNDCLASSEXW {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: Option<unsafe extern "system" fn(HWND, u32, usize, isize) -> isize>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: *mut c_void,
    h_icon: HICON,
    h_cursor: *mut c_void,
    hbr_background: *mut c_void,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: HICON,
}

#[repr(C)]
struct NOTIFYICONDATAW {
    cb_size: u32,
    h_wnd: HWND,
    u_id: u32,
    u_flags: u32,
    u_callback_message: u32,
    h_icon: HICON,
    sz_tip: [u16; 128],
    dw_state: u32,
    dw_state_mask: u32,
    sz_info: [u16; 256],
    u_timeout_or_version: u32,
    sz_info_title: [u16; 64],
    dw_info_flags: u32,
    guid_item: [u8; 16],
    h_balloon_icon: HICON,
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
        dwExStyle: u32,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: u32,
        X: i32,
        Y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: *mut c_void,
        hInstance: *mut c_void,
        lpParam: *mut c_void,
    ) -> HWND;
    fn DestroyWindow(hWnd: HWND) -> BOOL;
    fn DefWindowProcW(hWnd: HWND, Msg: u32, wParam: usize, lParam: isize) -> isize;
    fn LoadImageW(hInst: *mut c_void, name: *const u16, type_: u32, cx: i32, cy: i32, fuLoad: u32) -> HICON;
    fn DestroyIcon(hIcon: HICON) -> BOOL;
    fn PeekMessageW(lpMsg: *mut MSG, hWnd: HWND, wMsgFilterMin: u32, wMsgFilterMax: u32, wRemoveMsg: u32) -> BOOL;
    fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    fn DispatchMessageW(lpMsg: *const MSG) -> isize;
    fn CreatePopupMenu() -> HMENU;
    fn AppendMenuW(hMenu: HMENU, uFlags: u32, uIDNewItem: usize, lpNewItem: *const u16) -> BOOL;
    fn TrackPopupMenu(
        hMenu: HMENU,
        uFlags: u32,
        x: i32,
        y: i32,
        nReserved: i32,
        hWnd: HWND,
        prcRect: *const c_void,
    ) -> BOOL;
    fn DestroyMenu(hMenu: HMENU) -> BOOL;
    fn GetCursorPos(lpPoint: *mut POINT) -> BOOL;
    fn SetForegroundWindow(hWnd: HWND) -> BOOL;
    fn PostMessageW(hWnd: HWND, Msg: u32, wParam: usize, lParam: isize) -> BOOL;
    fn FindWindowW(lpClassName: *const u16, lpWindowName: *const u16) -> HWND;
    fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(lp_module_name: *const u16) -> *mut c_void;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn Shell_NotifyIconW(dwMessage: u32, lpData: *const NOTIFYICONDATAW) -> BOOL;
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Restores, shows and focuses the window titled `window_title`.
pub fn bring_main_window_to_front(window_title: &str) {
    unsafe {
        let wide_title = to_wide(window_title);
        let hwnd = FindWindowW(ptr::null(), wide_title.as_ptr());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_RESTORE);
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        }
    }
}

unsafe fn show_tray_context_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }

        let lbl_open = to_wide("Open OpenDesk");
        let lbl_disc = to_wide("Disconnect Remote Session");
        let lbl_recon = to_wide("Reconnect to Relay");
        let lbl_restart = to_wide("Restart");
        let lbl_exit = to_wide("Stop && Exit");

        AppendMenuW(menu, MF_STRING, CMD_SHOW_WINDOW, lbl_open.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, CMD_DISCONNECT_SESSION, lbl_disc.as_ptr());
        AppendMenuW(menu, MF_STRING, CMD_RECONNECT_RELAY, lbl_recon.as_ptr());
        AppendMenuW(menu, MF_STRING, CMD_RESTART_APP, lbl_restart.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, CMD_EXIT_APP, lbl_exit.as_ptr());

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);

        // Required by Win32 so clicking outside closes the popup menu properly
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, ptr::null())
            as usize;
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);

        match cmd {
            CMD_SHOW_WINDOW => dispatch_tray_action(TrayAction::ShowWindow),
            CMD_DISCONNECT_SESSION => dispatch_tray_action(TrayAction::DisconnectSession),
            CMD_RECONNECT_RELAY => dispatch_tray_action(TrayAction::ReconnectRelay),
            CMD_RESTART_APP => dispatch_tray_action(TrayAction::RestartApp),
            CMD_EXIT_APP => dispatch_tray_action(TrayAction::ExitApp),
            _ => {}
        }
    }
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        if msg == WM_TRAYICON {
            let event = (lparam as u32) & 0xFFFF;
            match event {
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    show_tray_context_menu(hwnd);
                    return 0;
                }
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    dispatch_tray_action(TrayAction::ShowWindow);
                    return 0;
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

/// The tray icon and its message window. Dropping it removes the icon.
pub struct SystemTray {
    is_running: Arc<AtomicBool>,
}

impl SystemTray {
    /// Adds the tray icon with `tooltip`; menu choices call `on_action` on the tray thread.
    pub fn new<F>(tooltip: &str, on_action: F) -> Self
    where
        F: Fn(TrayAction) + Send + Sync + 'static,
    {
        let lock = TRAY_CALLBACK.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = lock.lock() {
            *guard = Some(Box::new(on_action));
        }

        let is_running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&is_running);
        let tip_str = tooltip.to_string();

        thread::spawn(move || unsafe {
            let class_name = to_wide("OpenDeskTrayMsgClass");
            let win_name = to_wide("OpenDeskTrayMessageWindow");
            let hinst = GetModuleHandleW(ptr::null());

            let mut wc = std::mem::zeroed::<WNDCLASSEXW>();
            wc.cb_size = std::mem::size_of::<WNDCLASSEXW>() as u32;
            wc.lpfn_wnd_proc = Some(tray_proc);
            wc.h_instance = hinst;
            wc.lpsz_class_name = class_name.as_ptr();

            RegisterClassExW(&wc);

            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                win_name.as_ptr(),
                0x80000000, // WS_POPUP
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                hinst,
                ptr::null_mut(),
            );

            // Load icon: the embedded resource 1 (icon.rc; MAKEINTRESOURCE(1)), fallback to icon.ico
            let mut hicon = LoadImageW(hinst, ptr::without_provenance::<u16>(1), 1, 16, 16, 0);
            if hicon.is_null() {
                let icon_file = to_wide("icon.ico");
                hicon = LoadImageW(
                    ptr::null_mut(),
                    icon_file.as_ptr(),
                    1, // IMAGE_ICON
                    16,
                    16,
                    0x00000010, // LR_LOADFROMFILE
                );
            }

            let mut nid = std::mem::zeroed::<NOTIFYICONDATAW>();
            nid.cb_size = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.h_wnd = hwnd;
            nid.u_id = 1001;
            nid.u_flags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
            nid.u_callback_message = WM_TRAYICON;
            nid.h_icon = hicon;

            let wide_tip: Vec<u16> = tip_str.encode_utf16().take(127).chain(std::iter::once(0)).collect();
            for (i, &w) in wide_tip.iter().enumerate() {
                nid.sz_tip[i] = w;
            }

            let mut added = false;
            for _ in 0..10 {
                if Shell_NotifyIconW(NIM_ADD, &nid) != 0 {
                    added = true;
                    break;
                }
                thread::sleep(Duration::from_millis(200));
            }

            let mut msg = std::mem::zeroed::<MSG>();
            while running_clone.load(Ordering::SeqCst) {
                while PeekMessageW(&mut msg, hwnd, 0, 0, 1) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                thread::sleep(Duration::from_millis(30));
            }

            if added {
                Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            if !hicon.is_null() {
                DestroyIcon(hicon);
            }
            if !hwnd.is_null() {
                DestroyWindow(hwnd);
            }
        });

        Self { is_running }
    }
}

impl Drop for SystemTray {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}
