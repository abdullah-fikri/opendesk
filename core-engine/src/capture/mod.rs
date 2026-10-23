//! Screen capture. [`DesktopCapturer`] picks DXGI Desktop Duplication and falls back to GDI.

mod cursor;
pub mod dxgi;
mod gdi;
mod monitor;
mod win32;

pub use cursor::cursor_shape;
pub use gdi::GdiScreenCapturer;
pub use monitor::{MonitorBounds, enable_per_monitor_dpi_awareness, enumerate_monitors};

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

    /// Regions that moved (e.g. scrolling) in the last grab; empty for GDI.
    pub fn moves(&self) -> &[crate::codec::frame::MoveRect] {
        match self {
            DesktopCapturer::Dxgi(d) => d.moves(),
            DesktopCapturer::Gdi(_) => &[],
        }
    }
}
