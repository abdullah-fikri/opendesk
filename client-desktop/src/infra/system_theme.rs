//! The system's light or dark app setting. On Windows it is read from the registry: winit's own check calls an
//! undocumented uxtheme export that reports stale or wrong values on current Windows 11 builds, so "Match the
//! system" stayed light. Elsewhere `None` lets the UI keep Slint's own report.

/// `Some(true)` when apps should be dark, `None` when this platform has no direct reading.
#[cfg(windows)]
pub fn apps_dark() -> Option<bool> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;

    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the out pointers point at a live u32 and its size, as RRF_RT_REG_DWORD requires.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(value == 0)
}

#[cfg(not(windows))]
pub fn apps_dark() -> Option<bool> {
    None
}
