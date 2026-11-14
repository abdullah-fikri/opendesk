//! Screen capture. [`DesktopCapturer`] picks DXGI Desktop Duplication and falls back to GDI.

mod cursor;
pub mod dxgi;
mod gdi;
mod monitor;
mod win32;

pub use cursor::{cursor_shape, pointer_captured};
pub use gdi::GdiScreenCapturer;
pub use monitor::{enable_per_monitor_dpi_awareness, enumerate_monitors, refresh_rate};

use crate::capture::{HostSupport, MonitorBounds};

/// Windows can always share its screen (the input desktop is followed when running as SYSTEM).
pub fn host_support() -> HostSupport {
    HostSupport::Supported
}

/// Primary capturer: DXGI Desktop Duplication, with the GDI capturer as fallback (rotated outputs, no
/// D3D11 device, duplication denied, or `OPENDESK_CAPTURE=gdi`).
// One capturer exists per hosted session, so the size difference between variants costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum DesktopCapturer {
    /// Desktop Duplication.
    Dxgi(dxgi::DxgiCapturer),
    /// GDI fallback.
    Gdi(GdiScreenCapturer),
}

impl DesktopCapturer {
    /// Opens DXGI capture for `bounds` (GDI when it fails or `OPENDESK_CAPTURE=gdi`), scaled to `max_width` (0 = native).
    pub fn new_for_monitor(bounds: MonitorBounds, max_width: i32) -> Self {
        let force_gdi = std::env::var("OPENDESK_CAPTURE").is_ok_and(|v| v.eq_ignore_ascii_case("gdi"));
        if !force_gdi && let Ok(mut d) = dxgi::DxgiCapturer::new_for_monitor(bounds, max_width) {
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
        DesktopCapturer::Gdi(GdiScreenCapturer::new_for_monitor(bounds, max_width))
    }

    /// `"dxgi"` or `"gdi"`, for logs.
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

    /// Output width in pixels.
    pub fn width(&self) -> i32 {
        self.geometry().4
    }

    /// Output height in pixels.
    pub fn height(&self) -> i32 {
        self.geometry().5
    }

    /// Stops painting the cursor into frames while the viewer draws its own.
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
                Err(_) => {
                    Some((d.offset_x, d.offset_y, d.screen_width, d.screen_height, d.width, d.hide_viewer_cursor))
                }
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

    /// Part of the last `grab` spent waiting for a desktop change (DXGI); the rest is capture work. GDI never waits.
    pub fn last_wait(&self) -> std::time::Duration {
        match self {
            DesktopCapturer::Dxgi(d) => d.last_wait(),
            DesktopCapturer::Gdi(_) => std::time::Duration::ZERO,
        }
    }

    /// The last captured image as BGRX pixels, `width()` per row.
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

    /// Zero-copy source for the H.264 encoder: DXGI at native size only (the GPU scaler is softer than the CPU's
    /// area average). None for GDI, scaled capture, or a device that cannot keep GPU frames.
    pub fn gpu_source(&mut self) -> Option<crate::codec::h264::GpuSource> {
        match self {
            DesktopCapturer::Dxgi(d) if d.width == d.screen_width && d.height == d.screen_height => {
                let device = d.enable_gpu_frames().ok()?;
                Some(crate::codec::h264::GpuSource {
                    device,
                    src_w: d.screen_width as u32,
                    src_h: d.screen_height as u32,
                })
            }
            _ => None,
        }
    }

    /// The last image on the GPU (after [`Self::gpu_source`]).
    pub fn gpu_texture(&self) -> Option<crate::codec::h264::GpuTexture> {
        match self {
            DesktopCapturer::Dxgi(d) => d.gpu_texture().cloned(),
            DesktopCapturer::Gdi(_) => None,
        }
    }

    /// Off: the next grabs leave the image on the GPU (see `DxgiCapturer::set_cpu_frames`); GDI ignores it.
    pub fn set_cpu_frames(&mut self, on: bool) {
        if let DesktopCapturer::Dxgi(d) = self {
            d.set_cpu_frames(on);
        }
    }

    /// Brings `pixels` up to date after GPU-only grabs (one full readback). A failure leaves the last pixels.
    pub fn read_back(&mut self) {
        if let DesktopCapturer::Dxgi(d) = self {
            let _ = d.read_back();
        }
    }

    /// Regions that moved (e.g. scrolling) in the last grab; empty for GDI.
    pub fn moves(&self) -> &[crate::codec::frame::MoveRect] {
        match self {
            DesktopCapturer::Dxgi(d) => d.moves(),
            DesktopCapturer::Gdi(_) => &[],
        }
    }
}
