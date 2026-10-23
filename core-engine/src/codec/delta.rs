//! Dirty-region delta frames: the host packs changed rectangles into one atlas image (one JPEG header
//! and one encode call per frame), the viewer keeps a persistent framebuffer and blits the patches back.

use super::frame::{FrameHeader, KIND_DELTA, KIND_KEY, MoveRect, Patch};
use crate::diff::Rect;

/// JPEG works on 16x16 MCUs with 4:2:0 chroma; aligning atlas cells keeps one patch from bleeding into the next.
const ALIGN: i32 = 16;

fn align_up(v: i32) -> i32 {
    (v + ALIGN - 1) / ALIGN * ALIGN
}

/// Shelf-packs `rects` (tallest first) into an atlas no wider than `max_w`.
/// Returns (atlas_w, atlas_h, patches in input order).
pub fn pack_atlas(rects: &[Rect], max_w: i32) -> (i32, i32, Vec<Patch>) {
    if rects.is_empty() {
        return (0, 0, Vec::new());
    }
    let area: i64 = rects.iter().map(|r| align_up(r.width) as i64 * align_up(r.height) as i64).sum();
    let widest = rects.iter().map(|r| align_up(r.width)).max().unwrap_or(ALIGN);
    // Roughly square atlas keeps the encoded area (and so encode time) close to the dirty area.
    let target = align_up(((area as f64).sqrt() * 1.2) as i32);
    let atlas_w = target.max(widest).min(align_up(max_w).max(widest));

    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((rects[i].height, rects[i].width)));

    let mut patches = vec![Patch { dst_x: 0, dst_y: 0, w: 0, h: 0, atlas_x: 0, atlas_y: 0 }; rects.len()];
    let (mut x, mut y, mut shelf_h) = (0i32, 0i32, 0i32);
    for i in order {
        let r = rects[i];
        let (cw, ch) = (align_up(r.width), align_up(r.height));
        if x + cw > atlas_w {
            y += shelf_h;
            x = 0;
            shelf_h = 0;
        }
        patches[i] = Patch {
            dst_x: r.x as u16,
            dst_y: r.y as u16,
            w: r.width as u16,
            h: r.height as u16,
            atlas_x: x as u16,
            atlas_y: y as u16,
        };
        x += cw;
        shelf_h = shelf_h.max(ch);
    }
    (atlas_w, y + shelf_h, patches)
}

/// Copies the patch sources out of a BGRA frame into an atlas. Cell padding repeats the edge pixels so
/// the DCT does not ring against black at low quality.
pub fn build_atlas(src: &[u32], src_stride: usize, patches: &[Patch], atlas_w: i32, atlas_h: i32) -> Vec<u32> {
    let aw = atlas_w as usize;
    let mut atlas = vec![0u32; aw * atlas_h as usize];
    for p in patches {
        let (w, h) = (p.w as usize, p.h as usize);
        if w == 0 || h == 0 {
            continue;
        }
        let (cw, ch) = (align_up(p.w as i32) as usize, align_up(p.h as i32) as usize);
        let (ax, ay) = (p.atlas_x as usize, p.atlas_y as usize);
        for row in 0..ch {
            let sy = p.dst_y as usize + row.min(h - 1);
            let s = &src[sy * src_stride + p.dst_x as usize..][..w];
            let d = &mut atlas[(ay + row) * aw + ax..][..cw];
            d[..w].copy_from_slice(s);
            let edge = s[w - 1];
            d[w..].fill(edge);
        }
    }
    atlas
}

/// What applying one message did to the framebuffer.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// The framebuffer changed and should be shown.
    Updated,
    /// Nothing visible changed (keepalive).
    Unchanged,
    /// Chain broken (lost or out-of-order delta, size change); ask the host for a key frame.
    NeedKey,
}

/// Viewer-side persistent RGBA framebuffer that delta frames are composited into.
#[derive(Default)]
pub struct Compositor {
    /// Framebuffer width.
    pub width: u32,
    /// Framebuffer height.
    pub height: u32,
    /// Framebuffer, RGBA.
    pub fb: Vec<u8>,
    last_seq: Option<u32>,
}

