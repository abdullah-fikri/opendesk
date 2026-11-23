//! Windows focus reading: UI Automation's focused element, with the caret (`GetGUIThreadInfo`) for classic
//! Win32 controls UI Automation says nothing useful about.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern, UIA_TextPatternId,
    UIA_ValuePatternId,
};

use super::FocusReading;
use crate::infra::clipboard_helper::{self, ToAgent};
use crate::infra::logger;

type Hwnd = *mut c_void;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct GuiThreadInfo {
    cb_size: u32,
    flags: u32,
    hwnd_active: Hwnd,
    hwnd_focus: Hwnd,
    hwnd_capture: Hwnd,
    hwnd_menu_owner: Hwnd,
    hwnd_move_size: Hwnd,
    hwnd_caret: Hwnd,
    rc_caret: Rect,
}

const GUI_CARETBLINKING: u32 = 0x1;
const GWL_STYLE: i32 = -16;
const ES_PASSWORD: i32 = 0x20;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetGUIThreadInfo(thread: u32, info: *mut GuiThreadInfo) -> i32;
    fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
    fn GetClassNameW(hwnd: Hwnd, name: *mut u16, len: i32) -> i32;
    fn GetWindowLongW(hwnd: Hwnd, index: i32) -> i32;
}

// UI Automation control type IDs.
const EDIT: i32 = 50004;
const COMBO_BOX: i32 = 50003;
const DOCUMENT: i32 = 50030;
const PANE: i32 = 50033;
const CUSTOM: i32 = 50025;
const GROUP: i32 = 50026;
const WINDOW: i32 = 50032;

/// Window classes of terminals: their text area takes typing but is no edit control.
const TERMINAL_CLASSES: [&str; 6] =
    ["ConsoleWindowClass", "TermControl", "CASCADIA_HOSTING_WINDOW_CLASS", "VirtualConsoleClass", "PuTTY", "mintty"];

/// Whether a focused UI Automation element takes typed text: `Some` when its type settles it, `None` when the
/// caret should decide (panes and custom controls of applications without real UI Automation support).
/// `value_writable`: it has a Value pattern that is (true) or is not (false) writable.
fn classify(control_type: i32, value_writable: Option<bool>, has_text: bool, class: &str) -> Option<bool> {
    if TERMINAL_CLASSES.contains(&class) {
        return Some(true);
    }
    match control_type {
        EDIT => Some(value_writable.unwrap_or(true)),
        COMBO_BOX => Some(value_writable == Some(true)),
        // A browser page is a read-only document; a rich editor without a Value pattern shows its caret.
        DOCUMENT => value_writable,
        PANE | CUSTOM | GROUP | WINDOW => {
            if value_writable == Some(true) && has_text {
                Some(true)
            } else {
                None
            }
        }
        _ => Some(false),
    }
}

/// Reads focus on the calling thread.
pub struct Probe {
    uia: Option<IUIAutomation>,
    com: bool,
}

impl Probe {
    pub fn new(uia: bool) -> Self {
        if !uia {
            return Self { uia: None, com: false };
        }
        // SAFETY: COM is initialized for this thread and released in `drop`, after the interface.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let uia = com
            .then(|| unsafe { CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) })
            .and_then(|r| r.map_err(|e| logger::warn("text_focus", &format!("UI Automation unavailable: {e}"))).ok());
        Self { uia, com }
    }

    pub fn read(&mut self) -> FocusReading {
        let caret = caret_reading();
        let Some(uia) = &self.uia else { return caret.unwrap_or_default() };
        match unsafe { uia.GetFocusedElement() } {
            Ok(el) => match element_reading(&el) {
                Some(reading) => reading,
                None => caret.unwrap_or_default(),
            },
            Err(_) => caret.unwrap_or_default(),
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.uia = None;
        if self.com {
            unsafe { CoUninitialize() };
        }
    }
}

/// What UI Automation says about the focused element; `None` lets the caret decide.
fn element_reading(el: &IUIAutomationElement) -> Option<FocusReading> {
    // SAFETY: plain property reads on a live element; failures are errors, not undefined behavior.
    unsafe {
        let control_type = el.CurrentControlType().ok()?.0;
        let class = el.CurrentClassName().map(|b| b.to_string()).unwrap_or_default();
        let value_writable = el
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            .ok()
            .and_then(|p| p.CurrentIsReadOnly().ok())
            .map(|read_only| !read_only.as_bool());
        let has_text = el.GetCurrentPattern(UIA_TextPatternId).is_ok();
        let editable = classify(control_type, value_writable, has_text, &class)?
            && el.CurrentIsEnabled().is_ok_and(|b| b.as_bool());
        if !editable {
            return Some(FocusReading::default());
        }
        let password = el.CurrentIsPassword().is_ok_and(|b| b.as_bool());
        let r = el.CurrentBoundingRectangle().unwrap_or_default();
        Some(FocusReading { editable, password, left: r.left, top: r.top, right: r.right, bottom: r.bottom })
    }
}

