pub mod dxgi;

use std::ffi::c_void;
use std::ptr;

type HWND = *mut c_void;
type HDC = *mut c_void;
type HBITMAP = *mut c_void;
type HGDIOBJ = *mut c_void;
type HICON = *mut c_void;
type BOOL = i32;
type DWORD = u32;
type WORD = u16;
type UINT = u32;

const SRCCOPY: DWORD = 0x00CC0020;
const CURSOR_SHOWING: DWORD = 0x00000001;
const BI_RGB: DWORD = 0;
const COLORONCOLOR: i32 = 3;
const HALFTONE: i32 = 4;
const DESKTOPHORZRES: i32 = 118;
const DESKTOPVERTRES: i32 = 117;

const ENUM_CURRENT_SETTINGS: DWORD = 0xFFFFFFFF;
const DISPLAY_DEVICE_ATTACHED_TO_DESKTOP: DWORD = 0x00000001;
const DISPLAY_DEVICE_PRIMARY_DEVICE: DWORD = 0x00000004;
const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: isize = -4;

#[derive(Clone, Copy, Debug)]
pub struct MonitorBounds {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
struct CURSORINFO {
    cb_size: DWORD,
    flags: DWORD,
    h_cursor: HICON,
    pt_screen_pos: POINT,
}

#[repr(C)]
struct DISPLAY_DEVICEW {
    cb: DWORD,
    device_name: [u16; 32],
    device_string: [u16; 128],
    state_flags: DWORD,
    device_id: [u16; 128],
    device_key: [u16; 128],
}

#[repr(C)]
struct DEVMODEW {
    dm_device_name: [u16; 32],
    dm_spec_version: WORD,
    dm_driver_version: WORD,
    dm_size: WORD,
    dm_driver_extra: WORD,
    dm_fields: DWORD,
    dm_position: POINT,
    dm_display_orientation: DWORD,
    dm_display_fixed_output: DWORD,
    dm_color: i16,
    dm_duplex: i16,
    dm_y_resolution: i16,
    dm_tt_option: i16,
    dm_collate: i16,
    dm_form_name: [u16; 32],
    dm_log_pixels: WORD,
    dm_bits_per_pel: DWORD,
    dm_pels_width: DWORD,
    dm_pels_height: DWORD,
    dm_display_flags: DWORD,
    dm_display_frequency: DWORD,
    dm_icm_method: DWORD,
    dm_icm_intent: DWORD,
    dm_media_type: DWORD,
    dm_dither_type: DWORD,
    dm_reserved1: DWORD,
    dm_reserved2: DWORD,
    dm_panning_width: DWORD,
    dm_panning_height: DWORD,
}

#[repr(C)]
struct BITMAPINFOHEADER {
    bi_size: DWORD,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: DWORD,
    bi_size_image: DWORD,
    bi_x_pels_per_meter: i32,
    bi_y_pels_per_meter: i32,
    bi_clr_used: DWORD,
    bi_clr_important: DWORD,
}

#[repr(C)]
struct BITMAPINFO {
    bmi_header: BITMAPINFOHEADER,
    bmi_colors: [u32; 1],
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SetProcessDpiAwarenessContext(value: isize) -> BOOL;
    fn SetProcessDPIAware() -> BOOL;
    fn GetSystemMetrics(n_index: i32) -> i32;
    fn GetDC(h_wnd: HWND) -> HDC;
    fn ReleaseDC(h_wnd: HWND, h_dc: HDC) -> i32;
    fn GetCursorInfo(pci: *mut CURSORINFO) -> BOOL;
    fn DrawIcon(h_dc: HDC, x: i32, y: i32, h_icon: HICON) -> BOOL;
    fn LoadCursorW(h_instance: *mut c_void, lp_cursor_name: *const u16) -> HICON;
    fn EnumDisplayDevicesW(
        lp_device: *const u16,
        i_dev_num: DWORD,
        lp_display_device: *mut DISPLAY_DEVICEW,
        dw_flags: DWORD,
    ) -> BOOL;
    fn EnumDisplaySettingsExW(
        lpsz_device_name: *const u16,
        i_mode_num: DWORD,
        lp_dev_mode: *mut DEVMODEW,
        dw_flags: DWORD,
    ) -> BOOL;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(h_dc: HDC) -> HDC;
    fn DeleteDC(h_dc: HDC) -> BOOL;
    fn GetDeviceCaps(h_dc: HDC, index: i32) -> i32;
    fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BITMAPINFO,
        usage: UINT,
        ppv_bits: *mut *mut c_void,
        h_section: *mut c_void,
        offset: DWORD,
    ) -> HBITMAP;
    fn SelectObject(h_dc: HDC, h: HGDIOBJ) -> HGDIOBJ;
    fn DeleteObject(ho: HGDIOBJ) -> BOOL;
    fn BitBlt(
        h_dc: HDC,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        h_dc_src: HDC,
        x1: i32,
        y1: i32,
        rop: DWORD,
    ) -> BOOL;
    fn StretchBlt(
        h_dc_dest: HDC,
        x_dest: i32,
        y_dest: i32,
        w_dest: i32,
        h_dest: i32,
        h_dc_src: HDC,
        x_src: i32,
        y_src: i32,
        w_src: i32,
        h_src: i32,
        rop: DWORD,
    ) -> BOOL;
    fn SetStretchBltMode(h_dc: HDC, mode: i32) -> i32;
    fn SetBrushOrgEx(h_dc: HDC, x: i32, y: i32, lppt: *mut POINT) -> BOOL;
}

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
                        MonitorBounds {
                            left: dm.dm_position.x,
                            top: dm.dm_position.y,
                            width: w,
                            height: h,
                        },
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
            monitors.push(MonitorBounds {
                left: 0,
                top: 0,
                width: w,
                height: h,
            });
        }
        monitors
    }
}