impl Compositor {
    /// An empty compositor; the first message must be a full frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the framebuffer with a decoded full frame (key frame or legacy JPEG).
    pub fn set_full(&mut self, seq: Option<u32>, rgba: Vec<u8>, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.fb = rgba;
        self.last_seq = seq;
    }

    /// Sequence number of the last applied message, if the chain is intact.
    pub fn last_seq(&self) -> Option<u32> {
        self.last_seq
    }

    /// Records that `seq` was consumed without changing the picture (e.g. an H.264 frame still in the decoder).
    pub fn advance(&mut self, seq: u32) {
        self.last_seq = Some(seq);
    }

    /// Forget the chain, e.g. after a decode failure, so the next delta asks for a key frame.
    pub fn invalidate(&mut self) {
        self.last_seq = None;
    }

    /// Applies one delta. `atlas` is the decoded payload (RGBA, width, height) or None when it has no patches.
    pub fn apply_delta(&mut self, h: &FrameHeader, atlas: Option<(&[u8], u32, u32)>) -> Applied {
        if h.kind != KIND_DELTA
            || self.last_seq.is_none_or(|s| s.wrapping_add(1) != h.seq)
            || h.width as u32 != self.width
            || h.height as u32 != self.height
        {
            self.last_seq = None;
            return Applied::NeedKey;
        }
        self.last_seq = Some(h.seq);
        for m in &h.moves {
            self.apply_move(m);
        }
        let Some((src, aw, ah)) = atlas else {
            return if h.moves.is_empty() { Applied::Unchanged } else { Applied::Updated };
        };
        let fw = self.width as usize;
        for p in &h.patches {
            // Clip against both images so a malformed rect can never index out of bounds.
            let w = (p.w as u32).min(self.width.saturating_sub(p.dst_x as u32)).min(aw.saturating_sub(p.atlas_x as u32))
                as usize;
            let rows = (p.h as u32)
                .min(self.height.saturating_sub(p.dst_y as u32))
                .min(ah.saturating_sub(p.atlas_y as u32)) as usize;
            for row in 0..rows {
                let s = ((p.atlas_y as usize + row) * aw as usize + p.atlas_x as usize) * 4;
                let d = ((p.dst_y as usize + row) * fw + p.dst_x as usize) * 4;
                self.fb[d..d + w * 4].copy_from_slice(&src[s..s + w * 4]);
            }
        }
        Applied::Updated
    }

    fn apply_move(&mut self, m: &MoveRect) {
        let (fw, fh) = (self.width as usize, self.height as usize);
        let w = (m.w as usize).min(fw.saturating_sub(m.src_x as usize)).min(fw.saturating_sub(m.dst_x as usize));
        let h = (m.h as usize).min(fh.saturating_sub(m.src_y as usize)).min(fh.saturating_sub(m.dst_y as usize));
        if w == 0 || h == 0 {
            return;
        }
        // Rows are copied in an order that never overwrites a source row before it is read.
        let row_copy = |fb: &mut Vec<u8>, row: usize| {
            let s = ((m.src_y as usize + row) * fw + m.src_x as usize) * 4;
            let d = ((m.dst_y as usize + row) * fw + m.dst_x as usize) * 4;
            fb.copy_within(s..s + w * 4, d);
        };
        if m.dst_y > m.src_y {
            for row in (0..h).rev() {
                row_copy(&mut self.fb, row);
            }
        } else {
            for row in 0..h {
                row_copy(&mut self.fb, row);
            }
        }
    }
}

