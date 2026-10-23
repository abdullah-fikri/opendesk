//! Stream resolution policy shared by viewer (what to ask for) and host (what to capture).
//!
//! Viewer modes: 0 native, 1 automatic (follow the viewer canvas), 480/720/1080 presets.
//! An automatic request is stored on the host as `AUTO_BOX_FLAG | width << 16 | height`, so one atomic
//! carries both kinds of request into the capture loop.

/// Native resolution.
pub const NATIVE: usize = 0;
/// Automatic: stream at the size of the viewer canvas.
pub const AUTO: usize = 1;
/// Default preset; also what hosts that predate automatic mode do with an automatic request.
pub const DEFAULT_PRESET: usize = 1080;

const AUTO_BOX_FLAG: usize = 1 << 40;
/// Largest canvas dimension accepted from the wire.
const MAX_DIMENSION: u64 = 16_384;

/// Maps a UI value to a known viewer mode; unknown values fall back to the default preset.
pub fn sanitize_mode(mode: i32) -> usize {
    match mode {
        0 => NATIVE,
        1 => AUTO,
        480 | 720 | 1080 => mode as usize,
        _ => DEFAULT_PRESET,
    }
}

/// Packs a canvas size (physical px) into one atomic value.
pub fn pack_box(w: i32, h: i32) -> u64 {
    ((w.max(0) as u64) << 32) | h.max(0) as u64
}

/// Inverse of `pack_box`.
pub fn unpack_box(v: u64) -> (i32, i32) {
    ((v >> 32) as i32, (v & 0xFFFF_FFFF) as i32)
}

/// Debounced resizes only reach the host when they change the box noticeably
/// (each change recreates the capturer/encoder and costs a key frame).
pub fn box_changed_enough(prev: (i32, i32), next: (i32, i32)) -> bool {
    let differs = |a: i32, b: i32| (a - b).abs() >= 32.max(a / 25); // 32 px or 4 %
    prev.0 <= 0 || prev.1 <= 0 || differs(prev.0, next.0) || differs(prev.1, next.1)
}

/// What a viewer asks the host for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// Preset sent on the wire. Automatic requests still carry 1080 so older hosts keep their default.
    pub mode: usize,
    /// Canvas size for automatic mode.
    pub auto_box: Option<(i32, i32)>,
}

/// The request a viewer sends for `mode` with the current canvas `viewport`.
pub fn request(mode: usize, viewport: (i32, i32)) -> Request {
    match mode {
        AUTO if viewport.0 > 0 && viewport.1 > 0 => Request { mode: DEFAULT_PRESET, auto_box: Some(viewport) },
        AUTO => Request { mode: DEFAULT_PRESET, auto_box: None },
        other => Request { mode: other, auto_box: None },
    }
}

/// Host side: the value to store for a received request.
pub fn host_mode(mode: Option<u64>, auto: bool, width: Option<u64>, height: Option<u64>) -> usize {
    let dim = |v: Option<u64>| v.unwrap_or(0).min(MAX_DIMENSION) as usize;
    let (w, h) = (dim(width), dim(height));
    if auto && w > 0 && h > 0 {
        return AUTO_BOX_FLAG | w.max(320) << 16 | h.max(180);
    }
    let mode = mode.unwrap_or(DEFAULT_PRESET as u64) as usize;
    // Only this side builds automatic values; a raw one from the wire falls back to the default.
    if mode >= AUTO_BOX_FLAG { DEFAULT_PRESET } else { mode }
}

/// True when `host_mode` holds an automatic request.
pub fn is_auto(host_mode: usize) -> bool {
    host_mode & AUTO_BOX_FLAG != 0
}

/// Returns `(max_width, jpeg_quality)` for a host mode on a monitor of `width` x `height`. Presets fit their
/// box (e.g. 1280x720) on any aspect ratio, so "720p" never exceeds 720 rows on 16:10 or portrait monitors.
pub fn capture_params(host_mode: usize, width: i32, height: i32) -> (i32, u32) {
    if is_auto(host_mode) {
        return auto_box_params(((host_mode >> 16) & 0xFFFF) as i32, (host_mode & 0xFFFF) as i32, width, height);
    }
    let (box_w, box_h, quality) = match host_mode {
        NATIVE => return (0, 85),
        720 => (1280, 720, 78),
        480 => (854, 480, 72),
        _ => (1920, 1080, 82),
    };
    let by_height = (width.max(1) as i64 * box_h / height.max(1) as i64) as i32;
    (box_w.min(by_height).max(2), quality)
}