pub trait ScreenCapturer {
    fn capture_frame(&mut self) -> (HBITMAP, i32, i32);
    fn sample_grid(&self, width: i32, height: i32) -> Vec<u32>;
}

pub struct GdiScreenCapturer {
    pub offset_x: i32,
    pub offset_y: i32,
    pub screen_width: i32,
    pub screen_height: i32,
    pub width: i32,
    pub height: i32,
    screen_dc: HDC,
    mem_dc: HDC,
    mem_bmp: HBITMAP,
    old_bmp: HGDIOBJ,
    bits_ptr: *const u32,
    /// Viewer renders its own cursor (shape synced via `cursor_shape`), so skip painting it while the viewer drives it.
    pub hide_viewer_cursor: bool,
    cursor_drawn: bool,
}

unsafe impl Send for GdiScreenCapturer {}

impl GdiScreenCapturer {
    pub fn new() -> Self {
        let monitors = enumerate_monitors();
        Self::new_for_monitor(monitors[0], 1920)
    }

    pub fn new_for_monitor(bounds: MonitorBounds, max_width: i32) -> Self {
        enable_per_monitor_dpi_awareness();
        unsafe {
            let screen_dc = GetDC(ptr::null_mut());
            let screen_width = bounds.width.max(1);
            let screen_height = bounds.height.max(1);

            let (width, height) = if max_width > 0 && screen_width > max_width {
                let w = max_width & !1;
                let h = ((screen_height as i64 * w as i64) / screen_width as i64) as i32 & !1;
                (w.max(2), h.max(2))
            } else {
                (screen_width, screen_height)
            };

            let mem_dc = CreateCompatibleDC(screen_dc);
            if width != screen_width || height != screen_height {
                // Use fast COLORONCOLOR decimation for 720p/480p low-latency modes; HALFTONE for >720p text clarity
                let stretch_mode = if width <= 1280 { COLORONCOLOR } else { HALFTONE };
                SetStretchBltMode(mem_dc, stretch_mode);
                if stretch_mode == HALFTONE {
                    SetBrushOrgEx(mem_dc, 0, 0, ptr::null_mut());
                }
            }

            let bmi = BITMAPINFO {
                bmi_header: BITMAPINFOHEADER {
                    bi_size: std::mem::size_of::<BITMAPINFOHEADER>() as DWORD,
                    bi_width: width,
                    bi_height: -height, // Top-down DIB
                    bi_planes: 1,
                    bi_bit_count: 32,
                    bi_compression: BI_RGB,
                    bi_size_image: 0,
                    bi_x_pels_per_meter: 0,
                    bi_y_pels_per_meter: 0,
                    bi_clr_used: 0,
                    bi_clr_important: 0,
                },
                bmi_colors: [0],
            };

            let mut bits: *mut c_void = ptr::null_mut();
            let mem_bmp = CreateDIBSection(
                screen_dc,
                &bmi,
                0,
                &mut bits,
                ptr::null_mut(),
                0,
            );
            let old_bmp = SelectObject(mem_dc, mem_bmp);

            Self {
                offset_x: bounds.left,
                offset_y: bounds.top,
                screen_width,
                screen_height,
                width,
                height,
                screen_dc,
                mem_dc,
                mem_bmp,
                old_bmp,
                bits_ptr: bits as *const u32,
                hide_viewer_cursor: false,
                cursor_drawn: false,
            }
        }
    }
}

