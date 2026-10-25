//! Monitor enumeration and DPI awareness.

use std::ptr;

use super::win32::*;
use crate::capture::MonitorBounds;

/// Makes the process per-monitor DPI aware (V2, else system aware) so sizes are physical pixels.
pub fn enable_per_monitor_dpi_awareness() {
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            SetProcessDPIAware();
        }
    }
}

/// Enumerates physical hardware bounds of all active monitors using `EnumDisplayDevicesW` + `EnumDisplaySettingsExW`.
/// Always returns true physical pixel resolutions (e.g. 1920x1080, 1920x1200) unaffected by Windows 125%/150% DPI scaling.
pub fn enumerate_monitors() -> Vec<MonitorBounds> {
    enable_per_monitor_dpi_awareness();
    unsafe {
        let mut raw: Vec<(MonitorBounds, bool)> = Vec::new();
        let mut dev_idx: DWORD = 0;

        loop {
            let mut dd: DISPLAY_DEVICEW = std::mem::zeroed();
            dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as DWORD;
            if EnumDisplayDevicesW(ptr::null(), dev_idx, &mut dd, 0) == 0 {
                break;
            }
            dev_idx += 1;

            if (dd.state_flags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) == 0 {
                continue;
            }

            let mut dm: DEVMODEW = std::mem::zeroed();
            dm.dm_size = std::mem::size_of::<DEVMODEW>() as WORD;
            if EnumDisplaySettingsExW(dd.device_name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm, 0) != 0 {
                let w = dm.dm_pels_width as i32;
                let h = dm.dm_pels_height as i32;
                if w > 0 && h > 0 {
                    let is_primary = (dd.state_flags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0;
                    raw.push((
                        MonitorBounds { left: dm.dm_position.x, top: dm.dm_position.y, width: w, height: h },
                        is_primary,
                    ));
                }
            }
        }

        // Primary monitor always at index 0
        raw.sort_by_key(|(_, is_prim)| !*is_prim);
        let mut monitors: Vec<MonitorBounds> = raw.into_iter().map(|(b, _)| b).collect();

        if monitors.is_empty() {
            let screen_dc = GetDC(ptr::null_mut());
            let phys_w = GetDeviceCaps(screen_dc, DESKTOPHORZRES);
            let phys_h = GetDeviceCaps(screen_dc, DESKTOPVERTRES);
            ReleaseDC(ptr::null_mut(), screen_dc);
            let w = if phys_w > 0 { phys_w } else { GetSystemMetrics(0) }.max(1);
            let h = if phys_h > 0 { phys_h } else { GetSystemMetrics(1) }.max(1);
            monitors.push(MonitorBounds { left: 0, top: 0, width: w, height: h });
        }
        monitors
    }
}
