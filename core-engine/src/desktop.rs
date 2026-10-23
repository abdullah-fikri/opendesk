//! Attaching capture threads to the current input desktop (Default, Winlogon, Screen-saver).

//! Input-desktop following for a SYSTEM agent: the UAC prompt and the lock screen live on the
//! "Winlogon" desktop, so capture and input threads must attach to whichever desktop receives input.
//! Without SYSTEM rights OpenInputDesktop fails there and these calls are harmless no-ops.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr;

type HDESK = *mut c_void;
type BOOL = i32;

const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
const UOI_NAME: i32 = 2;

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenInputDesktop(dw_flags: u32, f_inherit: BOOL, dw_desired_access: u32) -> HDESK;
    fn GetThreadDesktop(dw_thread_id: u32) -> HDESK;
    fn SetThreadDesktop(h_desktop: HDESK) -> BOOL;
    fn CloseDesktop(h_desktop: HDESK) -> BOOL;
    fn GetUserObjectInformationW(
        h_obj: *mut c_void,
        n_index: i32,
        pv_info: *mut c_void,
        n_length: u32,
        lpn_length_needed: *mut u32,
    ) -> BOOL;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThreadId() -> u32;
}

thread_local! {
    /// Desktop handle this thread opened and attached to (closed when replaced).
    static ATTACHED: Cell<HDESK> = const { Cell::new(ptr::null_mut()) };
}

fn desktop_name(h: HDESK) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut needed = 0u32;
    let ok = unsafe {
        GetUserObjectInformationW(h, UOI_NAME, buf.as_mut_ptr() as *mut c_void, (buf.len() * 2) as u32, &mut needed)
    };
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Attaches the calling thread to the input desktop if it changed. Returns true when the thread switched,
/// so callers must recreate desktop-bound resources (GDI DCs, DXGI duplication).
/// The thread must not own windows or hooks, or SetThreadDesktop fails (then this returns false).
pub fn switch_to_input_desktop() -> bool {
    unsafe {
        let input = OpenInputDesktop(0, 0, MAXIMUM_ALLOWED);
        if input.is_null() {
            return false;
        }
        let current = GetThreadDesktop(GetCurrentThreadId());
        if !current.is_null() && desktop_name(current) == desktop_name(input) {
            CloseDesktop(input);
            return false;
        }
        if SetThreadDesktop(input) == 0 {
            CloseDesktop(input);
            return false;
        }
        let previous = ATTACHED.with(|a| a.replace(input));
        if !previous.is_null() {
            CloseDesktop(previous);
        }
        true
    }
}