impl GdiScreenCapturer {
    /// Last captured frame as top-down BGRX pixels (stride == width).
    pub fn pixels(&self) -> &[u32] {
        if self.bits_ptr.is_null() {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.bits_ptr, (self.width as usize) * (self.height as usize)) }
    }
}

impl ScreenCapturer for GdiScreenCapturer {
    fn capture_frame(&mut self) -> (HBITMAP, i32, i32) {
        unsafe {
            // Pure SRCCOPY reads directly from DWM desktop surface in ~1.2ms without stalling GPU pipeline
            if self.width == self.screen_width && self.height == self.screen_height {
                BitBlt(
                    self.mem_dc,
                    0,
                    0,
                    self.width,
                    self.height,
                    self.screen_dc,
                    self.offset_x,
                    self.offset_y,
                    SRCCOPY,
                );
            } else {
                StretchBlt(
                    self.mem_dc,
                    0,
                    0,
                    self.width,
                    self.height,
                    self.screen_dc,
                    self.offset_x,
                    self.offset_y,
                    self.screen_width,
                    self.screen_height,
                    SRCCOPY,
                );
            }

            // Draw hardware cursor translated into the active monitor's coordinate space
            let mut ci = CURSORINFO {
                cb_size: std::mem::size_of::<CURSORINFO>() as DWORD,
                flags: 0,
                h_cursor: ptr::null_mut(),
                pt_screen_pos: POINT { x: 0, y: 0 },
            };
            self.cursor_drawn = false;
            if GetCursorInfo(&mut ci) != 0
                && (ci.flags & CURSOR_SHOWING) != 0
                && !(self.hide_viewer_cursor
                    && crate::input::LAST_INJECTED_POS.load(std::sync::atomic::Ordering::Relaxed)
                        == crate::input::pack_pos(ci.pt_screen_pos.x, ci.pt_screen_pos.y))
            {
                let rel_x = ci.pt_screen_pos.x - self.offset_x;
                let rel_y = ci.pt_screen_pos.y - self.offset_y;
                if rel_x >= 0
                    && rel_x <= self.screen_width
                    && rel_y >= 0
                    && rel_y <= self.screen_height
                {
                    let cx = (rel_x as i64 * self.width as i64 / self.screen_width as i64) as i32;
                    let cy = (rel_y as i64 * self.height as i64 / self.screen_height as i64) as i32;
                    DrawIcon(self.mem_dc, cx, cy, ci.h_cursor);
                    self.cursor_drawn = true;
                }
            }

            (self.mem_bmp, self.width, self.height)
        }
    }

