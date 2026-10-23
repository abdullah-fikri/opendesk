//! Host-side frame selection and encoding: decides per capture tick whether to send nothing, a legacy
//! full JPEG, a key frame or a dirty-region delta, and at which JPEG quality.

use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use crate::codec::JpegEncoder;
use crate::codec::delta::{build_atlas, key_header, pack_atlas};
use crate::codec::frame::{FLAG_IDR, FrameHeader, KIND_DELTA, KIND_H264, MoveRect};
use crate::codec::h264::H264Encoder;
use crate::diff::{BlockDiff, Rect, merge_blocks};

/// 32px blocks: small enough that a caret or glyph costs ~1 KB, large enough to keep the rect list short.
pub const BLOCK: i32 = 32;
/// Idle ticks between keepalives (the old pipeline resent a full JPEG; deltas send a 20-byte header).
const KEEPALIVE_TICKS: u32 = 30;
/// Idle ticks after motion before blurry (low-quality) regions are resent sharp.
const SETTLE_TICKS: u32 = 2;
/// Consecutive motion ticks before switching to H.264 (one big update is often just a window opening).
const H264_ENTER_STREAK: u32 = 3;
/// A JPEG delta this large counts as motion: several times the size of an H.264 frame at our bitrates.
const H264_MOTION_BYTES: usize = 12_000;
/// Consecutive light-change ticks in H.264 before going back to sharp JPEG regions (e.g. typing after a scroll).
const H264_EXIT_LIGHT: u32 = 20;

/// When H.264 is used (viewer advertised `video-h264`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264Mode {
    /// Never use H.264.
    Off,
    /// Only during sustained heavy motion; text-like updates stay on sharp JPEG regions.
    Auto,
    /// Every changed frame (benchmarking); still ends motion with a sharp JPEG key frame.
    Always,
}

/// What the encoder sent for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SentKind {
    /// Nothing (the frame did not change).
    #[default]
    None,
    /// Full JPEG (viewer without `video-delta`).
    Legacy,
    /// Framed key frame.
    Key,
    /// Framed delta.
    Delta,
    /// H.264 access unit.
    H264,
}

/// Figures about the last encoded frame, for logs and benchmarks.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// What was sent.
    pub kind: SentKind,
    /// Message size in bytes.
    pub bytes: usize,
    /// Encoding time in microseconds.
    pub encode_us: u64,
    /// Changed 32 px blocks.
    pub dirty_blocks: usize,
    /// JPEG quality used.
    pub quality: u32,
}

/// One captured frame: top-down BGRX pixels plus optional OS-provided change information.
pub struct FrameInput<'a> {
    /// BGRX pixels of the captured frame.
    pub pixels: &'a [u32],
    /// Row stride in pixels.
    pub stride: usize,
    /// Regions the OS reports as changed (DXGI); None means "unknown, compare everything".
    pub dirty_hint: Option<&'a [Rect]>,
    /// Framebuffer moves the OS reports (DXGI), applied before dirty regions.
    pub moves: &'a [MoveRect],
}

/// Chooses and encodes what to send for each captured frame (see the module docs).
pub struct StreamEncoder {
    /// Frame width.
    pub width: i32,
    /// Frame height.
    pub height: i32,
    /// Viewer understands framed key/delta messages (`video-delta`).
    pub delta: bool,
    base_quality: u32,
    /// Extra quality reduction (0..=30) chosen from network backpressure.
    congestion_drop: u32,
    jpeg: JpegEncoder,
    diff: BlockDiff,
    /// Blocks last sent at reduced quality, resent sharp once motion settles.
    lowq: Vec<bool>,
    needs_key: bool,
    seq: u32,
    idle_ticks: u32,
    /// Statistics of the last encoded frame.
    pub last: FrameStats,
    h264_mode: H264Mode,
    h264: Option<H264Encoder>,
    /// Encoder being created on a helper thread (hardware MFT init can take a few hundred ms).
    h264_pending: Option<Receiver<Option<H264Encoder>>>,
    h264_failed: bool,
    in_h264: bool,
    /// Consecutive ticks with large changes (big JPEG deltas or heavy block counts).
    motion_streak: u32,
    light_streak: u32,
    congestion: f32,
    /// Name of the H.264 encoder in use, for logs.
    pub h264_name: Option<String>,
}

