//! X11 screen capture: MIT-SHM `GetImage` of the root window (plain `GetImage` when the display is remote),
//! RandR monitors, XFixes cursor shape and overlay. Wayland sessions cannot be captured this way; see
//! [`host_support`].

use std::cell::RefCell;
use std::error::Error;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder, Window};
use x11rb::rust_connection::RustConnection;

use super::{HostSupport, MonitorBounds, shape_from_cursor_name};
use crate::codec::frame::MoveRect;
use crate::diff::Rect;

type Res<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Size reported when no X display can be reached (tests, headless CI): callers always get one monitor.
const FALLBACK_MONITOR: MonitorBounds = MonitorBounds { left: 0, top: 0, width: 1920, height: 1080 };

/// Opens a connection and enables the extensions this module uses. Returns the connection and root window.
fn connect() -> Res<(RustConnection, Window)> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    // Extensions must be version-negotiated once per connection before their requests are valid.
    conn.xfixes_query_version(5, 0)?.reply()?;
    Ok((conn, root))
}

/// Whether this session can share its screen. Wayland sessions cannot be captured through X11 (XWayland only
/// sees X clients), so they are reported with instructions instead of streaming a black screen.
pub fn host_support() -> HostSupport {
    static SUPPORT: OnceLock<HostSupport> = OnceLock::new();
    SUPPORT
        .get_or_init(|| {
            let env_is = |key: &str, value: &str| std::env::var(key).is_ok_and(|v| v.eq_ignore_ascii_case(value));
            let wayland = env_is("XDG_SESSION_TYPE", "wayland")
                || (std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("DISPLAY").is_none());
            if wayland {
                return HostSupport::Unsupported(
                    "Wayland host capture is not supported yet. To let others control this computer, sign out and \
                     choose an X11 session (\"Ubuntu on Xorg\", \"GNOME on Xorg\" or \"Plasma (X11)\"). Viewing \
                     other computers works in this session."
                        .into(),
                );
            }
            match connect() {
                Ok((conn, _)) if pixel_layout_ok(&conn) => HostSupport::Supported,
                Ok(_) => HostSupport::Unsupported(
                    "This X display does not use 24/32-bit little-endian pixels, which screen sharing needs.".into(),
                ),
                Err(e) => HostSupport::Unsupported(format!(
                    "No X11 display could be opened ({e}). Screen sharing needs a graphical X11 session."
                )),
            }
        })
        .clone()
}

/// The capture code reads ZPixmap images as BGRX `u32`s: 32 bits per pixel, least significant byte first.
fn pixel_layout_ok(conn: &RustConnection) -> bool {
    let setup = conn.setup();
    let depth = setup.roots.first().map_or(0, |s| s.root_depth);
    setup.image_byte_order == ImageOrder::LSB_FIRST
        && cfg!(target_endian = "little")
        && setup.pixmap_formats.iter().any(|f| f.depth == depth && f.bits_per_pixel == 32)
}

thread_local! {
    /// Connection for the free functions below (monitor list, cursor shape), opened on first use per thread.
    static SHARED: RefCell<Option<(RustConnection, Window)>> = const { RefCell::new(None) };
}

/// Runs `f` on this thread's shared connection; a failed request drops the connection so the next call reconnects.
fn with_shared<T>(f: impl FnOnce(&RustConnection, Window) -> Res<T>) -> Option<T> {
    SHARED.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = connect().ok();
        }
        let (conn, root) = slot.as_ref()?;
        match f(conn, *root) {
            Ok(v) => Some(v),
            Err(_) => {
                *slot = None;
                None
            }
        }
    })
}

/// X11 has no per-monitor DPI virtualization: sizes are always physical pixels.
pub fn enable_per_monitor_dpi_awareness() {}