    fn sample_grid(&self, width: i32, height: i32) -> Vec<u32> {
        let grid_w = ((width + 63) / 64) as usize;
        let grid_h = ((height + 63) / 64) as usize;
        let mut samples = Vec::with_capacity(grid_w * grid_h + 1);

        if self.bits_ptr.is_null() {
            return samples;
        }

        let stride = width as usize;
        let total_pixels = (width as usize) * (height as usize);
        unsafe {
            let slice = std::slice::from_raw_parts(self.bits_ptr, total_pixels);
            for gy in 0..grid_h {
                let base_y = gy * 64;
                let max_y = (base_y + 64).min(height as usize);
                for gx in 0..grid_w {
                    let base_x = gx * 64;
                    let max_x = (base_x + 64).min(width as usize);
                    let mut hash: u32 = 2166136261;
                    let mut y = base_y;
                    while y < max_y {
                        let row_off = y * stride;
                        let mut x = base_x;
                        while x < max_x {
                            hash = (hash ^ slice[row_off + x]).wrapping_mul(16777619);
                            x += 8;
                        }
                        y += 4;
                    }
                    samples.push(hash);
                }
            }

            // Include cursor position in sample so cursor movement triggers instant frame update
            let mut ci = CURSORINFO {
                cb_size: std::mem::size_of::<CURSORINFO>() as DWORD,
                flags: 0,
                h_cursor: ptr::null_mut(),
                pt_screen_pos: POINT { x: 0, y: 0 },
            };
            if self.cursor_drawn && GetCursorInfo(&mut ci) != 0 {
                let pos_hash = ((ci.pt_screen_pos.x as u32) << 16) ^ (ci.pt_screen_pos.y as u32);
                samples.push(pos_hash);
            }
        }
        samples
    }
}

/// Current host cursor as a portable shape code: 0 hidden, 1 arrow, 2 text, 3 hand, 4 wait, 5 progress,
/// 6 crosshair, 7 move, 8 ew-resize, 9 ns-resize, 10 nwse-resize, 11 nesw-resize, 12 not-allowed, 13 help.
/// App-specific cursors fall back to arrow.
pub fn cursor_shape() -> u8 {
    // IDC_* resource IDs in the same order as shape codes 1..=13.
    const IDS: [u16; 13] = [32512, 32513, 32649, 32514, 32650, 32515, 32646, 32644, 32645, 32642, 32643, 32648, 32651];
    static HANDLES: std::sync::OnceLock<[usize; 13]> = std::sync::OnceLock::new();
    let handles = HANDLES.get_or_init(|| IDS.map(|id| unsafe { LoadCursorW(ptr::null_mut(), id as usize as *const u16) } as usize));
    let mut ci = CURSORINFO {
        cb_size: std::mem::size_of::<CURSORINFO>() as DWORD,
        flags: 0,
        h_cursor: ptr::null_mut(),
        pt_screen_pos: POINT { x: 0, y: 0 },
    };
    if unsafe { GetCursorInfo(&mut ci) } == 0 || (ci.flags & CURSOR_SHOWING) == 0 {
        return 0;
    }
    handles
        .iter()
        .position(|&h| h == ci.h_cursor as usize)
        .map_or(1, |i| i as u8 + 1)
}

impl Drop for GdiScreenCapturer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old_bmp);
            DeleteObject(self.mem_bmp);
            DeleteDC(self.mem_dc);
            ReleaseDC(ptr::null_mut(), self.screen_dc);
        }
    }
}

/// Primary capturer: DXGI Desktop Duplication, with the GDI capturer as fallback (rotated outputs, no
/// D3D11 device, duplication denied, or `OPENDESK_CAPTURE=gdi`).
pub enum DesktopCapturer {
    Dxgi(dxgi::DxgiCapturer),
    Gdi(GdiScreenCapturer),
}