fn class_name(hwnd: Hwnd) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The caret of the foreground thread (classic Win32 edits, terminals); `None` when it shows none.
fn caret_reading() -> Option<FocusReading> {
    let mut info = GuiThreadInfo {
        cb_size: std::mem::size_of::<GuiThreadInfo>() as u32,
        flags: 0,
        hwnd_active: std::ptr::null_mut(),
        hwnd_focus: std::ptr::null_mut(),
        hwnd_capture: std::ptr::null_mut(),
        hwnd_menu_owner: std::ptr::null_mut(),
        hwnd_move_size: std::ptr::null_mut(),
        hwnd_caret: std::ptr::null_mut(),
        rc_caret: Rect::default(),
    };
    // SAFETY: `info` is sized as the API expects; thread 0 = the foreground thread.
    if unsafe { GetGUIThreadInfo(0, &mut info) } == 0 {
        return None;
    }
    let (hwnd, terminal) = if !info.hwnd_caret.is_null() && info.flags & GUI_CARETBLINKING != 0 {
        (info.hwnd_caret, false)
    } else if !info.hwnd_focus.is_null() && TERMINAL_CLASSES.contains(&class_name(info.hwnd_focus).as_str()) {
        (info.hwnd_focus, true)
    } else {
        return None;
    };
    let mut r = Rect::default();
    unsafe { GetWindowRect(hwnd, &mut r) };
    let password = !terminal
        && class_name(hwnd).eq_ignore_ascii_case("Edit")
        && unsafe { GetWindowLongW(hwnd, GWL_STYLE) } & ES_PASSWORD != 0;
    Some(FocusReading { editable: true, password, left: r.left, top: r.top, right: r.right, bottom: r.bottom })
}

// ---------------------------------------------------------------------------------------------
// Helper process: watches focus while the agent keeps asking.

/// The agent repeats its request this often; the helper stops watching when it has not heard one for
/// [`WATCH_LEASE`] (agent gone, session over).
pub const KEEPALIVE: Duration = Duration::from_secs(2);
const WATCH_LEASE: Duration = Duration::from_secs(5);

static LEASE: Mutex<Option<Instant>> = Mutex::new(None);
static WATCHING: AtomicBool = AtomicBool::new(false);
/// Set by every request: the next reading goes out even if unchanged (a new session starts from nothing).
static RESEND: AtomicBool = AtomicBool::new(false);

/// Helper: the agent asks to watch focus (`on`) or to stop.
pub fn helper_watch(on: bool) {
    *LEASE.lock() = on.then(Instant::now);
    RESEND.store(on, Ordering::SeqCst);
    if !on || WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::infra::thread::spawn_named("text-focus", || {
        let mut probe = Probe::new(true);
        let mut last = None;
        while LEASE.lock().is_some_and(|t| t.elapsed() < WATCH_LEASE) {
            let reading = probe.read();
            let resend = RESEND.swap(false, Ordering::SeqCst);
            if (resend || last != Some(reading)) && clipboard_helper::to_agent(&ToAgent::TextFocus(reading)) {
                last = Some(reading);
            }
            std::thread::sleep(super::POLL);
        }
        WATCHING.store(false, Ordering::SeqCst);
    });
}

/// Agent: the last reading its helper reported.
static FROM_HELPER: Mutex<Option<FocusReading>> = Mutex::new(None);

/// Agent: the helper reported a change.
pub fn from_helper(reading: FocusReading) {
    *FROM_HELPER.lock() = Some(reading);
}

/// Agent: the helper's last reading, if it sent one since [`reset_helper_reading`].
pub fn helper_reading() -> Option<FocusReading> {
    *FROM_HELPER.lock()
}

/// Agent: forgets the helper's reading (a new watch, or a helper that went away).
pub fn reset_helper_reading() {
    *FROM_HELPER.lock() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_take_text_unless_read_only() {
        assert_eq!(classify(EDIT, None, false, "Edit"), Some(true));
        assert_eq!(classify(EDIT, Some(true), true, ""), Some(true));
        assert_eq!(classify(EDIT, Some(false), true, ""), Some(false), "read-only box");
    }

    #[test]
    fn documents_and_combos_need_a_writable_value() {
        assert_eq!(classify(DOCUMENT, Some(false), true, "Chrome_RenderWidgetHostHWND"), Some(false), "web page");
        assert_eq!(classify(DOCUMENT, Some(true), true, "RichEditD2DPT"), Some(true), "Notepad");
        assert_eq!(classify(DOCUMENT, None, true, "_WwG"), None, "caret decides");
        assert_eq!(classify(COMBO_BOX, None, false, ""), Some(false), "drop-down list");
        assert_eq!(classify(COMBO_BOX, Some(true), false, ""), Some(true));
    }

    #[test]
    fn panes_defer_to_the_caret_and_buttons_do_not_type() {
        assert_eq!(classify(PANE, None, false, ""), None);
        assert_eq!(classify(GROUP, Some(true), true, ""), Some(true), "contenteditable");
        assert_eq!(classify(50000, None, false, "Button"), Some(false));
        assert_eq!(classify(50000, None, false, "ConsoleWindowClass"), Some(true), "terminal");
    }
}

#[cfg(test)]
mod live {
    /// Manual check on a desktop: `cargo test live_probe -- --ignored --nocapture` opens Notepad and prints what
    /// the probe reads while it has focus.
    #[test]
    #[ignore]
    fn live_probe_reads_notepad() {
        let mut child = std::process::Command::new("notepad.exe").spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        let mut probe = super::Probe::new(true);
        for _ in 0..5 {
            println!("{:?}", probe.read());
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}
