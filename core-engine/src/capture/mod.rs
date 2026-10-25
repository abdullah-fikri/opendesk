//! Screen capture. [`DesktopCapturer`] is the platform backend chosen at compile time: DXGI Desktop
//! Duplication with GDI fallback on Windows, X11 (MIT-SHM) elsewhere.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod x11;
#[cfg(not(windows))]
pub use x11::*;

/// Full-frame capturer for small periodic snapshots (thumbnails): GDI on Windows, so it never takes one of the
/// few DXGI duplication slots a live session needs; the X11 capturer elsewhere. Both offer `new_for_monitor`,
/// `capture_frame`, `pixels` and the `width`/`height` fields.
#[cfg(windows)]
pub type ThumbnailCapturer = GdiScreenCapturer;
/// Full-frame capturer for small periodic snapshots (thumbnails): GDI on Windows, so it never takes one of the
/// few DXGI duplication slots a live session needs; the X11 capturer elsewhere. Both offer `new_for_monitor`,
/// `capture_frame`, `pixels` and the `width`/`height` fields.
#[cfg(not(windows))]
pub type ThumbnailCapturer = DesktopCapturer;

/// A monitor in physical pixels, positioned in virtual-desktop coordinates.
#[derive(Clone, Copy, Debug)]
pub struct MonitorBounds {
    /// Left edge in virtual-desktop coordinates.
    pub left: i32,
    /// Top edge in virtual-desktop coordinates.
    pub top: i32,
    /// Width in physical pixels.
    pub width: i32,
    /// Height in physical pixels.
    pub height: i32,
}

/// Whether this session can share its screen. Viewing works everywhere; hosting needs a capturable desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostSupport {
    /// Capture and input injection are available.
    Supported,
    /// Hosting is impossible in this session; the text says why and what to do instead.
    Unsupported(String),
}

/// Maps an X cursor name (XFixes, freedesktop cursor themes) to the portable shape codes of `cursor_shape`.
#[cfg(any(not(windows), test))]
pub fn shape_from_cursor_name(name: &str) -> u8 {
    match name {
        "xterm" | "text" | "ibeam" | "vertical-text" => 2,
        "hand" | "hand1" | "hand2" | "pointer" | "pointing_hand" | "grab" | "openhand" => 3,
        "watch" | "wait" => 4,
        "left_ptr_watch" | "progress" | "half-busy" => 5,
        "crosshair" | "cross" | "tcross" | "cell" | "plus" => 6,
        "fleur" | "move" | "all-scroll" | "size_all" | "grabbing" | "closedhand" | "dnd-move" => 7,
        "sb_h_double_arrow" | "h_double_arrow" | "ew-resize" | "col-resize" | "size_hor" | "left_side"
        | "right_side" | "e-resize" | "w-resize" | "split_h" => 8,
        "sb_v_double_arrow" | "v_double_arrow" | "ns-resize" | "row-resize" | "size_ver" | "top_side"
        | "bottom_side" | "n-resize" | "s-resize" | "split_v" => 9,
        "nwse-resize"
        | "size_fdiag"
        | "bd_double_arrow"
        | "top_left_corner"
        | "bottom_right_corner"
        | "nw-resize"
        | "se-resize" => 10,
        "nesw-resize" | "size_bdiag" | "fd_double_arrow" | "top_right_corner" | "bottom_left_corner" | "ne-resize"
        | "sw-resize" => 11,
        "not-allowed" | "crossed_circle" | "forbidden" | "no-drop" | "circle" | "dnd-no-drop" => 12,
        "help" | "question_arrow" | "whats_this" | "left_ptr_help" => 13,
        // Arrows and application-specific cursors.
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_names_map_to_shape_codes() {
        assert_eq!(shape_from_cursor_name("left_ptr"), 1);
        assert_eq!(shape_from_cursor_name("xterm"), 2);
        assert_eq!(shape_from_cursor_name("hand2"), 3);
        assert_eq!(shape_from_cursor_name("sb_h_double_arrow"), 8);
        assert_eq!(shape_from_cursor_name("not-allowed"), 12);
        assert_eq!(shape_from_cursor_name("some-app-cursor"), 1);
    }
}
