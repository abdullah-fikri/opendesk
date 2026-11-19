//! Viewer-side video: the per-session inbox, the decoder for every stream format a host may send (plain
//! JPEG, framed key/delta, H.264 inside the framing), and the per-session decoder thread.

use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use core_engine::codec::delta::{Applied, Compositor};
use core_engine::codec::frame::{
    FLAG_IDR, FLAG_LOSSLESS, FrameHeader, KIND_DELTA, KIND_H264, KIND_KEY, is_framed, read_stamp, split_mixed,
};
use core_engine::codec::h264::H264Decoder;
use core_engine::codec::lossless;
use image::ImageReader;
use parking_lot::{Condvar, Mutex};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::domain::capability;
use crate::domain::latency;
use crate::domain::link_quality::EventRate;
use crate::domain::perf_stats::{Counter, PerfWindow, STATS_INTERVAL, Stage, kbps};
use crate::infra::logger;
use crate::infra::process_usage::UsageSampler;
use crate::protocol::{ClientMessage, InputEvent, Outgoing};
use crate::service::ui_port::Frame;
use crate::service::viewer::registry::Session;
use crate::service::viewer::stats::{FrameLatency, ViewerStats, path_label};

mod inbox;
#[cfg(test)]
mod tests;
mod worker;

pub use inbox::*;
pub use worker::*;

/// Minimum spacing of key-frame requests after a broken delta chain.
const KEY_REQUEST_INTERVAL: Duration = Duration::from_millis(300);

/// Video capabilities this client advertises at registration. H.264 is advertised only when this machine can
/// create the Media Foundation decoder (`OPENDESK_VIDEO_H264=0` turns it off). `frame-stamp` is always listed:
/// this client parses stamps as a viewer and, as a host, sends them only to viewers that list it.
pub fn supported_video_caps() -> Vec<&'static str> {
    static H264_OK: OnceLock<bool> = OnceLock::new();
    let h264 = *H264_OK.get_or_init(|| {
        std::env::var("OPENDESK_VIDEO_H264").map_or(true, |v| v != "0") && H264Decoder::new(64, 64).is_ok()
    });
    let mut caps = vec![capability::VIDEO_DELTA, capability::FRAME_STAMP];
    if h264 {
        caps.push(capability::VIDEO_H264);
    }
    caps
}

pub(crate) fn decode_jpeg_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let img = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?.decode().ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some((img.into_raw(), width, height))
}

pub(crate) fn to_pixel_buffer(rgba: &[u8], width: u32, height: u32) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(rgba);
    buffer
}

/// What a decoded batch means for the screen.
pub enum ViewerOutput {
    /// Show this frame; the rows that differ from the previous one (None: all of them).
    Frame(SharedPixelBuffer<Rgba8Pixel>, u32, u32, Rows),
    /// Nothing to redraw.
    Unchanged,
    /// Delta chain broken or undecodable; ask the host for a key frame.
    NeedKey,
}

/// Viewer side of the video stream: plain JPEGs and framed key/delta/H.264 messages.
#[derive(Default)]
pub struct ViewerDecoder {
    comp: Compositor,
    h264: Option<(H264Decoder, u32, u32)>,
    /// The H.264 decoder has seen an IDR since the last non-H.264 picture, so P-frames may follow.
    h264_ready: bool,
    /// Output buffers, reused round-robin (see [`ViewerDecoder::output`]).
    pool: Vec<SharedPixelBuffer<Rgba8Pixel>>,
    next: usize,
    /// Framebuffer rows changed since the last output.
    dirty: Rows,
    /// Rows changed by the message being applied.
    last_rows: Rows,
    /// Per pool buffer: framebuffer rows it is missing (changed since it was last filled).
    stale: [Rows; ViewerDecoder::POOL],
}

/// A band of framebuffer rows `start..end`, or None for no rows.
type Rows = Option<(usize, usize)>;

/// The smallest band holding both.
fn union(a: Rows, b: Rows) -> Rows {
    match (a, b) {
        (Some((a0, a1)), Some((b0, b1))) => Some((a0.min(b0), a1.max(b1))),
        (a, None) => a,
        (None, b) => b,
    }
}