impl StreamEncoder {
    /// An encoder for `width` x `height` frames at `base_quality`; `delta` enables framed key/delta output.
    pub fn new(width: i32, height: i32, base_quality: u32, delta: bool) -> Self {
        let diff = BlockDiff::new(width, height, BLOCK);
        let blocks = diff.grid_w * diff.grid_h;
        Self {
            width,
            height,
            delta,
            base_quality,
            congestion_drop: 0,
            jpeg: JpegEncoder::new(base_quality),
            diff,
            lowq: vec![false; blocks],
            needs_key: true,
            seq: 0,
            idle_ticks: 0,
            last: FrameStats::default(),
            h264_mode: H264Mode::Off,
            h264: None,
            h264_pending: None,
            h264_failed: false,
            in_h264: false,
            motion_streak: 0,
            light_streak: 0,
            congestion: 0.0,
            h264_name: None,
        }
    }

    /// Allows H.264 for motion. Needs delta framing and even dimensions (NV12).
    pub fn set_h264(&mut self, mode: H264Mode) {
        self.h264_mode = if self.delta && self.width % 2 == 0 && self.height % 2 == 0 { mode } else { H264Mode::Off };
        if self.h264_mode != H264Mode::Off && self.h264.is_none() && self.h264_pending.is_none() && !self.h264_failed {
            // Prepare the encoder off the capture thread so the first motion burst does not stall.
            let (tx, rx) = mpsc::channel();
            let (w, h, bps) = (self.width as u32, self.height as u32, self.h264_bitrate());
            let allow_hw = std::env::var("OPENDESK_H264_HW").map(|v| v != "0").unwrap_or(true);
            std::thread::spawn(move || {
                let _ = tx.send(H264Encoder::new(w, h, 60, bps, allow_hw).ok());
            });
            self.h264_pending = Some(rx);
        }
    }

    /// Blocks until the background H.264 encoder is ready (or failed). For tests and benchmarks.
    pub fn wait_h264_ready(&mut self) -> bool {
        if let Some(rx) = self.h264_pending.take() {
            match rx.recv() {
                Ok(Some(e)) => {
                    self.h264_name = Some(e.name.clone());
                    self.h264 = Some(e);
                }
                _ => self.h264_failed = true,
            }
        }
        self.h264.is_some()
    }

