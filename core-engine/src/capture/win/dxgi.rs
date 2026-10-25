//! DXGI Desktop Duplication capture. Compared with GDI BitBlt it reads the DWM surface on the GPU, waits for
//! the next present instead of polling, and reports exactly which regions changed or moved.

use std::ffi::c_void;
use std::ptr;

use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_MOVE_RECT, IDXGIAdapter, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use crate::capture::MonitorBounds;
use crate::codec::frame::MoveRect;
use crate::diff::Rect;

// Shared GDI declarations live in the parent module; only the cursor-specific calls are declared here.
use super::win32::{
    BITMAPINFO, BITMAPINFOHEADER, CURSORINFO, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
    GetCursorInfo, HBITMAP, HDC, HGDIOBJ, HICON, POINT, SelectObject,
};

type BOOL = i32;

const CURSOR_SHOWING: u32 = 1;
const DI_NORMAL: u32 = 3;
/// Cursor tile; large accessibility cursors are clipped to it.
const TILE: i32 = 128;

#[repr(C)]
struct ICONINFO {
    f_icon: BOOL,
    x_hotspot: u32,
    y_hotspot: u32,
    hbm_mask: HBITMAP,
    hbm_color: HBITMAP,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetIconInfo(h_icon: HICON, piconinfo: *mut ICONINFO) -> BOOL;
    fn DrawIconEx(
        hdc: HDC,
        x: i32,
        y: i32,
        h_icon: HICON,
        cx: i32,
        cy: i32,
        step: u32,
        hbr: *mut c_void,
        flags: u32,
    ) -> BOOL;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn GdiFlush() -> BOOL;
}

/// Source span and fixed-point weights (sum 256) of one output column or row for area-average scaling.
struct Span {
    start: usize,
    weights: Vec<u32>,
}

fn spans(src: usize, dst: usize) -> Vec<Span> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|i| {
            let (a, b) = (i as f64 * scale, ((i + 1) as f64 * scale).min(src as f64));
            let start = a.floor() as usize;
            let end = (b.ceil() as usize).clamp(start + 1, src);
            let mut weights: Vec<u32> = (start..end)
                .map(|s| ((b.min(s as f64 + 1.0) - a.max(s as f64)).max(0.0) / (b - a) * 256.0).round() as u32)
                .collect();
            // Rounding may leave the sum at 255/257; fix it on the largest tap so flat colours stay exact.
            let sum: u32 = weights.iter().sum();
            if let Some(m) = weights.iter_mut().max() {
                *m = (*m + 256).saturating_sub(sum);
            }
            Span { start, weights }
        })
        .collect()
}

/// Cursor drawn into the frame, with the pixels it covered so it can be removed before the next update.
struct CursorOverlay {
    rect: Rect,
    under: Vec<u32>,
}

/// DXGI Desktop Duplication of one monitor: GPU copy, dirty and move rectangles, optional scaling.
pub struct DxgiCapturer {
    /// Left edge of the captured monitor in virtual-desktop coordinates.
    pub offset_x: i32,
    /// Top edge of the captured monitor in virtual-desktop coordinates.
    pub offset_y: i32,
    /// Native monitor width.
    pub screen_width: i32,
    /// Native monitor height.
    pub screen_height: i32,
    /// Output width.
    pub width: i32,
    /// Output height.
    pub height: i32,
    /// Viewer renders its own cursor, so skip painting it while the viewer drives it (same rule as GDI).
    pub hide_viewer_cursor: bool,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    output: IDXGIOutput1,
    dup: Option<IDXGIOutputDuplication>,
    staging: ID3D11Texture2D,
    frame: Vec<u32>,
    xspans: Vec<Span>,
    yspans: Vec<Span>,
    /// Output-space regions rewritten by the last `grab`; None means the whole frame.
    dirty: Option<Vec<Rect>>,
    moves: Vec<MoveRect>,
    fresh: bool,
    has_image: bool,
    cursor: Option<CursorOverlay>,
    tile_dc: HDC,
    tile_bmp: HBITMAP,
    tile_old: HGDIOBJ,
    tile_bits: *mut u32,
    hotspots: Vec<(usize, i32, i32)>,
    rect_buf: Vec<u8>,
    /// Horizontally reduced source rows used by the scaler.
    hrows: Vec<u32>,
}

unsafe impl Send for DxgiCapturer {}