/// Largest stream that fits the viewer box, keeping the monitor's aspect ratio and never upscaling.
fn auto_box_params(box_w: i32, box_h: i32, width: i32, height: i32) -> (i32, u32) {
    let (sw, sh) = (width.max(1) as i64, height.max(1) as i64);
    let by_height = sw * box_h as i64 / sh;
    let w = (box_w as i64).min(by_height).clamp(2, sw) as i32 & !1;
    let out_h = sh * w as i64 / sw;
    let quality = if out_h >= 900 {
        82
    } else if out_h >= 600 {
        78
    } else {
        72
    };
    // Native is reported as the monitor width, the same value the 1080 preset yields on a 1080p monitor,
    // so switching between them does not recreate the capturer.
    (if w as i64 >= sw - 1 { sw as i32 } else { w.max(2) }, quality)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(w: u64, h: u64) -> usize {
        host_mode(Some(1080), true, Some(w), Some(h))
    }

    #[test]
    fn presets_fit_their_box_on_any_aspect_ratio() {
        assert_eq!(capture_params(720, 1920, 1080).0, 1280);
        assert_eq!(capture_params(720, 1920, 1200).0, 1152);
        assert_eq!(capture_params(720, 1080, 1920).0, 405);
        assert_eq!(capture_params(480, 3440, 1440).0, 854);
        assert_eq!(capture_params(NATIVE, 3840, 2160).0, 0);
        assert_eq!(capture_params(999, 1920, 1080), (1920, 82));
        assert_eq!(capture_params(AUTO, 1920, 1080), (1920, 82));
    }

    #[test]
    fn auto_mode_fits_the_viewer_box() {
        assert_eq!(capture_params(auto(1000, 700), 1920, 1080), (1000, 72));
        assert_eq!(capture_params(auto(2500, 900), 1920, 1080), (1600, 82));
        assert_eq!(capture_params(auto(1279, 2000), 1920, 1080).0, 1278);
        assert_eq!(capture_params(auto(3840, 2160), 1920, 1080), capture_params(1080, 1920, 1080));
        assert_eq!(capture_params(auto(5000, 3000), 3840, 2160), (3840, 82));
        assert_eq!(capture_params(auto(2560, 1300), 3840, 2160), (2310, 82));
        assert_eq!(capture_params(auto(1600, 800), 1080, 1920), (450, 78));
        assert_eq!(capture_params(auto(10, 10), 1920, 1080), (320, 72));
    }

    #[test]
    fn requests_stay_compatible_with_older_hosts() {
        let r = request(AUTO, (1200, 700));
        assert_eq!(r, Request { mode: 1080, auto_box: Some((1200, 700)) });
        assert_eq!(host_mode(Some(r.mode as u64), true, Some(1200), Some(700)), auto(1200, 700));
        assert_eq!(request(AUTO, (0, 0)), Request { mode: 1080, auto_box: None });
        assert_eq!(request(720, (1200, 700)), Request { mode: 720, auto_box: None });
        assert_eq!(host_mode(Some(0), false, None, None), 0);
        assert_eq!(host_mode(None, false, None, None), 1080);
        assert_eq!(host_mode(Some((AUTO_BOX_FLAG | 5) as u64), false, None, None), 1080);
    }

    #[test]
    fn tiny_resizes_are_ignored_and_modes_sanitized() {
        assert!(box_changed_enough((0, 0), (1200, 700)));
        assert!(!box_changed_enough((1200, 700), (1210, 690)));
        assert!(box_changed_enough((1200, 700), (1260, 700)));
        assert!(box_changed_enough((1200, 700), (1200, 650)));
        assert_eq!(unpack_box(pack_box(2560, 1377)), (2560, 1377));
        assert_eq!((sanitize_mode(0), sanitize_mode(1), sanitize_mode(720), sanitize_mode(12345)), (0, 1, 720, 1080));
    }
}