impl DesktopCapturer {
    pub fn new_for_monitor(bounds: MonitorBounds, max_width: i32) -> Self {
        let force_gdi = std::env::var("OPENDESK_CAPTURE").is_ok_and(|v| v.eq_ignore_ascii_case("gdi"));
        if !force_gdi {
            if let Ok(mut d) = dxgi::DxgiCapturer::new_for_monitor(bounds, max_width) {
                // The first acquired frames can be pointer-only; wait briefly for a real image so a session
                // never starts with a black key frame. A fully static desktop may need a moment to present.
                let start = std::time::Instant::now();
                while !d.has_image() && start.elapsed() < std::time::Duration::from_millis(300) {
                    if d.grab(50).is_err() {
                        break;
                    }
                }
                return DesktopCapturer::Dxgi(d);
            }
        }
        DesktopCapturer::Gdi(GdiScreenCapturer::new_for_monitor(bounds, max_width))
    }

    pub fn backend(&self) -> &'static str {
        match self {
            DesktopCapturer::Dxgi(_) => "dxgi",
            DesktopCapturer::Gdi(_) => "gdi",
        }
    }

    /// (offset_x, offset_y, screen_width, screen_height, width, height)
    pub fn geometry(&self) -> (i32, i32, i32, i32, i32, i32) {
        match self {
            DesktopCapturer::Dxgi(d) => (d.offset_x, d.offset_y, d.screen_width, d.screen_height, d.width, d.height),
            DesktopCapturer::Gdi(g) => (g.offset_x, g.offset_y, g.screen_width, g.screen_height, g.width, g.height),
        }
    }

    pub fn width(&self) -> i32 {
        self.geometry().4
    }

    pub fn height(&self) -> i32 {
        self.geometry().5
    }

    pub fn set_hide_viewer_cursor(&mut self, hide: bool) {
        match self {
            DesktopCapturer::Dxgi(d) => d.hide_viewer_cursor = hide,
            DesktopCapturer::Gdi(g) => g.hide_viewer_cursor = hide,
        }
    }

    /// Captures the next frame. DXGI waits up to `timeout_ms` for a change; GDI returns at once.
    /// A dead duplication switches this capturer to GDI permanently (same geometry).
    pub fn grab(&mut self, timeout_ms: u32) {
        let fallback = match self {
            DesktopCapturer::Dxgi(d) => match d.grab(timeout_ms) {
                Ok(_) => None,
                Err(_) => Some((d.offset_x, d.offset_y, d.screen_width, d.screen_height, d.width, d.hide_viewer_cursor)),
            },
            DesktopCapturer::Gdi(g) => {
                g.capture_frame();
                None
            }
        };
        if let Some((left, top, width, height, out_w, hide)) = fallback {
            let max_w = if out_w < width { out_w } else { 0 };
            let mut g = GdiScreenCapturer::new_for_monitor(MonitorBounds { left, top, width, height }, max_w);
            g.hide_viewer_cursor = hide;
            g.capture_frame();
            *self = DesktopCapturer::Gdi(g);
        }
    }

    pub fn pixels(&self) -> &[u32] {
        match self {
            DesktopCapturer::Dxgi(d) => d.pixels(),
            DesktopCapturer::Gdi(g) => g.pixels(),
        }
    }

    /// Changed regions of the last grab; None = unknown (GDI), compare the whole frame.
    pub fn dirty_hint(&self) -> Option<&[crate::diff::Rect]> {
        match self {
            DesktopCapturer::Dxgi(d) => d.dirty_hint(),
            DesktopCapturer::Gdi(_) => None,
        }
    }

    pub fn moves(&self) -> &[crate::codec::frame::MoveRect] {
        match self {
            DesktopCapturer::Dxgi(d) => d.moves(),
            DesktopCapturer::Gdi(_) => &[],
        }
    }

    /// True when `grab` already waited for changes, so the caller should not sleep a full tick on top.
    pub fn waits_for_changes(&self) -> bool {
        matches!(self, DesktopCapturer::Dxgi(_))
    }
}