/// Result of one `grab`.
#[derive(Debug, PartialEq, Eq)]
pub enum Grab {
    /// Nothing changed within the timeout.
    Unchanged,
    /// The frame buffer was updated; see `dirty_hint` / `moves`.
    Changed,
}

fn output_size(sw: i32, sh: i32, max_width: i32) -> (i32, i32) {
    if max_width > 0 && sw > max_width {
        let w = max_width & !1;
        let h = ((sh as i64 * w as i64) / sw as i64) as i32 & !1;
        (w.max(2), h.max(2))
    } else {
        (sw, sh)
    }
}

impl DxgiCapturer {
    /// Opens duplication for the output whose desktop origin matches `bounds`. Errors mean "use GDI".
    pub fn new_for_monitor(bounds: MonitorBounds, max_width: i32) -> windows::core::Result<Self> {
        super::enable_per_monitor_dpi_awareness();
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut found = None;
            let mut a = 0;
            while let Ok(adapter) = factory.EnumAdapters1(a) {
                a += 1;
                let mut o = 0;
                while let Ok(output) = adapter.EnumOutputs(o) {
                    o += 1;
                    let desc = output.GetDesc()?;
                    let r = desc.DesktopCoordinates;
                    if desc.AttachedToDesktop.as_bool() && r.left == bounds.left && r.top == bounds.top {
                        found = Some((adapter.cast::<IDXGIAdapter>()?, output, desc.Rotation, r));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            let Some((adapter, output, rotation, r)) = found else {
                return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
            };
            // Rotated outputs deliver an unrotated surface; GDI handles those.
            if rotation != DXGI_MODE_ROTATION_IDENTITY && rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
                return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_NOTIMPL));
            }
            let (sw, sh) = (r.right - r.left, r.bottom - r.top);
            let mut device = None;
            let mut ctx = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )?;
            let (device, ctx) = (device.unwrap(), ctx.unwrap());
            let output: IDXGIOutput1 = output.cast()?;
            let dup = output.DuplicateOutput(&device)?;