fn query_monitors(conn: &RustConnection, root: Window) -> Res<Vec<MonitorBounds>> {
    conn.randr_query_version(1, 5)?.reply()?;
    let reply = conn.randr_get_monitors(root, true)?.reply()?;
    let mut list: Vec<(MonitorBounds, bool)> = reply
        .monitors
        .iter()
        .filter(|m| m.width > 0 && m.height > 0)
        .map(|m| {
            let b = MonitorBounds { left: m.x as i32, top: m.y as i32, width: m.width as i32, height: m.height as i32 };
            (b, m.primary)
        })
        .collect();
    // Primary first, like the Windows enumeration; the rest left to right.
    list.sort_by_key(|(b, primary)| (!*primary, b.left, b.top));
    if list.is_empty() {
        let s = &conn.setup().roots[0];
        list.push((
            MonitorBounds { left: 0, top: 0, width: s.width_in_pixels as i32, height: s.height_in_pixels as i32 },
            true,
        ));
    }
    Ok(list.into_iter().map(|(b, _)| b).collect())
}

/// Active monitors (RandR 1.5), primary first. Without a display, one 1920x1080 placeholder.
pub fn enumerate_monitors() -> Vec<MonitorBounds> {
    with_shared(query_monitors).filter(|l| !l.is_empty()).unwrap_or_else(|| vec![FALLBACK_MONITOR])
}

/// Current host cursor as a portable shape code (see the Windows backend for the list): the XFixes cursor
/// name mapped through [`shape_from_cursor_name`], 0 when the cursor image is fully transparent.
pub fn cursor_shape() -> u8 {
    with_shared(|conn, _| {
        let reply = conn.xfixes_get_cursor_image_and_name()?.reply()?;
        if reply.cursor_image.iter().all(|&argb| argb >> 24 == 0) {
            return Ok(0);
        }
        Ok(shape_from_cursor_name(&String::from_utf8_lossy(&reply.name)))
    })
    .unwrap_or(1)
}

/// A SysV shared-memory segment attached to the X server (MIT-SHM).
struct ShmSegment {
    seg: u32,
    addr: *mut u8,
    len: usize,
}

impl ShmSegment {
    fn new(conn: &RustConnection, len: usize) -> Option<Self> {
        conn.shm_query_version().ok()?.reply().ok()?;
        // SAFETY: plain SysV calls; the segment is removed once both sides detach (IPC_RMID below).
        unsafe {
            let id = libc::shmget(libc::IPC_PRIVATE, len, libc::IPC_CREAT | 0o600);
            if id < 0 {
                return None;
            }
            let addr = libc::shmat(id, std::ptr::null(), 0);
            if addr as isize == -1 {
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
                return None;
            }
            let seg = conn.generate_id().ok();
            // A remote display cannot attach local shared memory: fall back to plain GetImage.
            let attached = seg.is_some_and(|s| conn.shm_attach(s, id as u32, false).is_ok_and(|c| c.check().is_ok()));
            libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
            match seg {
                Some(seg) if attached => Some(Self { seg, addr: addr as *mut u8, len }),
                _ => {
                    libc::shmdt(addr);
                    None
                }
            }
        }
    }

    fn release(&mut self, conn: &RustConnection) {
        let _ = conn.shm_detach(self.seg);
        let _ = conn.flush();
        // SAFETY: `addr` came from shmat and is detached exactly once.
        unsafe { libc::shmdt(self.addr as *const libc::c_void) };
    }
}

/// Last cursor image read through XFixes (premultiplied ARGB), re-read only when its serial changes.
#[derive(Default)]
struct CursorImage {
    serial: u32,
    width: usize,
    height: usize,
    xhot: i32,
    yhot: i32,
    argb: Vec<u32>,
}

/// Captures one monitor of the X screen into top-down BGRX pixels, scaled to at most `max_width`.
pub struct DesktopCapturer {
    conn: Option<RustConnection>,
    root: Window,
    shm: Option<ShmSegment>,
    /// Left edge of the captured monitor in root-window coordinates.
    pub offset_x: i32,
    /// Top edge of the captured monitor in root-window coordinates.
    pub offset_y: i32,
    /// Native monitor width.
    pub screen_width: i32,
    /// Native monitor height.
    pub screen_height: i32,
    /// Output width (scaled down when `max_width` is smaller than the monitor).
    pub width: i32,
    /// Output height.
    pub height: i32,
    /// Output frame, `width` x `height`.
    frame: Vec<u32>,
    /// Source column for every output column when scaling.
    x_map: Vec<usize>,
    cursor: CursorImage,
    /// Viewer renders its own cursor, so skip painting it while the viewer drives it.
    hide_viewer_cursor: bool,
}

