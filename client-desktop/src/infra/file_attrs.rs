//! Sets the file attributes a transfer carries (read-only, hidden, archive) on a received file.

use std::path::Path;

/// Adds `attrs` (FILE_ATTRIBUTE_* bits) to the file's attributes; failures are cosmetic and ignored.
#[cfg(windows)]
pub fn set(path: &Path, attrs: u32) {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileAttributesW(name: *const u16) -> u32;
        fn SetFileAttributesW(name: *const u16, attrs: u32) -> i32;
    }
    const INVALID: u32 = u32::MAX;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated path that outlives both calls.
    unsafe {
        let now = GetFileAttributesW(wide.as_ptr());
        if now != INVALID {
            SetFileAttributesW(wide.as_ptr(), (now & !FILE_ATTRIBUTE_NORMAL) | attrs);
        }
    }
}

/// Only read-only has a meaning here.
#[cfg(not(windows))]
pub fn set(path: &Path, attrs: u32) {
    if attrs & 0x1 != 0
        && let Ok(meta) = std::fs::metadata(path)
    {
        let mut perms = meta.permissions();
        perms.set_readonly(true);
        let _ = std::fs::set_permissions(path, perms);
    }
}