            let desc = D3D11_TEXTURE2D_DESC {
                Width: sw as u32,
                Height: sh as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;

            let (w, h) = output_size(sw, sh, max_width);
            let tile_dc = CreateCompatibleDC(ptr::null_mut());
            let bmi = BITMAPINFO {
                bmi_header: BITMAPINFOHEADER {
                    bi_size: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    bi_width: TILE,
                    bi_height: -TILE,
                    bi_planes: 1,
                    bi_bit_count: 32,
                    bi_compression: 0,
                    bi_size_image: 0,
                    bi_x_pels_per_meter: 0,
                    bi_y_pels_per_meter: 0,
                    bi_clr_used: 0,
                    bi_clr_important: 0,
                },
                bmi_colors: [0],
            };
            let mut bits: *mut c_void = ptr::null_mut();
            let tile_bmp = CreateDIBSection(tile_dc, &bmi, 0, &mut bits, ptr::null_mut(), 0);
            let tile_old = SelectObject(tile_dc, tile_bmp);

            Ok(Self {
                offset_x: r.left,
                offset_y: r.top,
                screen_width: sw,
                screen_height: sh,
                width: w,
                height: h,
                hide_viewer_cursor: false,
                device,
                ctx,
                output,
                dup: Some(dup),
                staging: staging.unwrap(),
                frame: vec![0; (w * h) as usize],
                xspans: spans(sw as usize, w as usize),
                yspans: spans(sh as usize, h as usize),
                dirty: None,
                moves: Vec::new(),
                fresh: true,
                has_image: false,
                cursor: None,
                tile_dc,
                tile_bmp,
                tile_old,
                tile_bits: bits as *mut u32,
                hotspots: Vec::new(),
                rect_buf: Vec::new(),
                hrows: Vec::new(),
            })
        }
    }

    /// The last captured image as BGRX pixels, `width` per row.
    pub fn pixels(&self) -> &[u32] {
        &self.frame
    }

    /// False until the first real desktop image arrived (the frame is black before that).
    pub fn has_image(&self) -> bool {
        self.has_image
    }

    /// Regions of `pixels` changed by the last `Changed` grab (None = everything).
    pub fn dirty_hint(&self) -> Option<&[Rect]> {
        self.dirty.as_deref()
    }

    /// Framebuffer moves of the last grab (only when not scaling).
    pub fn moves(&self) -> &[MoveRect] {
        &self.moves
    }

    /// Waits up to `timeout_ms` for a new desktop image or cursor update.
    /// Errors mean the duplication is gone for good (caller falls back to GDI).
    pub fn grab(&mut self, timeout_ms: u32) -> windows::core::Result<Grab> {
        self.moves.clear();
        if self.dup.is_none() {
            // Recreate after ACCESS_LOST (mode change, full-screen app, desktop switch).
            self.dup = Some(unsafe { self.output.DuplicateOutput(&self.device)? });
            self.fresh = true;
        }
        let dup = self.dup.clone().unwrap();
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe { dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                self.dirty = Some(Vec::new());
                return Ok(Grab::Unchanged);
            }
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                self.dup = None;
                self.dirty = Some(Vec::new());
                return Ok(Grab::Unchanged);
            }
            Err(e) => return Err(e),
        }

        let mut dirty: Vec<Rect> = Vec::new();
        // Take the old cursor out first so updates land on clean pixels; its area must be re-examined.
        if let Some(c) = self.cursor.take() {
            self.blit_rect(&c.rect, &c.under);
            dirty.push(c.rect);
        }

        // Pointer-only updates carry no valid desktop image (it can even be black right after creation).
        let image_updated = info.LastPresentTime != 0;
        let result = match resource.filter(|_| image_updated) {
            Some(res) => self.copy_image(&dup, &res, &mut dirty),
            None => Ok(()),
        };
        unsafe {
            let _ = dup.ReleaseFrame();
        }
        if let Err(e) = result {
            if e.code() == DXGI_ERROR_ACCESS_LOST {
                self.dup = None;
            } else {
                return Err(e);
            }
        }

        if let Some(r) = self.draw_cursor() {
            dirty.push(r);
        }
        if self.fresh && image_updated {
            self.fresh = false;
            self.has_image = true;
            self.dirty = None;
        } else {
            self.dirty = Some(dirty);
        }
        Ok(Grab::Changed)
    }

    fn copy_image(
        &mut self,
        dup: &IDXGIOutputDuplication,
        res: &IDXGIResource,
        dirty: &mut Vec<Rect>,
    ) -> windows::core::Result<()> {
        let (src_rects, src_moves) = if self.fresh {
            (vec![RECT { left: 0, top: 0, right: self.screen_width, bottom: self.screen_height }], Vec::new())
        } else {
            self.frame_metadata(dup)?
        };
        if src_rects.is_empty() && src_moves.is_empty() {
            return Ok(());
        }
        let scaled = self.width != self.screen_width || self.height != self.screen_height;
        let mut regions: Vec<RECT> = src_rects;
        for m in &src_moves {
            let (w, h) =
                (m.DestinationRect.right - m.DestinationRect.left, m.DestinationRect.bottom - m.DestinationRect.top);
            if !scaled {
                self.moves.push(MoveRect {
                    src_x: m.SourcePoint.x as u16,
                    src_y: m.SourcePoint.y as u16,
                    dst_x: m.DestinationRect.left as u16,
                    dst_y: m.DestinationRect.top as u16,
                    w: w as u16,
                    h: h as u16,
                });
            }
            // The destination is rewritten from the new image either way; the encoder's exact diff then
            // finds that a moved area matches the moved reference and sends nothing for it.
            regions.push(m.DestinationRect);
        }

        unsafe {
            let tex: ID3D11Texture2D = res.cast()?;
            self.ctx.CopyResource(&self.staging, &tex);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let pitch = mapped.RowPitch as usize / 4;
            let src = std::slice::from_raw_parts(mapped.pData as *const u32, pitch * self.screen_height as usize);
            for r in &regions {
                let out = self.update_region(src, pitch, r);
                if out.width > 0 && out.height > 0 {
                    dirty.push(out);
                }
            }
            self.ctx.Unmap(&self.staging, 0);
        }
        Ok(())
    }

    fn frame_metadata(
        &mut self,
        dup: &IDXGIOutputDuplication,
    ) -> windows::core::Result<(Vec<RECT>, Vec<DXGI_OUTDUPL_MOVE_RECT>)> {
        unsafe {
            let mut need = 0u32;
            let mut moves = Vec::new();
            loop {
                let cap = self.rect_buf.len() as u32;
                match dup.GetFrameMoveRects(cap, self.rect_buf.as_mut_ptr() as *mut DXGI_OUTDUPL_MOVE_RECT, &mut need) {
                    Ok(()) => {
                        let n = need as usize / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>();
                        moves.extend_from_slice(std::slice::from_raw_parts(
                            self.rect_buf.as_ptr() as *const DXGI_OUTDUPL_MOVE_RECT,
                            n,
                        ));
                        break;
                    }
                    Err(_) if need as usize > self.rect_buf.len() => self.rect_buf.resize(need as usize + 64, 0),
                    Err(e) => return Err(e),
                }
            }
            let mut rects = Vec::new();
            loop {
                let cap = self.rect_buf.len() as u32;
                match dup.GetFrameDirtyRects(cap, self.rect_buf.as_mut_ptr() as *mut RECT, &mut need) {
                    Ok(()) => {
                        let n = need as usize / std::mem::size_of::<RECT>();
                        rects.extend_from_slice(std::slice::from_raw_parts(self.rect_buf.as_ptr() as *const RECT, n));
                        break;
                    }
                    Err(_) if need as usize > self.rect_buf.len() => self.rect_buf.resize(need as usize + 64, 0),
                    Err(e) => return Err(e),
                }
            }
            Ok((rects, moves))
        }
    }

    /// Copies (and downscales) one source-space rectangle into the frame; returns the output-space rect.
    fn update_region(&mut self, src: &[u32], pitch: usize, r: &RECT) -> Rect {
        let (sw, sh, w, h) = (self.screen_width, self.screen_height, self.width, self.height);
        let (l, t) = (r.left.clamp(0, sw), r.top.clamp(0, sh));
        let (rr, b) = (r.right.clamp(0, sw), r.bottom.clamp(0, sh));
        if rr <= l || b <= t {
            return Rect { x: 0, y: 0, width: 0, height: 0 };
        }
        let fw = w as usize;
        if w == sw && h == sh {
            for y in t as usize..b as usize {
                self.frame[y * fw + l as usize..y * fw + rr as usize]
                    .copy_from_slice(&src[y * pitch + l as usize..y * pitch + rr as usize]);
            }
            return Rect { x: l, y: t, width: rr - l, height: b - t };
        }
        // Output pixels whose source footprint touches the rect (1 px margin for the filter).
        let ox0 = ((l as i64 * w as i64) / sw as i64 - 1).max(0) as usize;
        let oy0 = ((t as i64 * h as i64) / sh as i64 - 1).max(0) as usize;
        let ox1 = (((rr as i64 * w as i64) + sw as i64 - 1) / sw as i64 + 1).min(w as i64) as usize;
        let oy1 = (((b as i64 * h as i64) + sh as i64 - 1) / sh as i64 + 1).min(h as i64) as usize;
        // Separable area average: each source row of the band is reduced horizontally once (R|B and G
        // packed in u32 lanes, weights sum to 256 so lanes cannot overflow), then rows are blended vertically.
        let (sy0, sy1) = (self.yspans[oy0].start, self.yspans[oy1 - 1].start + self.yspans[oy1 - 1].weights.len());
        let ow = ox1 - ox0;
        self.hrows.resize((sy1 - sy0) * ow * 2, 0);
        for sy in sy0..sy1 {
            let row = &src[sy * pitch..sy * pitch + sw as usize];
            let out = &mut self.hrows[(sy - sy0) * ow * 2..][..ow * 2];
            for (k, xs) in self.xspans[ox0..ox1].iter().enumerate() {
                let (mut rb, mut g) = (0u32, 0u32);
                for (&p, &wx) in row[xs.start..xs.start + xs.weights.len()].iter().zip(&xs.weights) {
                    rb += (p & 0x00FF_00FF) * wx;
                    g += ((p >> 8) & 0xFF) * wx;
                }
                // rb: R in bits 16..31 and B in bits 0..15, each scaled by 256.
                out[k * 2] = rb;
                out[k * 2 + 1] = g;
            }
        }
        for oy in oy0..oy1 {
            let ys = &self.yspans[oy];
            let dst = &mut self.frame[oy * fw + ox0..oy * fw + ox1];
            for (k, d) in dst.iter_mut().enumerate() {
                let (mut r_, mut g_, mut b_) = (0u32, 0u32, 0u32);
                for (j, &wy) in ys.weights.iter().enumerate() {
                    let base = ((ys.start + j - sy0) * ow + k) * 2;
                    let (rb, g) = (self.hrows[base], self.hrows[base + 1]);
                    r_ += (rb >> 16) * wy;
                    b_ += (rb & 0xFFFF) * wy;
                    g_ += g * wy;
                }
                *d = ((r_ + 32768) >> 16) << 16 | ((g_ + 32768) >> 16) << 8 | ((b_ + 32768) >> 16);
            }
        }
        Rect { x: ox0 as i32, y: oy0 as i32, width: (ox1 - ox0) as i32, height: (oy1 - oy0) as i32 }
    }

    fn blit_rect(&mut self, r: &Rect, px: &[u32]) {
        let fw = self.width as usize;
        for row in 0..r.height as usize {
            let d = (r.y as usize + row) * fw + r.x as usize;
            self.frame[d..d + r.width as usize].copy_from_slice(&px[row * r.width as usize..][..r.width as usize]);
        }
    }

    fn hotspot(&mut self, cursor: HICON) -> (i32, i32) {
        if let Some(&(_, x, y)) = self.hotspots.iter().find(|(h, _, _)| *h == cursor as usize) {
            return (x, y);
        }
        let mut ii =
            ICONINFO { f_icon: 0, x_hotspot: 0, y_hotspot: 0, hbm_mask: ptr::null_mut(), hbm_color: ptr::null_mut() };
        let hs = unsafe {
            if GetIconInfo(cursor, &mut ii) != 0 {
                for bmp in [ii.hbm_mask, ii.hbm_color] {
                    if !bmp.is_null() {
                        DeleteObject(bmp);
                    }
                }
                (ii.x_hotspot as i32, ii.y_hotspot as i32)
            } else {
                (0, 0)
            }
        };
        if self.hotspots.len() > 64 {
            self.hotspots.clear();
        }
        self.hotspots.push((cursor as usize, hs.0, hs.1));
        hs
    }

    /// DXGI images exclude the hardware cursor; paint it like the GDI path does (DrawIconEx handles
    /// monochrome/inverting cursors against the real background). Returns the painted rect.
    fn draw_cursor(&mut self) -> Option<Rect> {
        let mut ci = CURSORINFO {
            cb_size: std::mem::size_of::<CURSORINFO>() as u32,
            flags: 0,
            h_cursor: ptr::null_mut(),
            pt_screen_pos: POINT { x: 0, y: 0 },
        };
        unsafe {
            if GetCursorInfo(&mut ci) == 0
                || (ci.flags & CURSOR_SHOWING) == 0
                || ci.h_cursor.is_null()
                || self.tile_bits.is_null()
            {
                return None;
            }
        }
        if self.hide_viewer_cursor
            && crate::input::LAST_INJECTED_POS.load(std::sync::atomic::Ordering::Relaxed)
                == crate::input::pack_pos(ci.pt_screen_pos.x, ci.pt_screen_pos.y)
        {
            return None;
        }
        let (rel_x, rel_y) = (ci.pt_screen_pos.x - self.offset_x, ci.pt_screen_pos.y - self.offset_y);
        if rel_x < 0 || rel_y < 0 || rel_x > self.screen_width || rel_y > self.screen_height {
            return None;
        }
        let (hx, hy) = self.hotspot(ci.h_cursor);
        let cx = (rel_x as i64 * self.width as i64 / self.screen_width as i64) as i32 - hx;
        let cy = (rel_y as i64 * self.height as i64 / self.screen_height as i64) as i32 - hy;
        let (x0, y0) = (cx.max(0), cy.max(0));
        let (x1, y1) = ((cx + TILE).min(self.width), (cy + TILE).min(self.height));
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let rect = Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
        let (rw, fw) = (rect.width as usize, self.width as usize);
        let mut under = Vec::with_capacity(rw * rect.height as usize);
        unsafe {
            let tile = std::slice::from_raw_parts_mut(self.tile_bits, (TILE * TILE) as usize);
            let (tx, ty) = ((x0 - cx) as usize, (y0 - cy) as usize);
            for row in 0..rect.height as usize {
                let s = &self.frame[(y0 as usize + row) * fw + x0 as usize..][..rw];
                under.extend_from_slice(s);
                tile[(ty + row) * TILE as usize + tx..][..rw].copy_from_slice(s);
            }
            DrawIconEx(self.tile_dc, 0, 0, ci.h_cursor, 0, 0, 0, ptr::null_mut(), DI_NORMAL);
            GdiFlush();
            for row in 0..rect.height as usize {
                let src = &tile[(ty + row) * TILE as usize + tx..][..rw];
                let d = &mut self.frame[(y0 as usize + row) * fw + x0 as usize..][..rw];
                for (o, &p) in d.iter_mut().zip(src) {
                    *o = p & 0x00FF_FFFF;
                }
            }
        }
        self.cursor = Some(CursorOverlay { rect, under });
        Some(rect)
    }
}