// SAFETY: the shared-memory pointer is owned by this capturer and only touched through `&mut self`.
unsafe impl Send for DesktopCapturer {}

impl DesktopCapturer {
    /// Captures `bounds`, scaled to at most `max_width` pixels wide (0 = native), keeping the aspect ratio.
    /// Without a usable X display the capturer still exists and produces black frames.
    pub fn new_for_monitor(bounds: MonitorBounds, max_width: i32) -> Self {
        let screen_width = bounds.width.max(2);
        let screen_height = bounds.height.max(2);
        let (width, height) = if max_width > 0 && screen_width > max_width {
            let w = max_width & !1;
            let h = ((screen_height as i64 * w as i64) / screen_width as i64) as i32 & !1;
            (w.max(2), h.max(2))
        } else {
            (screen_width, screen_height)
        };
        let (conn, root) = match connect() {
            Ok((c, r)) if pixel_layout_ok(&c) => (Some(c), r),
            _ => (None, 0),
        };
        let shm = conn.as_ref().and_then(|c| ShmSegment::new(c, screen_width as usize * screen_height as usize * 4));
        let x_map = (0..width as usize).map(|x| x * screen_width as usize / width as usize).collect();
        Self {
            conn,
            root,
            shm,
            offset_x: bounds.left,
            offset_y: bounds.top,
            screen_width,
            screen_height,
            width,
            height,
            frame: vec![0; width as usize * height as usize],
            x_map,
            cursor: CursorImage::default(),
            hide_viewer_cursor: false,
        }
    }

