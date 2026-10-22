#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub struct DirtyRectDetector {
    prev_sample: Vec<u32>,
    grid_w: usize,
    _grid_h: usize,
}

impl DirtyRectDetector {
    pub fn new(width: i32, height: i32) -> Self {
        let grid_w = ((width + 63) / 64) as usize;
        let grid_h = ((height + 63) / 64) as usize;
        Self {
            prev_sample: vec![0; grid_w * grid_h],
            grid_w,
            _grid_h: grid_h,
        }
    }

    pub fn detect_changes(&mut self, current_sample: &[u32]) -> (bool, Vec<Rect>) {
        let mut changed_blocks = Vec::new();
        let mut has_change = false;

        for (i, (&curr, prev)) in current_sample.iter().zip(self.prev_sample.iter_mut()).enumerate() {
            if curr != *prev {
                has_change = true;
                *prev = curr;
                let bx = (i % self.grid_w) as i32 * 64;
                let by = (i / self.grid_w) as i32 * 64;
                changed_blocks.push(Rect {
                    x: bx,
                    y: by,
                    width: 64,
                    height: 64,
                });
            }
        }

        (has_change, changed_blocks)
    }

    pub fn reset(&mut self) {
        self.prev_sample.fill(0);
    }
}
