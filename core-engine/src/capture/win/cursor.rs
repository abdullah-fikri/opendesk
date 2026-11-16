//! Host cursor shape detection.

use std::ptr;

use super::win32::*;

/// Current host cursor as a portable shape code: 0 hidden, 1 arrow, 2 text, 3 hand, 4 wait, 5 progress,
/// 6 crosshair, 7 move, 8 ew-resize, 9 ns-resize, 10 nwse-resize, 11 nesw-resize, 12 not-allowed, 13 help.
/// App-specific cursors fall back to arrow.
pub fn cursor_shape() -> u8 {
    // IDC_* resource IDs in the same order as shape codes 1..=13.
    const IDS: [u16; 13] = [32512, 32513, 32649, 32514, 32650, 32515, 32646, 32644, 32645, 32642, 32643, 32648, 32651];
    static HANDLES: std::sync::OnceLock<[usize; 13]> = std::sync::OnceLock::new();
    let handles = HANDLES
        .get_or_init(|| IDS.map(|id| unsafe { LoadCursorW(ptr::null_mut(), id as usize as *const u16) } as usize));
    let mut ci = CURSORINFO {
        cb_size: std::mem::size_of::<CURSORINFO>() as DWORD,
        flags: 0,
        h_cursor: ptr::null_mut(),
        pt_screen_pos: POINT { x: 0, y: 0 },
    };
    if unsafe { GetCursorInfo(&mut ci) } == 0 || (ci.flags & CURSOR_SHOWING) == 0 {
        return 0;
    }
    handles.iter().position(|&h| h == ci.h_cursor as usize).map_or(1, |i| i as u8 + 1)
}

const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;

#[repr(C)]
#[derive(Default)]
struct ClipRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetClipCursor(rect: *mut ClipRect) -> BOOL;
}

/// True while an application captured the mouse the way games do for mouse look: the cursor is hidden and
/// confined to less than the whole virtual desktop. Such an application reads relative motion, so absolute
/// pointer positions from the viewer would spin its camera. A cursor hidden only while typing is not confined.
pub fn pointer_captured() -> bool {
    let mut ci = CURSORINFO {
        cb_size: std::mem::size_of::<CURSORINFO>() as DWORD,
        flags: 0,
        h_cursor: ptr::null_mut(),
        pt_screen_pos: POINT { x: 0, y: 0 },
    };
    // SAFETY: plain out-parameters sized as the API expects.
    unsafe {
        if GetCursorInfo(&mut ci) == 0 || (ci.flags & CURSOR_SHOWING) != 0 {
            return false;
        }
        let mut clip = ClipRect::default();
        if GetClipCursor(&mut clip) == 0 {
            return false;
        }
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        clip.left > vx || clip.top > vy || clip.right < vx + vw || clip.bottom < vy + vh
    }
}