    /// `"x11-shm"`, `"x11"` (plain GetImage) or `"none"` (no display), for logs.
    pub fn backend(&self) -> &'static str {
        match (&self.conn, &self.shm) {
            (None, _) => "none",
            (Some(_), Some(_)) => "x11-shm",
            (Some(_), None) => "x11",
        }
    }

    /// (offset_x, offset_y, screen_width, screen_height, width, height)
    pub fn geometry(&self) -> (i32, i32, i32, i32, i32, i32) {
        (self.offset_x, self.offset_y, self.screen_width, self.screen_height, self.width, self.height)
    }

    /// Output width in pixels.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// Output height in pixels.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Stops painting the cursor into frames while the viewer draws its own.
    pub fn set_hide_viewer_cursor(&mut self, hide: bool) {
        self.hide_viewer_cursor = hide;
    }

    /// Captures the next frame immediately (X11 cannot wait for changes; the caller paces the loop).
    /// A failed capture keeps the previous frame; a lost connection leaves the last image frozen.
    pub fn grab(&mut self, _timeout_ms: u32) {
        if self.capture().is_err() && self.shm.is_some() {
            // A failed shared-memory read (e.g. the segment was rejected) retries over the socket next time.
            if let (Some(mut shm), Some(conn)) = (self.shm.take(), self.conn.as_ref()) {
                shm.release(conn);
            }
        }
    }

    /// Captures a frame now (the GDI capturer's name for it; see `ThumbnailCapturer`).
    pub fn capture_frame(&mut self) {
        self.grab(0);
    }

    /// The last captured image as BGRX pixels, `width()` per row.
    pub fn pixels(&self) -> &[u32] {
        &self.frame
    }

    /// X11 reports no damage here: None = compare the whole frame.
    pub fn dirty_hint(&self) -> Option<&[Rect]> {
        None
    }

    /// X11 reports no framebuffer moves.
    pub fn moves(&self) -> &[MoveRect] {
        &[]
    }

    fn capture(&mut self) -> Res<()> {
        let Some(conn) = self.conn.as_ref() else { return Ok(()) };
        let (sw, sh) = (self.screen_width as usize, self.screen_height as usize);
        let (x, y, w, h) = (self.offset_x as i16, self.offset_y as i16, sw as u16, sh as u16);
        let owned;
        let native: &[u32] = if let Some(shm) = &self.shm {
            conn.shm_get_image(self.root, x, y, w, h, !0, ImageFormat::Z_PIXMAP.into(), shm.seg, 0)?.reply()?;
            // SAFETY: the reply arrived, so the server finished writing the segment; it holds sw*sh pixels.
            unsafe { std::slice::from_raw_parts(shm.addr as *const u32, (shm.len / 4).min(sw * sh)) }
        } else {
            let reply = conn.get_image(ImageFormat::Z_PIXMAP, self.root, x, y, w, h, !0)?.reply()?;
            owned =
                reply.data.chunks_exact(4).map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]])).collect::<Vec<_>>();
            &owned
        };
        if native.len() < sw * sh {
            return Err("short image".into());
        }
        let (dw, dh) = (self.width as usize, self.height as usize);
        if dw == sw && dh == sh {
            // The padding byte of 24-bit visuals is undefined; clear it so frame diffs stay exact.
            for (d, s) in self.frame.iter_mut().zip(native) {
                *d = s & 0x00FF_FFFF;
            }
        } else {
            scale_box(native, sw, sh, &mut self.frame, dw, dh, &self.x_map);
        }
        self.overlay_cursor()
    }

    /// Paints the host cursor (XFixes image, premultiplied ARGB) at its scaled position, at native size.
    fn overlay_cursor(&mut self) -> Res<()> {
        let Some(conn) = self.conn.as_ref() else { return Ok(()) };
        let reply = conn.xfixes_get_cursor_image()?.reply()?;
        let (cx, cy) = (reply.x as i32, reply.y as i32);
        if self.hide_viewer_cursor
            && crate::input::LAST_INJECTED_POS.load(Ordering::Relaxed) == crate::input::pack_pos(cx, cy)
        {
            return Ok(());
        }
        if reply.cursor_serial != self.cursor.serial || self.cursor.argb.is_empty() {
            self.cursor = CursorImage {
                serial: reply.cursor_serial,
                width: reply.width as usize,
                height: reply.height as usize,
                xhot: reply.xhot as i32,
                yhot: reply.yhot as i32,
                argb: reply.cursor_image,
            };
        }
        let (rel_x, rel_y) = (cx - self.offset_x, cy - self.offset_y);
        if rel_x < 0 || rel_y < 0 || rel_x > self.screen_width || rel_y > self.screen_height {
            return Ok(());
        }
        let px = (rel_x as i64 * self.width as i64 / self.screen_width as i64) as i32 - self.cursor.xhot;
        let py = (rel_y as i64 * self.height as i64 / self.screen_height as i64) as i32 - self.cursor.yhot;
        blend_cursor(&mut self.frame, self.width as usize, self.height as usize, &self.cursor, px, py);
        Ok(())
    }
}

impl Drop for DesktopCapturer {
    fn drop(&mut self) {
        if let (Some(mut shm), Some(conn)) = (self.shm.take(), self.conn.as_ref()) {
            shm.release(conn);
        }
    }
}

/// Downscales BGRX `src` (sw x sh) into `dst` (dw x dh), averaging a 2x2 block at each sampled position:
/// sharper than bilinear for text at the 1.5-2x ratios the resolution presets use, and cheap.
fn scale_box(src: &[u32], sw: usize, sh: usize, dst: &mut [u32], dw: usize, dh: usize, x_map: &[usize]) {
    for (y, row) in dst.chunks_exact_mut(dw).take(dh).enumerate() {
        let sy = y * sh / dh;
        let r0 = &src[sy * sw..sy * sw + sw];
        let r1 = &src[(sy + 1).min(sh - 1) * sw..][..sw];
        for (d, &sx) in row.iter_mut().zip(x_map) {
            let sx1 = (sx + 1).min(sw - 1);
            let (a, b, c, e) = (r0[sx], r0[sx1], r1[sx], r1[sx1]);
            let ch = |shift: u32| {
                (((a >> shift) & 0xFF) + ((b >> shift) & 0xFF) + ((c >> shift) & 0xFF) + ((e >> shift) & 0xFF) + 2) / 4
            };
            *d = (ch(16) << 16) | (ch(8) << 8) | ch(0);
        }
    }
}