impl Drop for DxgiCapturer {
    fn drop(&mut self) {
        unsafe {
            if let Some(dup) = self.dup.take() {
                let _ = dup.ReleaseFrame();
            }
            SelectObject(self.tile_dc, self.tile_old);
            DeleteObject(self.tile_bmp);
            DeleteDC(self.tile_dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_cover_source_with_exact_weights() {
        for (src, dst) in [(1920, 1728), (2880, 854), (1200, 720), (100, 100)] {
            let sp = spans(src, dst);
            assert_eq!(sp.len(), dst);
            for s in &sp {
                assert_eq!(s.weights.iter().sum::<u32>(), 256, "{src}->{dst}");
                assert!(s.start + s.weights.len() <= src);
            }
            assert_eq!(sp.last().map(|s| s.start + s.weights.len()), Some(src));
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// `cargo test --release --lib bench_dxgi_capture -- --ignored --nocapture` (needs a real desktop).
    #[test]
    #[ignore]
    fn bench_dxgi_capture() {
        let mon = crate::capture::enumerate_monitors()[0];
        let src: Vec<u32> = (0..(mon.width * mon.height) as u32).collect();
        let full = RECT { left: 0, top: 0, right: mon.width, bottom: mon.height };
        for max_w in [0, 1728, 1280] {
            let mut d = DxgiCapturer::new_for_monitor(mon, max_w).expect("DXGI duplication");
            let t = std::time::Instant::now();
            d.grab(500).unwrap();
            let first = t.elapsed();
            let t = std::time::Instant::now();
            for _ in 0..10 {
                d.update_region(&src, mon.width as usize, &full);
            }
            let per_full = t.elapsed() / 10;
            let t = std::time::Instant::now();
            for _ in 0..100 {
                d.grab(0).unwrap();
            }
            eprintln!(
                "dxgi {}x{} -> {}x{}: first grab {:?}, full-frame copy/scale {:?}, idle grab {:?}",
                mon.width,
                mon.height,
                d.width,
                d.height,
                first,
                per_full,
                t.elapsed() / 100
            );
            drop(d);
            let mut g = crate::capture::GdiScreenCapturer::new_for_monitor(mon, max_w);
            let t = std::time::Instant::now();
            for _ in 0..20 {
                g.capture_frame();
            }
            eprintln!("gdi  {}x{} -> {}x{}: {:?}/frame", mon.width, mon.height, g.width, g.height, t.elapsed() / 20);
        }

        // Same desktop through both capturers at native size: nearly all pixels must agree
        // (cursor placement and live content such as clocks may differ).
        let mut d = DxgiCapturer::new_for_monitor(mon, 0).unwrap();
        let t = std::time::Instant::now();
        let mut tries = 0;
        while !d.has_image() && t.elapsed() < std::time::Duration::from_secs(2) {
            d.grab(100).unwrap();
            tries += 1;
        }
        eprintln!("first desktop image after {} grabs / {:?}", tries, t.elapsed());
        let mut g = crate::capture::GdiScreenCapturer::new_for_monitor(mon, 0);
        g.capture_frame();
        let same = d.pixels().iter().zip(g.pixels()).filter(|(a, b)| (**a ^ **b) & 0x00FF_FFFF == 0).count();
        let frac = same as f64 / d.pixels().len() as f64;
        eprintln!("dxgi vs gdi identical pixels: {:.3}%", frac * 100.0);
        let nz = d.pixels().iter().filter(|&&p| p & 0xFFFFFF != 0).count();
        eprintln!(
            "dxgi nonzero {} of {}; samples dxgi {:08x?} gdi {:08x?}",
            nz,
            d.pixels().len(),
            &d.pixels()[500_000..500_004],
            &g.pixels()[500_000..500_004]
        );
        assert!(frac > 0.95);
    }
}
