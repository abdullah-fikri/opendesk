//! Block-level change detection between consecutive frames.

/// Axis-aligned rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

/// Exact block-level change detection against a private copy of the last frame. Unlike the sampled
/// grid hash it never misses a one-pixel caret or glyph, which matters once only dirty regions are sent.
pub struct BlockDiff {
    /// Frame width.
    pub width: i32,
    /// Frame height.
    pub height: i32,
    /// Block size in pixels.
    pub block: i32,
    /// Blocks per row.
    pub grid_w: usize,
    /// Block rows.
    pub grid_h: usize,
    prev: Vec<u32>,
    primed: bool,
}

impl BlockDiff {
    /// A detector for `width` x `height` frames compared in `block` x `block` tiles.
    pub fn new(width: i32, height: i32, block: i32) -> Self {
        let grid_w = ((width + block - 1) / block) as usize;
        let grid_h = ((height + block - 1) / block) as usize;
        Self { width, height, block, grid_w, grid_h, prev: vec![0; (width * height) as usize], primed: false }
    }

    /// Compares `cur` (stride in pixels) against the previous frame and returns the dirty block map
    /// (row-major, grid_w x grid_h). Only blocks touching `hint` are examined when given (OS dirty rects).
    /// The first call marks every block dirty.
    pub fn diff(&mut self, cur: &[u32], stride: usize, hint: Option<&[Rect]>) -> Vec<bool> {
        let (w, h, b) = (self.width as usize, self.height as usize, self.block as usize);
        let mut candidates = vec![hint.is_none() || !self.primed; self.grid_w * self.grid_h];
        if let Some(rects) = hint.filter(|_| self.primed) {
            for r in rects {
                let x0 = (r.x.max(0) as usize) / b;
                let y0 = (r.y.max(0) as usize) / b;
                let x1 = ((r.x + r.width).clamp(0, self.width) as usize).div_ceil(b).min(self.grid_w);
                let y1 = ((r.y + r.height).clamp(0, self.height) as usize).div_ceil(b).min(self.grid_h);
                for gy in y0..y1 {
                    candidates[gy * self.grid_w + x0..gy * self.grid_w + x1.max(x0)].fill(true);
                }
            }
        }
        let first = !self.primed;
        self.primed = true;
        let mut dirty = vec![false; candidates.len()];
        for gy in 0..self.grid_h {
            let y0 = gy * b;
            let y1 = (y0 + b).min(h);
            for gx in 0..self.grid_w {
                let idx = gy * self.grid_w + gx;
                if !candidates[idx] {
                    continue;
                }
                let x0 = gx * b;
                let x1 = (x0 + b).min(w);
                let mut changed = first;
                for y in y0..y1 {
                    let c = &cur[y * stride + x0..y * stride + x1];
                    let p = &mut self.prev[y * w + x0..y * w + x1];
                    if changed {
                        p.copy_from_slice(c);
                    } else if c != p {
                        changed = true;
                        // Copy the whole block (rows above this one already matched).
                        p.copy_from_slice(c);
                        for yy in y + 1..y1 {
                            self.prev[yy * w + x0..yy * w + x1]
                                .copy_from_slice(&cur[yy * stride + x0..yy * stride + x1]);
                        }
                        break;
                    }
                }
                dirty[idx] = changed;
            }
        }
        dirty
    }

    /// Next diff reports everything dirty (used after a resize or a key-frame request).
    pub fn reset(&mut self) {
        self.primed = false;
    }

    /// Applies a framebuffer move to the reference copy so the moved content is not re-detected as dirty.
    pub fn apply_move(&mut self, src_x: i32, src_y: i32, dst_x: i32, dst_y: i32, w: i32, h: i32) {
        let fw = self.width as usize;
        let w = w.min(self.width - src_x).min(self.width - dst_x).max(0) as usize;
        let h = h.min(self.height - src_y).min(self.height - dst_y).max(0) as usize;
        let rows: Box<dyn Iterator<Item = usize>> = if dst_y > src_y { Box::new((0..h).rev()) } else { Box::new(0..h) };
        for row in rows {
            let s = (src_y as usize + row) * fw + src_x as usize;
            let d = (dst_y as usize + row) * fw + dst_x as usize;
            self.prev.copy_within(s..s + w, d);
        }
    }
}

/// Merges a dirty block map into rectangles: horizontal runs per block row, then runs with identical
/// x-span in consecutive rows are joined vertically. Rects are clipped to the frame.
pub fn merge_blocks(dirty: &[bool], grid_w: usize, block: i32, width: i32, height: i32) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    // Rects that ended on the previous block row, keyed by (x, width), so they can grow downwards.
    let mut open: Vec<usize> = Vec::new();
    for (gy, row) in dirty.chunks(grid_w.max(1)).enumerate() {
        let y = gy as i32 * block;
        let mut next_open = Vec::new();
        let mut gx = 0;
        while gx < row.len() {
            if !row[gx] {
                gx += 1;
                continue;
            }
            let start = gx;
            while gx < row.len() && row[gx] {
                gx += 1;
            }
            let x = start as i32 * block;
            let w = (gx as i32 * block).min(width) - x;
            let h = (y + block).min(height) - y;
            if let Some(&i) = open.iter().find(|&&i| out[i].x == x && out[i].width == w) {
                out[i].height += h;
                next_open.push(i);
            } else {
                out.push(Rect { x, y, width: w, height: h });
                next_open.push(out.len() - 1);
            }
        }
        open = next_open;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_joins_runs_and_clips_edges() {
        // 4x3 grid of 32px blocks over a 100x70 frame:
        // row0: X X . X
        // row1: X X . X
        // row2: . X X .
        let d = [true, true, false, true, true, true, false, true, false, true, true, false];
        let rects = merge_blocks(&d, 4, 32, 100, 70);
        assert_eq!(
            rects,
            vec![
                Rect { x: 0, y: 0, width: 64, height: 64 },
                Rect { x: 96, y: 0, width: 4, height: 64 },
                Rect { x: 32, y: 64, width: 64, height: 6 },
            ]
        );
    }

    #[test]
    fn block_diff_finds_single_pixel_changes_only_where_hinted() {
        let (w, h) = (96, 64);
        let mut frame = vec![7u32; (w * h) as usize];
        let mut bd = BlockDiff::new(w, h, 32);
        assert!(bd.diff(&frame, w as usize, None).iter().all(|&d| d), "first frame is all dirty");
        assert!(bd.diff(&frame, w as usize, None).iter().all(|&d| !d));
        frame[(40 * w + 70) as usize] = 8; // block (2,1)
        let hint = [Rect { x: 0, y: 0, width: 10, height: 10 }];
        assert!(bd.diff(&frame, w as usize, Some(&hint)).iter().all(|&d| !d), "outside the hint");
        let d = bd.diff(&frame, w as usize, None);
        assert_eq!(d.iter().filter(|&&x| x).count(), 1);
        assert!(d[3 + 2]);
        assert!(bd.diff(&frame, w as usize, None).iter().all(|&d| !d), "reference updated");
    }
}
