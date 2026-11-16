//! Pointer lock for relative mouse input (`mouse-relative`): while a host application captured the mouse (a game
//! with mouse look), the viewer confines its cursor to the viewer window, keeps it at the window's centre and turns
//! every move into a delta. Holding Ctrl+Alt hands the cursor back (as in virtual machine consoles); a click on
//! the remote screen takes it again. Windows only; elsewhere the lock never engages and the viewer keeps sending
//! absolute positions.

use std::sync::atomic::{AtomicBool, Ordering};

/// The user's setting (Settings › Mouse › Pointer lock); off keeps absolute positions, as before the lock.
static ALLOWED: AtomicBool = AtomicBool::new(false);
/// The focused host asked for the lock.
static LOCKED: AtomicBool = AtomicBool::new(false);
/// The cursor is confined to the viewer window right now.
static CLIPPED: AtomicBool = AtomicBool::new(false);
/// The user took the cursor back with Ctrl+Alt; a click on the remote screen ends it.
static USER_RELEASED: AtomicBool = AtomicBool::new(false);

/// What a mouse move turned into.
#[derive(Debug, PartialEq, Eq)]
pub enum Motion {
    /// Not locked: send the absolute position as usual.
    Absolute,
    /// Locked: send this relative motion (nothing when it is zero).
    Delta(i32, i32),
    /// Locked, but the user just took the cursor back: show it again.
    Released,
}

/// Applies the pointer lock setting (read once at start and on every change).
pub fn set_allowed(on: bool) {
    ALLOWED.store(on, Ordering::SeqCst);
}

/// Whether the user lets hosts lock the pointer.
pub fn is_allowed() -> bool {
    ALLOWED.load(Ordering::SeqCst)
}

/// Starts (`on`) or ends the lock the host asked for.
pub fn set_locked(on: bool) {
    if LOCKED.swap(on, Ordering::SeqCst) == on {
        return;
    }
    USER_RELEASED.store(false, Ordering::SeqCst);
    if !on {
        unclip();
    }
}

/// True while the user took the cursor back from a lock the host still holds.
pub fn is_user_released() -> bool {
    LOCKED.load(Ordering::SeqCst) && USER_RELEASED.load(Ordering::SeqCst)
}

/// Takes the cursor again after Ctrl+Alt (a click on the remote screen).
pub fn resume() {
    USER_RELEASED.store(false, Ordering::SeqCst);
}

/// Frees the cursor while the viewer is in the background or another session has focus; the next move over the
/// remote screen confines it again.
pub fn suspend() {
    unclip();
}

/// Turns a mouse move over the remote screen into what to send.
pub fn on_move() -> Motion {
    if !LOCKED.load(Ordering::SeqCst) {
        return Motion::Absolute;
    }
    if USER_RELEASED.load(Ordering::SeqCst) {
        return Motion::Delta(0, 0);
    }
    if sys::ctrl_alt_held() {
        USER_RELEASED.store(true, Ordering::SeqCst);
        unclip();
        return Motion::Released;
    }
    let Some((left, top, right, bottom)) = sys::viewer_client_rect() else {
        unclip();
        return Motion::Delta(0, 0);
    };
    let (cx, cy) = ((left + right) / 2, (top + bottom) / 2);
    if !CLIPPED.swap(true, Ordering::SeqCst) {
        sys::clip(Some((left, top, right, bottom)));
        sys::set_pos(cx, cy);
        return Motion::Delta(0, 0);
    }
    let (x, y) = sys::pos();
    let (dx, dy) = (x - cx, y - cy);
    if dx != 0 || dy != 0 {
        sys::set_pos(cx, cy);
    }
    Motion::Delta(dx, dy)
}

fn unclip() {
    if CLIPPED.swap(false, Ordering::SeqCst) {
        sys::clip(None);
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Point {
        x: i32,
        y: i32,
    }

    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetForegroundWindow() -> *mut c_void;
        fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
        fn GetClientRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
        fn ClientToScreen(hwnd: *mut c_void, point: *mut Point) -> i32;
        fn ClipCursor(rect: *const Rect) -> i32;
        fn GetCursorPos(point: *mut Point) -> i32;
        fn SetCursorPos(x: i32, y: i32) -> i32;
        fn GetAsyncKeyState(vk: i32) -> i16;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcessId() -> u32;
    }

    /// Screen rectangle of the client area of the viewer window in front; None when another process's window
    /// is in front.
    pub fn viewer_client_rect() -> Option<(i32, i32, i32, i32)> {
        // SAFETY: plain calls with out-parameters that outlive them; a stale handle only fails.
        unsafe {
            let fg = GetForegroundWindow();
            let mut pid = 0u32;
            if fg.is_null() || GetWindowThreadProcessId(fg, &mut pid) == 0 || pid != GetCurrentProcessId() {
                return None;
            }
            let mut r = Rect::default();
            let mut origin = Point::default();
            if GetClientRect(fg, &mut r) == 0 || ClientToScreen(fg, &mut origin) == 0 || r.right < 8 || r.bottom < 8 {
                return None;
            }
            Some((origin.x, origin.y, origin.x + r.right, origin.y + r.bottom))
        }
    }

    pub fn clip(rect: Option<(i32, i32, i32, i32)>) {
        // SAFETY: the rectangle outlives the call; null frees the cursor.
        unsafe {
            match rect {
                Some((left, top, right, bottom)) => ClipCursor(&Rect { left, top, right, bottom }),
                None => ClipCursor(std::ptr::null()),
            };
        }
    }

    pub fn pos() -> (i32, i32) {
        let mut p = Point::default();
        // SAFETY: out-parameter.
        unsafe { GetCursorPos(&mut p) };
        (p.x, p.y)
    }

    pub fn set_pos(x: i32, y: i32) {
        // SAFETY: plain values.
        unsafe { SetCursorPos(x, y) };
    }

    pub fn ctrl_alt_held() -> bool {
        // SAFETY: plain values.
        unsafe { GetAsyncKeyState(VK_CONTROL) < 0 && GetAsyncKeyState(VK_MENU) < 0 }
    }
}

#[cfg(not(windows))]
mod sys {
    pub fn viewer_client_rect() -> Option<(i32, i32, i32, i32)> {
        None
    }
    pub fn clip(_: Option<(i32, i32, i32, i32)>) {}
    pub fn pos() -> (i32, i32) {
        (0, 0)
    }
    pub fn set_pos(_: i32, _: i32) {}
    pub fn ctrl_alt_held() -> bool {
        false
    }
}