/// Header for a key frame carrying one full-frame JPEG.
pub fn key_header(seq: u32, width: i32, height: i32) -> FrameHeader {
    FrameHeader {
        kind: KIND_KEY,
        flags: 0,
        seq,
        width: width as u16,
        height: height as u16,
        moves: Vec::new(),
        patches: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn atlas_cells_are_aligned_and_disjoint() {
        let rects = [r(0, 0, 64, 32), r(320, 96, 32, 32), r(1888, 1056, 32, 24), r(0, 512, 256, 64)];
        let (aw, ah, patches) = pack_atlas(&rects, 1920);
        assert!(aw <= 1920 && aw % 16 == 0 && ah % 16 == 0);
        for (i, a) in patches.iter().enumerate() {
            assert_eq!(
                (a.dst_x, a.dst_y, a.w, a.h),
                (rects[i].x as u16, rects[i].y as u16, rects[i].width as u16, rects[i].height as u16)
            );
            assert!(a.atlas_x % 16 == 0 && a.atlas_y % 16 == 0);
            assert!(a.atlas_x as i32 + align_up(a.w as i32) <= aw && a.atlas_y as i32 + align_up(a.h as i32) <= ah);
            for b in &patches[i + 1..] {
                let apart = a.atlas_x as i32 + align_up(a.w as i32) <= b.atlas_x as i32
                    || b.atlas_x as i32 + align_up(b.w as i32) <= a.atlas_x as i32
                    || a.atlas_y as i32 + align_up(a.h as i32) <= b.atlas_y as i32
                    || b.atlas_y as i32 + align_up(b.h as i32) <= a.atlas_y as i32;
                assert!(apart, "overlapping atlas cells {a:?} {b:?}");
            }
        }
    }

    /// BGRA host pixel -> RGBA viewer bytes, as a lossless stand-in for JPEG encode + decode.
    fn to_rgba(px: &[u32]) -> Vec<u8> {
        px.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]).collect()
    }

    #[test]
    fn patches_and_moves_reconstruct_the_source() {
        let (w, h) = (100usize, 70usize);
        let old: Vec<u32> = (0..w * h).map(|i| (i as u32).wrapping_mul(2654435761)).collect();
        // New frame: content scrolled up by 10 rows, a fresh strip at the bottom, and an edited block.
        let mut new = old.clone();
        for y in 0..h - 10 {
            for x in 0..w {
                new[y * w + x] = old[(y + 10) * w + x];
            }
        }
        for (i, px) in new.iter_mut().enumerate().skip((h - 10) * w) {
            *px = 0x00FF_00FF ^ i as u32;
        }
        for y in 5..20 {
            for x in 90..100 {
                new[y * w + x] = 0x0012_3456;
            }
        }

        let mut comp = Compositor::new();
        comp.set_full(Some(7), to_rgba(&old), w as u32, h as u32);
        let dirty = [r(0, 60, 100, 10), r(90, 5, 10, 15)];
        let (aw, ah, patches) = pack_atlas(&dirty, w as i32);
        let atlas = build_atlas(&new, w, &patches, aw, ah);
        let header = FrameHeader {
            kind: KIND_DELTA,
            flags: 0,
            seq: 8,
            width: w as u16,
            height: h as u16,
            moves: vec![MoveRect { src_x: 0, src_y: 10, dst_x: 0, dst_y: 0, w: 100, h: 60 }],
            patches,
        };
        let bytes = header.encode(&[]);
        let (parsed, _) = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(comp.apply_delta(&parsed, Some((&to_rgba(&atlas), aw as u32, ah as u32))), Applied::Updated);
        assert_eq!(comp.fb, to_rgba(&new));

        // A replayed or skipped sequence number breaks the chain until the next key frame.
        assert_eq!(comp.apply_delta(&parsed, None), Applied::NeedKey);
        let mut next = parsed.clone();
        next.seq = 9;
        assert_eq!(comp.apply_delta(&next, None), Applied::NeedKey);
    }

    #[test]
    fn malformed_rects_are_clipped() {
        let mut comp = Compositor::new();
        comp.set_full(Some(0), vec![0; 8 * 8 * 4], 8, 8);
        let h = FrameHeader {
            kind: KIND_DELTA,
            flags: 0,
            seq: 1,
            width: 8,
            height: 8,
            moves: vec![MoveRect { src_x: 6, src_y: 6, dst_x: 0, dst_y: 0, w: 60000, h: 60000 }],
            patches: vec![Patch { dst_x: 4, dst_y: 4, w: 100, h: 100, atlas_x: 0, atlas_y: 0 }],
        };
        assert_eq!(comp.apply_delta(&h, Some((&[255; 16 * 4], 4, 4))), Applied::Updated);
        assert_eq!(comp.fb[(7 * 8 + 7) * 4], 255);
        assert_eq!(comp.fb[0], 0);
    }
}