/// Rows a delta writes (moved and patched destinations), clipped to the frame.
fn delta_rows(h: &FrameHeader) -> Rows {
    let height = h.height as usize;
    let band = |y: u16, rows: u16| {
        let (y, end) = (y as usize, (y as usize + rows as usize).min(height));
        (y < end).then_some((y, end))
    };
    let moves = h.moves.iter().map(|m| band(m.dst_y, m.h));
    let patches = h.patches.iter().map(|p| band(p.dst_y, p.h));
    moves.chain(patches).fold(None, union)
}

impl ViewerDecoder {
    /// A decoder that has not seen a frame yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a batch of messages in order and returns what to display afterwards.
    pub fn process(&mut self, batch: Vec<Vec<u8>>) -> ViewerOutput {
        let mut changed = false;
        let mut need_key = false;
        for msg in batch {
            let (applied, rows) = self.apply(&msg);
            match applied {
                Applied::Updated => {
                    changed = true;
                    self.dirty = union(self.dirty, rows);
                }
                Applied::Unchanged => {}
                Applied::NeedKey => need_key = true,
            }
        }
        if changed {
            let (pixels, rows) = self.output();
            ViewerOutput::Frame(pixels, self.comp.width, self.comp.height, rows)
        } else if need_key {
            ViewerOutput::NeedKey
        } else {
            ViewerOutput::Unchanged
        }
    }

    /// Bound on reused output buffers: one on screen, one waiting for the UI thread, one being filled.
    const POOL: usize = 3;

    /// Copies the framebuffer into an output buffer. Buffers are reused round-robin, and a reused buffer gets
    /// only the rows that changed since it was last filled (typing touches a few rows, not the whole 8 MB of a
    /// 1080p frame). One the UI still holds is shared, so `make_mut_bytes` copies it first and the picture on
    /// screen is never written to.
    /// Also returns the rows that differ from the previous output; None (all rows) for the first outputs of a
    /// size, when nothing on screen can be built on.
    fn output(&mut self) -> (SharedPixelBuffer<Rgba8Pixel>, Rows) {
        let (w, h) = (self.comp.width, self.comp.height);
        let dirty = self.dirty.take();
        if self.pool.first().is_some_and(|b| (b.width(), b.height()) != (w, h)) {
            self.pool.clear();
        }
        let fresh = self.pool.is_empty();
        if self.pool.len() < Self::POOL {
            for stale in &mut self.stale[..self.pool.len()] {
                *stale = union(*stale, dirty);
            }
            self.pool.push(to_pixel_buffer(&self.comp.fb, w, h));
            self.stale[self.pool.len() - 1] = None;
            self.next = 0;
            return (self.pool.last().unwrap().clone(), if fresh { None } else { dirty });
        }
        for stale in &mut self.stale {
            *stale = union(*stale, dirty);
        }
        let i = self.next;
        self.next = (self.next + 1) % Self::POOL;
        let row = w as usize * 4;
        if let Some((y0, y1)) = self.stale[i].take() {
            let (a, b) = (y0 * row, (y1 * row).min(self.comp.fb.len()));
            self.pool[i].make_mut_bytes()[a..b].copy_from_slice(&self.comp.fb[a..b]);
        }
        (self.pool[i].clone(), dirty)
    }

    /// Applies one message; on `Updated` also the rows it changed.
    fn apply(&mut self, msg: &[u8]) -> (Applied, Rows) {
        let applied = self.apply_inner(msg);
        let rows = match applied {
            Applied::Updated => self.last_rows.take(),
            _ => None,
        };
        (applied, rows)
    }