    /// Picks up the encoder prepared by `set_h264` once it is ready.
    fn poll_h264(&mut self) {
        if let Some(rx) = &self.h264_pending {
            match rx.try_recv() {
                Ok(Some(e)) => {
                    self.h264_name = Some(e.name.clone());
                    self.h264 = Some(e);
                    self.h264_pending = None;
                }
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.h264_failed = true;
                    self.h264_pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    /// CBR target: ~2 bits per pixel per second (3.7 Mbps at 1728x1080), lowered under congestion.
    fn h264_bitrate(&self) -> u32 {
        let base = (self.width as f64 * self.height as f64 * 2.0).clamp(1_500_000.0, 10_000_000.0);
        (base * (1.0 - 0.7 * self.congestion as f64)) as u32
    }

    /// New capture geometry or quality (monitor / resolution switch): restart with a key frame.
    pub fn reconfigure(&mut self, width: i32, height: i32, base_quality: u32) {
        if width != self.width || height != self.height {
            self.width = width;
            self.height = height;
            self.diff = BlockDiff::new(width, height, BLOCK);
            self.lowq = vec![false; self.diff.grid_w * self.diff.grid_h];
            // The encoder is bound to the old size.
            self.h264 = None;
            self.h264_pending = None;
            self.in_h264 = false;
            self.set_h264(self.h264_mode);
        }
        self.base_quality = base_quality;
        self.request_key();
    }

    /// Next tick sends a full frame (viewer lost a delta, desktop switched, new session).
    pub fn request_key(&mut self) {
        self.needs_key = true;
        self.diff.reset();
    }

    /// 0 = link keeps up, 1 = fully congested. Lowers JPEG quality like AnyDesk's adaptive quality.
    pub fn set_congestion(&mut self, level: f32) {
        self.congestion = level.clamp(0.0, 1.0);
        self.congestion_drop = (self.congestion * 30.0) as u32;
    }

    fn quality(&self, heavy: bool) -> u32 {
        let q = self.base_quality.saturating_sub(self.congestion_drop);
        if heavy { q.saturating_sub(18).max(40) } else { q.max(40) }
    }

    /// Processes one captured frame. Returns the message to send, if any.
    pub fn encode(&mut self, f: &FrameInput) -> Option<Vec<u8>> {
        let t0 = Instant::now();
        self.last = FrameStats::default();
        if self.delta {
            for m in f.moves {
                self.diff.apply_move(
                    m.src_x as i32,
                    m.src_y as i32,
                    m.dst_x as i32,
                    m.dst_y as i32,
                    m.w as i32,
                    m.h as i32,
                );
            }
        }
        let force_key = self.needs_key;
        let dirty = self.diff.diff(f.pixels, f.stride, if force_key { None } else { f.dirty_hint });
        let total = dirty.len().max(1);
        let n_dirty = dirty.iter().filter(|&&d| d).count();
        let changed = n_dirty > 0 || (self.delta && !f.moves.is_empty());
        let heavy = n_dirty * 4 > total;
        self.last.dirty_blocks = n_dirty;

        let out = if !self.delta {
            self.encode_legacy(f, changed, heavy)
        } else if let Some(out) = self.encode_h264_path(f, changed, heavy, force_key) {
            out
        } else if force_key {
            self.needs_key = false;
            self.idle_ticks = 0;
            self.send_key(f, false)
        } else if changed {
            self.idle_ticks = 0;
            if n_dirty * 2 > total {
                // Mostly changed: one plain JPEG beats an atlas of almost everything.
                self.send_key(f, heavy)
            } else {
                let q = self.quality(heavy);
                let blurry = q < self.quality(false);
                for (lq, &d) in self.lowq.iter_mut().zip(&dirty) {
                    if d {
                        *lq = blurry;
                    }
                }
                self.send_delta(f, &dirty, q)
            }
        } else {
            self.idle_ticks += 1;
            let n_low = self.lowq.iter().filter(|&&d| d).count();
            if n_low > 0 && self.idle_ticks == SETTLE_TICKS {
                if n_low * 2 > total {
                    self.send_key(f, false)
                } else {
                    let blocks = std::mem::replace(&mut self.lowq, vec![false; total]);
                    let q = self.quality(false);
                    self.send_delta(f, &blocks, q)
                }
            } else if self.idle_ticks >= KEEPALIVE_TICKS {
                self.idle_ticks = 0;
                self.send_delta(f, &[], self.base_quality)
            } else {
                None
            }
        };
        self.last.encode_us = t0.elapsed().as_micros() as u64;
        self.last.bytes = out.as_ref().map_or(0, |b| b.len());
        let motion = heavy || (self.last.kind == SentKind::Delta && self.last.bytes > H264_MOTION_BYTES);
        self.motion_streak = if motion {
            self.motion_streak + 1
        } else if changed {
            self.motion_streak
        } else {
            0
        };
        out
    }

    /// H.264 state machine. Returns None when the JPEG paths should handle this tick.
    fn encode_h264_path(
        &mut self,
        f: &FrameInput,
        changed: bool,
        heavy: bool,
        force_key: bool,
    ) -> Option<Option<Vec<u8>>> {
        if self.h264_mode == H264Mode::Off || self.h264_failed {
            return None;
        }
        self.poll_h264();
        if !self.in_h264 {
            let enter = changed
                && match self.h264_mode {
                    H264Mode::Always => true,
                    _ => self.motion_streak >= H264_ENTER_STREAK,
                };
            // Until the encoder is ready, motion keeps going out as JPEG deltas.
            let enc = self.h264.as_mut().filter(|_| enter)?;
            enc.discard_pending();
            enc.force_keyframe();
            self.in_h264 = true;
            self.light_streak = 0;
            self.needs_key = false;
            self.idle_ticks = 0;
            return Some(self.send_h264(f));
        }
        if !changed {
            self.idle_ticks += 1;
            // Motion over: one sharp JPEG key frame replaces the H.264 picture, then regions as usual.
            if self.idle_ticks == SETTLE_TICKS {
                self.in_h264 = false;
                return Some(self.send_key(f, false));
            }
            return Some(None);
        }
        self.idle_ticks = 0;
        // Typing-sized changes (under ~3% of the blocks) are better served by sharp JPEG regions.
        let light = !heavy && self.last.dirty_blocks * 33 < self.lowq.len().max(1);
        self.light_streak = if light { self.light_streak + 1 } else { 0 };
        if self.h264_mode == H264Mode::Auto && self.light_streak >= H264_EXIT_LIGHT {
            self.in_h264 = false;
            return Some(self.send_key(f, false));
        }
        if force_key {
            self.needs_key = false;
            if let Some(e) = self.h264.as_mut() {
                e.force_keyframe();
            }
        }
        Some(self.send_h264(f))
    }

    fn send_h264(&mut self, f: &FrameInput) -> Option<Vec<u8>> {
        let target = self.h264_bitrate();
        let enc = self.h264.as_mut()?;
        if (enc.bitrate() as f64 - target as f64).abs() > target as f64 * 0.1 {
            enc.set_bitrate(target);
        }
        match enc.encode(f.pixels, f.stride) {
            Ok(Some((au, idr))) => {
                self.seq = self.seq.wrapping_add(1);
                self.last.kind = SentKind::H264;
                let header = FrameHeader {
                    kind: KIND_H264,
                    flags: if idr { FLAG_IDR } else { 0 },
                    seq: self.seq,
                    width: self.width as u16,
                    height: self.height as u16,
                    moves: Vec::new(),
                    patches: Vec::new(),
                };
                // Whatever the viewer shows now is H.264 quality; the settle key frame restores sharpness.
                self.lowq.fill(true);
                Some(header.encode(&au))
            }
            Ok(None) => None,
            Err(_) => {
                // Broken encoder (device lost, driver reset): stay on JPEG for the rest of the session.
                self.h264 = None;
                self.h264_failed = true;
                self.in_h264 = false;
                self.send_key(f, false)
            }
        }
    }

    /// The original pipeline: the whole frame as one JPEG whenever anything changed, a sharp refresh after
    /// motion settles, and a full keepalive every 30 idle ticks.
    fn encode_legacy(&mut self, f: &FrameInput, changed: bool, heavy: bool) -> Option<Vec<u8>> {
        let any_low = self.lowq.first().copied().unwrap_or(false);
        let settle = !changed && any_low && self.idle_ticks == SETTLE_TICKS;
        if !(changed || settle || self.idle_ticks >= KEEPALIVE_TICKS || self.needs_key) {
            self.idle_ticks += 1;
            return None;
        }
        self.needs_key = false;
        if changed || self.idle_ticks >= KEEPALIVE_TICKS {
            self.idle_ticks = 0;
        } else {
            self.idle_ticks += 1;
        }
        let q = self.quality(heavy);
        let blurry = q < self.quality(false);
        if let Some(first) = self.lowq.first_mut() {
            *first = blurry;
        }
        self.jpeg.set_quality(q);
        self.last.kind = SentKind::Legacy;
        self.last.quality = q;
        self.jpeg.encode_bgra(f.pixels, self.width, self.height, f.stride)
    }

    fn send_key(&mut self, f: &FrameInput, heavy: bool) -> Option<Vec<u8>> {
        let q = self.quality(heavy);
        self.jpeg.set_quality(q);
        let jpeg = self.jpeg.encode_bgra(f.pixels, self.width, self.height, f.stride)?;
        self.needs_key = false;
        let blurry = q < self.quality(false);
        self.lowq.fill(blurry);
        self.seq = self.seq.wrapping_add(1);
        self.last.kind = SentKind::Key;
        self.last.quality = q;
        Some(key_header(self.seq, self.width, self.height).encode(&jpeg))
    }

    fn send_delta(&mut self, f: &FrameInput, blocks: &[bool], q: u32) -> Option<Vec<u8>> {
        let rects = merge_blocks(blocks, self.diff.grid_w, BLOCK, self.width, self.height);
        let (aw, ah, patches) = pack_atlas(&rects, self.width);
        let payload = if patches.is_empty() {
            Vec::new()
        } else {
            let atlas = build_atlas(f.pixels, f.stride, &patches, aw, ah);
            self.jpeg.set_quality(q);
            match self.jpeg.encode_bgra(&atlas, aw, ah, aw as usize) {
                Some(j) => j,
                None => {
                    // The viewer would miss these regions; resend everything next tick instead.
                    self.request_key();
                    return None;
                }
            }
        };
        self.seq = self.seq.wrapping_add(1);
        self.last.kind = SentKind::Delta;
        self.last.quality = q;
        let header = FrameHeader {
            kind: KIND_DELTA,
            flags: 0,
            seq: self.seq,
            width: self.width as u16,
            height: self.height as u16,
            moves: if self.delta { f.moves.to_vec() } else { Vec::new() },
            patches,
        };
        Some(header.encode(&payload))
    }
}
