//! GDI BitBlt screen capture: the fallback when DXGI Desktop Duplication is unavailable.

use std::ffi::c_void;
use std::ptr;

use super::monitor::{MonitorBounds, enable_per_monitor_dpi_awareness};
use super::win32::*;

/// Screen capture with GDI `BitBlt`/`StretchBlt` into a DIB section. Slower than DXGI but works everywhere.
pub struct GdiScreenCapturer {
    /// Left edge of the captured monitor in virtual-desktop coordinates.
    pub offset_x: i32,
    /// Top edge of the captured monitor in virtual-desktop coordinates.
    pub offset_y: i32,
    /// Native monitor width.
    pub screen_width: i32,
    /// Native monitor height.
    pub screen_height: i32,
    /// Output width (scaled down when `max_width` is smaller than the monitor).
    pub width: i32,
    /// Output height.
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
    /// Captures `bounds`, scaled to at most `max_width` pixels wide (0 = native), keeping the aspect ratio.
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
            let mem_bmp = CreateDIBSection(screen_dc, &bmi, 0, &mut bits, ptr::null_mut(), 0);
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

impl GdiScreenCapturer {
    /// Copies the monitor into the DIB section and returns (bitmap, width, height).
    pub fn capture_frame(&mut self) -> (HBITMAP, i32, i32) {
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
                if rel_x >= 0 && rel_x <= self.screen_width && rel_y >= 0 && rel_y <= self.screen_height {
                    let cx = (rel_x as i64 * self.width as i64 / self.screen_width as i64) as i32;
                    let cy = (rel_y as i64 * self.height as i64 / self.screen_height as i64) as i32;
                    DrawIcon(self.mem_dc, cx, cy, ci.h_cursor);
                    self.cursor_drawn = true;
                }
            }

            (self.mem_bmp, self.width, self.height)
        }
    }
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
