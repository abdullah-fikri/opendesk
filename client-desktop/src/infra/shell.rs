//! The desktop shell, for the file browser's local pane: open a file with its default app, show it in Explorer
//! (Finder, the file manager), and move items to the Recycle Bin. Windows calls the shell directly
//! (`ShellExecuteW`, `SHFileOperationW`); macOS and Linux start `open` / `xdg-open`.

use std::path::{Path, PathBuf};

/// Opens `path` the way a double click in Explorer would (its default app; a folder opens a window).
pub fn open(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    return win::open(path);
    #[cfg(target_os = "macos")]
    return spawn("open", &[path.as_os_str()]);
    #[cfg(all(unix, not(target_os = "macos")))]
    return spawn("xdg-open", &[path.as_os_str()]);
}

/// Shows `path` selected in its folder.
pub fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer parses its own command line: `/select,"C:\a b\c.txt"` must reach it unquoted as a whole.
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Cannot open Explorer: {e}"))
    }
    #[cfg(target_os = "macos")]
    return spawn("open", &["-R".as_ref(), path.as_os_str()]);
    #[cfg(all(unix, not(target_os = "macos")))]
    return spawn("xdg-open", &[path.parent().unwrap_or(path).as_os_str()]);
}

/// True when [`trash`] can move items to the Recycle Bin here (Windows, as the signed-in user itself: under
/// SYSTEM the bin would be the wrong user's).
pub fn can_trash() -> bool {
    #[cfg(windows)]
    return !crate::infra::system_service::is_local_system();
    #[cfg(not(windows))]
    false
}

/// Moves `paths` to the Recycle Bin. Err when this platform or account cannot (see [`can_trash`]).
pub fn trash(paths: &[PathBuf]) -> Result<(), String> {
    if !can_trash() {
        return Err("The Recycle Bin is not available here".into());
    }
    #[cfg(windows)]
    return win::trash(paths);
    #[cfg(not(windows))]
    {
        let _ = paths;
        Err("The Recycle Bin is not available here".into())
    }
}

#[cfg(unix)]
fn spawn(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let mut child =
        std::process::Command::new(program).args(args).spawn().map_err(|e| format!("Cannot run {program}: {e}"))?;
    // Reap it so it does not linger as a zombie.
    crate::infra::thread::spawn_named("shell-open", move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr;

    const SW_SHOWNORMAL: i32 = 1;
    const FO_DELETE: u32 = 3;
    const FOF_SILENT: u16 = 0x0004;
    const FOF_NOCONFIRMATION: u16 = 0x0010;
    const FOF_ALLOWUNDO: u16 = 0x0040;
    const FOF_NOERRORUI: u16 = 0x0400;
    /// Warn instead of silently deleting for good what does not fit in the bin.
    const FOF_WANTNUKEWARNING: u16 = 0x4000;

    /// SHFILEOPSTRUCTW; shellapi.h packs it to 1 byte only on 32-bit Windows.
    #[cfg_attr(target_pointer_width = "32", repr(C, packed(1)))]
    #[cfg_attr(target_pointer_width = "64", repr(C))]
    struct ShFileOp {
        hwnd: *mut c_void,
        func: u32,
        from: *const u16,
        to: *const u16,
        flags: u16,
        aborted: i32,
        mappings: *mut c_void,
        title: *const u16,
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
        fn SHFileOperationW(op: *mut ShFileOp) -> i32;
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    pub fn open(path: &Path) -> Result<(), String> {
        let (verb, file) = (wide(std::ffi::OsStr::new("open")), wide(path.as_os_str()));
        let dir = path.parent().map(|d| wide(d.as_os_str()));
        // SAFETY: every pointer is a NUL-terminated UTF-16 string that outlives the call.
        let code = unsafe {
            ShellExecuteW(
                ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                ptr::null(),
                dir.as_ref().map_or(ptr::null(), |d| d.as_ptr()),
                SW_SHOWNORMAL,
            )
        };
        // Values above 32 mean success; 31 = no app is associated with the file type.
        match code {
            c if c > 32 => Ok(()),
            31 => Err(format!("No app is set to open {}", path.display())),
            c => Err(format!("Cannot open {} (error {c})", path.display())),
        }
    }

    pub fn trash(paths: &[PathBuf]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        // A list of full paths, each NUL-terminated, ending with an extra NUL.
        let mut from: Vec<u16> = paths.iter().flat_map(|p| wide(p.as_os_str())).collect();
        from.push(0);
        let mut op = ShFileOp {
            hwnd: ptr::null_mut(),
            func: FO_DELETE,
            from: from.as_ptr(),
            to: ptr::null(),
            flags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING,
            aborted: 0,
            mappings: ptr::null_mut(),
            title: ptr::null(),
        };
        // SAFETY: `op` is a valid SHFILEOPSTRUCTW whose `from` list lives until the call returns.
        let code = unsafe { SHFileOperationW(&mut op) };
        let aborted = op.aborted;
        match (code, aborted) {
            (0, 0) => Ok(()),
            (0, _) => Err("Moving to the Recycle Bin was cancelled".into()),
            (c, _) => Err(format!("Cannot move to the Recycle Bin (error {c:#x})")),
        }
    }
}
