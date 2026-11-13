//! The terminal a CLI command writes to. OpenDesk is a GUI-subsystem program on Windows, so it first attaches
//! to the console of the shell that started it; output then goes through `std::io::stdout()`, which writes
//! UTF-16 to a console and bytes to a file or pipe. Also reports what that output can show (escape sequences,
//! Unicode, width) and, on Windows, catches Ctrl+C for a command that only watches the service.

use std::io::Write;

/// What the output of this process is and what it can display.
pub struct Console {
    terminal: bool,
    ansi: bool,
    unicode: bool,
}

impl Console {
    /// An interactive terminal, not a file or a pipe.
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// ANSI escape sequences (colors, cursor movement) are interpreted.
    pub fn ansi(&self) -> bool {
        self.ansi
    }

    /// Block, check and braille characters display (or, redirected, are decoded) correctly.
    pub fn unicode(&self) -> bool {
        self.unicode
    }

    /// Writes `text`; `\n` becomes `\r\n` on Windows. Errors (no console, closed pipe) are ignored.
    pub fn write(&self, text: &str) {
        let text = text.replace("\r\n", "\n");
        #[cfg(windows)]
        let text = text.replace('\n', "\r\n");
        let mut out = std::io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }
}

/// True when `NO_COLOR` is set to a non-empty value (<https://no-color.org>).
pub fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

#[cfg(windows)]
pub use win::{ctrl_c_pressed, detach, open, watch_ctrl_c, width};

#[cfg(not(windows))]
pub use unix::open;

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::io::IsTerminal;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::Console;

    type HANDLE = *mut c_void;
    type BOOL = i32;

    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const CP_UTF8: u32 = 65001;
    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;

    #[repr(C)]
    #[derive(Default)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    #[derive(Default)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ScreenBufferInfo {
        size: Coord,
        cursor: Coord,
        attributes: u16,
        window: SmallRect,
        max_window: Coord,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> BOOL;
        fn FreeConsole() -> BOOL;
        fn GetStdHandle(std_handle: u32) -> HANDLE;
        fn GetConsoleMode(console: HANDLE, mode: *mut u32) -> BOOL;
        fn SetConsoleMode(console: HANDLE, mode: u32) -> BOOL;
        fn GetConsoleOutputCP() -> u32;
        fn GetConsoleScreenBufferInfo(console: HANDLE, info: *mut ScreenBufferInfo) -> BOOL;
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> BOOL>, add: BOOL) -> BOOL;
    }

    static CTRL_C: AtomicBool = AtomicBool::new(false);

    /// Attaches to the parent's console (when there is one) and turns on escape sequences for it.
    pub fn open() -> Console {
        // SAFETY: plain Win32 calls on this process's own console and standard handle; `mode` outlives them.
        let (ansi, code_page) = unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            let out = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0;
            let ansi = !out.is_null()
                && GetConsoleMode(out, &mut mode) != 0
                && (mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
                    || SetConsoleMode(out, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0);
            (ansi, GetConsoleOutputCP())
        };
        let terminal = std::io::stdout().is_terminal();
        // Characters reach a console as UTF-16 whatever its code page, but the classic console font lacks
        // braille and check marks; Windows Terminal (WT_SESSION) has them. A file or pipe is decoded with the
        // console's code page by the shell reading it.
        let windows_terminal = std::env::var_os("WT_SESSION").is_some();
        let unicode = code_page == CP_UTF8 || (terminal && windows_terminal);
        Console { terminal, ansi: ansi && terminal, unicode }
    }

    /// Columns of the console window, when the output is one.
    pub fn width() -> Option<usize> {
        let mut info = ScreenBufferInfo::default();
        // SAFETY: `info` is a valid, writable CONSOLE_SCREEN_BUFFER_INFO for the call.
        let ok = unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } != 0;
        let columns = i32::from(info.window.right) - i32::from(info.window.left) + 1;
        (ok && columns > 0).then_some(columns as usize)
    }

    /// Lets go of the console, so Ctrl+C in that shell no longer reaches this process (the GUI app keeps
    /// running after `opendesk` printed its banner).
    pub fn detach() {
        // SAFETY: no arguments; detaching an unattached process just fails.
        unsafe { FreeConsole() };
    }

    unsafe extern "system" fn on_ctrl(event: u32) -> BOOL {
        if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
            CTRL_C.store(true, Ordering::SeqCst);
            return 1;
        }
        0
    }

    /// From now on Ctrl+C (and Ctrl+Break) set [`ctrl_c_pressed`] instead of ending the process.
    pub fn watch_ctrl_c() {
        // SAFETY: `on_ctrl` is a static function that only stores an atomic; it stays valid for the process.
        unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    }

    /// True once Ctrl+C was pressed after [`watch_ctrl_c`].
    pub fn ctrl_c_pressed() -> bool {
        CTRL_C.load(Ordering::SeqCst)
    }
}

#[cfg(not(windows))]
mod unix {
    use std::io::IsTerminal;

    use super::Console;

    /// Standard output as it is; a terminal other than `TERM=dumb` takes escape sequences.
    pub fn open() -> Console {
        let terminal = std::io::stdout().is_terminal();
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        // The first of LC_ALL, LC_CTYPE and LANG that is set decides the character set.
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_default()
            .to_ascii_lowercase();
        let unicode = locale.contains("utf-8") || locale.contains("utf8");
        Console { terminal, ansi: terminal && !dumb, unicode }
    }
}