    fn apply_inner(&mut self, msg: &[u8]) -> Applied {
        // Every picture that is not a delta replaces the whole frame.
        self.last_rows = Some((0, self.comp.height as usize));
        if !is_framed(msg) {
            self.h264_ready = false;
            return match decode_jpeg_rgba(msg) {
                Some((rgba, w, h)) => {
                    self.comp.set_full(None, rgba, w, h);
                    self.last_rows = Some((0, h as usize));
                    Applied::Updated
                }
                None => Applied::Unchanged,
            };
        }
        let Some((header, payload)) = FrameHeader::parse(msg) else {
            self.comp.invalidate();
            return Applied::NeedKey;
        };
        self.last_rows = match header.kind {
            KIND_DELTA => delta_rows(&header),
            _ => Some((0, header.height as usize)),
        };
        if header.kind != KIND_H264 {
            self.h264_ready = false;
        }
        match header.kind {
            KIND_KEY => match decode_jpeg_rgba(payload) {
                Some((rgba, w, h)) => {
                    self.comp.set_full(Some(header.seq), rgba, w, h);
                    Applied::Updated
                }
                None => {
                    self.comp.invalidate();
                    Applied::NeedKey
                }
            },
            KIND_DELTA if header.flags & FLAG_LOSSLESS != 0 => self.apply_lossless_delta(&header, payload),
            KIND_DELTA => {
                if payload.is_empty() || header.patches.is_empty() {
                    return self.comp.apply_delta(&header, None);
                }
                match decode_jpeg_rgba(payload) {
                    Some((rgba, w, h)) => self.comp.apply_delta(&header, Some((&rgba, w, h))),
                    None => {
                        self.comp.invalidate();
                        Applied::NeedKey
                    }
                }
            }
            KIND_H264 => self.apply_h264(&header, payload),
            // Unknown kinds are never sent to us; resync if they are.
            _ => Applied::NeedKey,
        }
    }

    /// A delta with a JPEG atlas and a lossless text/UI atlas (`tile-lossless`).
    fn apply_lossless_delta(&mut self, header: &FrameHeader, payload: &[u8]) -> Applied {
        let decoded = split_mixed(payload).and_then(|(jpeg, tiles)| {
            let jpeg = if jpeg.is_empty() { None } else { Some(decode_jpeg_rgba(jpeg)?) };
            Some((jpeg, lossless::decode(tiles)?))
        });
        let Some((jpeg, (tiles, tw, th))) = decoded else {
            self.comp.invalidate();
            return Applied::NeedKey;
        };
        let jpeg = jpeg.as_ref().map(|(rgba, w, h)| (rgba.as_slice(), *w, *h));
        self.comp.apply_mixed(header, jpeg, Some((&tiles, tw, th)))
    }

    fn apply_h264(&mut self, h: &FrameHeader, au: &[u8]) -> Applied {
        let idr = h.flags & FLAG_IDR != 0;
        let (w, hh) = (h.width as u32, h.height as u32);
        // P-frames need the previous picture of the same chain.
        if !idr && (!self.h264_ready || self.comp.last_seq().is_none_or(|s| s.wrapping_add(1) != h.seq)) {
            self.comp.invalidate();
            return Applied::NeedKey;
        }
        if idr && self.h264.as_ref().is_none_or(|(_, dw, dh)| (*dw, *dh) != (w, hh)) {
            self.h264 = H264Decoder::new(w, hh).ok().map(|d| (d, w, hh));
            if let Some((d, ..)) = &self.h264 {
                let kind = if d.is_hardware() { "GPU (DXVA)" } else { "software" };
                logger::info("viewer", &format!("H.264 decoder for {w}x{hh}: {kind}."));
            }
        }
        let Some((dec, _, _)) = self.h264.as_mut() else {
            self.comp.invalidate();
            return Applied::NeedKey;
        };
        // Decode straight into the framebuffer; it is only kept if a picture came out.
        let mut fb = std::mem::take(&mut self.comp.fb);
        match dec.decode_into(au, w, hh, &mut fb) {
            Ok(true) => {
                self.comp.set_full(Some(h.seq), fb, w, hh);
                self.h264_ready = true;
                Applied::Updated
            }
            Ok(false) => {
                self.comp.fb = fb;
                self.comp.advance(h.seq);
                self.h264_ready |= idr;
                Applied::Unchanged
            }
            Err(_) => {
                self.h264 = None;
                self.h264_ready = false;
                self.comp.invalidate();
                Applied::NeedKey
            }
        }
    }
}