/// Alpha-blends a premultiplied ARGB cursor onto BGRX `frame` with its top-left corner at (`x`, `y`).
fn blend_cursor(frame: &mut [u32], fw: usize, fh: usize, cur: &CursorImage, x: i32, y: i32) {
    for cy in 0..cur.height {
        let fy = y + cy as i32;
        if fy < 0 || fy >= fh as i32 {
            continue;
        }
        for cx in 0..cur.width {
            let fx = x + cx as i32;
            if fx < 0 || fx >= fw as i32 {
                continue;
            }
            let s = cur.argb[cy * cur.width + cx];
            let alpha = s >> 24;
            if alpha == 0 {
                continue;
            }
            let d = &mut frame[fy as usize * fw + fx as usize];
            let inv = 255 - alpha;
            let ch = |shift: u32| (((s >> shift) & 0xFF) + (((*d >> shift) & 0xFF) * inv + 127) / 255).min(255);
            *d = (ch(16) << 16) | (ch(8) << 8) | ch(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_scaling_averages_neighbours() {
        let src = vec![0x0000_0000, 0x00FF_FFFF, 0x0000_0000, 0x00FF_FFFF];
        let mut dst = vec![0u32; 1];
        scale_box(&src, 2, 2, &mut dst, 1, 1, &[0]);
        assert_eq!(dst[0], 0x0080_8080);
    }

    #[test]
    fn opaque_cursor_pixels_replace_the_frame_and_clip_at_edges() {
        let cur = CursorImage { width: 2, height: 1, argb: vec![0xFF11_2233, 0x0000_0000], ..Default::default() };
        let mut frame = vec![0x00AA_AAAA; 4];
        blend_cursor(&mut frame, 2, 2, &cur, 1, 1);
        assert_eq!(frame, vec![0x00AA_AAAA, 0x00AA_AAAA, 0x00AA_AAAA, 0x0011_2233]);
    }

    #[test]
    fn monitors_are_never_empty() {
        assert!(!enumerate_monitors().is_empty());
    }

    /// Needs an X server (e.g. `xvfb-run cargo test -- --ignored`): paints the root window and captures it.
    #[test]
    #[ignore]
    fn captures_the_root_window_live() {
        use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt as _};
        // Connect first and stay connected: a bare Xvfb resets when its last client leaves, and a connection
        // made during the reset fails (desktop sessions always have clients; CI also passes -noreset).
        let (conn, root) = connect().unwrap();
        assert_eq!(host_support(), HostSupport::Supported);
        let aux = ChangeWindowAttributesAux::new().background_pixel(0x00FF_8000);
        conn.change_window_attributes(root, &aux).unwrap();
        conn.clear_area(false, root, 0, 0, 0, 0).unwrap();
        conn.get_input_focus().unwrap().reply().unwrap();
        let mon = enumerate_monitors()[0];
        let mut cap = DesktopCapturer::new_for_monitor(mon, mon.width / 2);
        cap.grab(0);
        assert_ne!(cap.backend(), "none");
        assert_eq!(cap.pixels().len(), (cap.width() * cap.height()) as usize);
        // The bottom-right corner is the painted background: the pointer (and its drawn cursor) never goes there,
        // even while the input test moves it.
        assert_eq!(*cap.pixels().last().unwrap(), 0x00FF_8000, "backend {}", cap.backend());
        assert!((1..=13).contains(&cursor_shape()) || cursor_shape() == 0);

        // The thumbnail path (monitor wall) uses the same capturer through `ThumbnailCapturer`.
        let mut thumb = crate::capture::ThumbnailCapturer::new_for_monitor(mon, 320);
        thumb.capture_frame();
        assert_eq!((thumb.width, thumb.pixels().len()), (320, (320 * thumb.height) as usize));
        assert_eq!(*thumb.pixels().last().unwrap(), 0x00FF_8000);
    }
}
