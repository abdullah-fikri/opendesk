use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_engine::capture::{cursor_shape, enumerate_monitors, DesktopCapturer, MonitorBounds};
use core_engine::codec::delta::{Applied, Compositor};
use core_engine::codec::frame::{is_framed, FrameHeader, FLAG_IDR, KIND_DELTA, KIND_H264, KIND_KEY};
use core_engine::codec::h264::H264Decoder;
use core_engine::codec::from_base64;
use core_engine::stream::{FrameInput, H264Mode, StreamEncoder};
use image::ImageReader;
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::logger;
use crate::protocol::OutgoingMsg;

/// Whether the current viewer advertised `cursor-shape`: it draws its own cursor, so the host stops painting it.
pub static PEER_CURSOR_SHAPE: AtomicBool = AtomicBool::new(false);
/// Capability for framed key/delta video (core_engine::codec::frame). Viewers without it get full JPEGs.
pub const CAP_VIDEO_DELTA: &str = "video-delta";
/// Whether the current viewer advertised `video-delta`.
pub static PEER_VIDEO_DELTA: AtomicBool = AtomicBool::new(false);
/// Capability for H.264 motion frames inside the `video-delta` framing; advertised only when this
/// machine can create the Media Foundation H.264 decoder.
pub const CAP_VIDEO_H264: &str = "video-h264";
/// Whether the current viewer advertised `video-h264`.
pub static PEER_VIDEO_H264: AtomicBool = AtomicBool::new(false);

/// Video capabilities this client advertises at register.
pub fn video_caps() -> Vec<&'static str> {
    static H264_OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let h264 = *H264_OK.get_or_init(|| std::env::var("OPENDESK_VIDEO_H264").map_or(true, |v| v != "0") && H264Decoder::new(64, 64).is_ok());
    if h264 { vec![CAP_VIDEO_DELTA, CAP_VIDEO_H264] } else { vec![CAP_VIDEO_DELTA] }
}

/// Set when the viewer asked for a key frame (`video-keyframe` input event) after losing a delta.
pub static KEYFRAME_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn decode_jpeg_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let img = reader.decode().ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some((img.into_raw(), width, height))
}

fn to_pixel_buffer(rgba: &[u8], width: u32, height: u32) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(rgba);
    buffer
}

pub fn decode_jpeg_bytes_to_pixel_buffer(bytes: &[u8]) -> Option<(SharedPixelBuffer<Rgba8Pixel>, u32, u32)> {
    let (rgba, width, height) = decode_jpeg_rgba(bytes)?;
    Some((to_pixel_buffer(&rgba, width, height), width, height))
}

pub fn decode_b64_to_pixel_buffer(b64: &str) -> Option<(SharedPixelBuffer<Rgba8Pixel>, u32, u32)> {
    let bytes = from_base64(b64)?;
    decode_jpeg_bytes_to_pixel_buffer(&bytes)
}

/// Received video messages in arrival order. Deltas must all be applied, so unlike the old single
/// latest-frame slot nothing is overwritten; a full frame makes everything queued before it obsolete.
#[derive(Default)]
pub struct VideoInbox {
    queue: Mutex<VecDeque<Vec<u8>>>,
}

impl VideoInbox {
    /// Bound on queued messages; beyond it the oldest are dropped and the sequence gap requests a key frame.
    const MAX: usize = 32;

    pub fn push(&self, msg: Vec<u8>) {
        let full = !is_framed(&msg) || FrameHeader::parse(&msg).is_some_and(|(h, _)| h.kind == KIND_KEY);
        let mut q = self.queue.lock().unwrap();
        if full {
            q.clear();
        }
        if q.len() >= Self::MAX {
            q.pop_front();
        }
        q.push_back(msg);
    }

    pub fn drain(&self) -> Vec<Vec<u8>> {
        self.queue.lock().unwrap().drain(..).collect()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.queue.lock().unwrap().is_empty()
    }
}

impl ViewerDecoder {
    fn apply_h264(&mut self, h: &FrameHeader, au: &[u8]) -> Applied {
        let idr = h.flags & FLAG_IDR != 0;
        let (w, hh) = (h.width as u32, h.height as u32);
        // P-frames need the previous picture of the same chain.
        if !idr && (!self.h264_ready || self.comp.last_seq().is_none_or(|s| s.wrapping_add(1) != h.seq)) {
            self.comp.invalidate();
            return Applied::NeedKey;
        }
        if idr && !self.h264.as_ref().is_some_and(|(_, dw, dh)| (*dw, *dh) == (w, hh)) {
            self.h264 = H264Decoder::new(w, hh).ok().map(|d| (d, w, hh));
        }
        let Some((dec, _, _)) = self.h264.as_mut() else {
            self.comp.invalidate();
            return Applied::NeedKey;
        };
        // Decode straight into the framebuffer; it is only kept if a picture came out.
        let mut fb = std::mem::take(&mut self.comp.fb);
        let result = dec.decode_into(au, w, hh, &mut fb);
        match result {
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

pub enum ViewerOutput {
    /// Show this frame.
    Frame(SharedPixelBuffer<Rgba8Pixel>, u32, u32),
    /// Nothing to redraw.
    Unchanged,
    /// Delta chain broken or undecodable; ask the host for a key frame.
    NeedKey,
}

/// Viewer side of the video stream: legacy full JPEGs and framed key/delta messages.
#[derive(Default)]
pub struct ViewerDecoder {
    comp: Compositor,
    h264: Option<(H264Decoder, u32, u32)>,
    /// The H.264 decoder has seen an IDR since the last non-H.264 picture, so P-frames may follow.
    h264_ready: bool,
}

impl ViewerDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a batch of messages in order and returns what to display afterwards.
    pub fn process(&mut self, batch: Vec<Vec<u8>>) -> ViewerOutput {
        let mut changed = false;
        let mut need_key = false;
        for msg in batch {
            match self.apply(&msg) {
                Applied::Updated => changed = true,
                Applied::Unchanged => {}
                Applied::NeedKey => need_key = true,
            }
        }
        if changed {
            ViewerOutput::Frame(to_pixel_buffer(&self.comp.fb, self.comp.width, self.comp.height), self.comp.width, self.comp.height)
        } else if need_key {
            ViewerOutput::NeedKey
        } else {
            ViewerOutput::Unchanged
        }
    }

    fn apply(&mut self, msg: &[u8]) -> Applied {
        if !is_framed(msg) {
            self.h264_ready = false;
            return match decode_jpeg_rgba(msg) {
                Some((rgba, w, h)) => {
                    self.comp.set_full(None, rgba, w, h);
                    Applied::Updated
                }
                None => Applied::Unchanged,
            };
        }
        let Some((header, payload)) = FrameHeader::parse(msg) else {
            self.comp.invalidate();
            return Applied::NeedKey;
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
}

/// Returns `(max_width, jpeg_quality)`. The width is chosen so the stream fits the mode's
/// box (e.g. 1280x720) on any aspect ratio, so "720p" never exceeds 720 rows on 16:10 or portrait monitors.
pub fn resolution_mode_to_params(mode: usize, bounds: MonitorBounds) -> (i32, u32) {
    let (box_w, box_h, quality) = match mode {
        0 => return (0, 85),     // 1:1 Native Resolution, Quality 85
        720 => (1280, 720, 78),  // 720p HD Smooth (COLORONCOLOR fast StretchBlt), Quality 78
        480 => (854, 480, 72),   // 480p Low Latency (COLORONCOLOR fast StretchBlt), Quality 72
        _ => (1920, 1080, 82),   // 1080p Full HD, Quality 82
    };
    let by_height = (bounds.width.max(1) as i64 * box_h / bounds.height.max(1) as i64) as i32;
    (box_w.min(by_height).max(2), quality)
}

/// Turns how long the capture loop waited for `can_send_frame` into a 0..1 congestion level:
/// waits under ~40 ms are normal pacing, ~250 ms or more means the link cannot keep up.
pub struct CongestionMeter {
    blocked_since: Option<Instant>,
    ewma_ms: f32,
}

impl CongestionMeter {
    pub fn new() -> Self {
        Self { blocked_since: None, ewma_ms: 0.0 }
    }

    /// Call every tick with the gate state; returns the current level.
    pub fn observe(&mut self, can_send: bool, now: Instant) -> f32 {
        match (can_send, self.blocked_since) {
            (false, None) => self.blocked_since = Some(now),
            (true, Some(t)) => {
                let waited = now.duration_since(t).as_secs_f32() * 1000.0;
                self.ewma_ms = self.ewma_ms * 0.8 + waited * 0.2;
                self.blocked_since = None;
            }
            (true, None) => self.ewma_ms *= 0.98, // idle link recovers slowly
            (false, Some(_)) => {}
        }
        ((self.ewma_ms - 40.0) / 210.0).clamp(0.0, 1.0)
    }
}

pub fn spawn_host_capture_loop(
    partner_clean: String,
    is_capturing: Arc<AtomicBool>,
    active_monitor_idx: Arc<AtomicUsize>,
    host_resolution_mode: Arc<AtomicUsize>,
    host_monitor_bounds: Arc<Mutex<MonitorBounds>>,
    can_send_frame: Arc<AtomicBool>,
    tx_out: Sender<OutgoingMsg>,
) {
    thread::spawn(move || {
        // Follow the secure desktop (UAC / lock screen) when running as SYSTEM; a no-op otherwise.
        core_engine::desktop::switch_to_input_desktop();
        let mut monitors = enumerate_monitors();
        let mut current_idx = active_monitor_idx.load(Ordering::SeqCst) % monitors.len().max(1);
        let initial_bounds = monitors[current_idx];
        *host_monitor_bounds.lock().unwrap() = initial_bounds;

        let mut current_res_mode = host_resolution_mode.load(Ordering::SeqCst);
        let (mut max_width, mut base_quality) = resolution_mode_to_params(current_res_mode, initial_bounds);

        let local_cursor = PEER_CURSOR_SHAPE.load(Ordering::SeqCst);
        let delta = PEER_VIDEO_DELTA.load(Ordering::SeqCst);
        // OPENDESK_VIDEO_H264=0 disables H.264, =always uses it for every changed frame.
        let h264_mode = match std::env::var("OPENDESK_VIDEO_H264").unwrap_or_default().as_str() {
            "0" => H264Mode::Off,
            "always" => H264Mode::Always,
            _ if PEER_VIDEO_H264.load(Ordering::SeqCst) => H264Mode::Auto,
            _ => H264Mode::Off,
        };
        KEYFRAME_REQUESTED.store(false, Ordering::SeqCst);
        let mut capturer = DesktopCapturer::new_for_monitor(initial_bounds, max_width);
        capturer.set_hide_viewer_cursor(local_cursor);
        let mut last_shape = u8::MAX;
        let mut encoder = StreamEncoder::new(capturer.width(), capturer.height(), base_quality, delta);
        encoder.set_h264(h264_mode);
        let mut h264_logged = false;
        let mut congestion = CongestionMeter::new();
        let target_interval = Duration::from_micros(16_666); // 60.0 FPS pacing

        let (ox, oy, sw, sh, w, h) = capturer.geometry();
        logger::info(
            "desk_rt.display_component",
            &format!(
                "Host capture started: monitor={} native={}x{}+({},{}) stream={}x{} mode={} quality={} fps=60 capture={} video={}",
                current_idx,
                sw,
                sh,
                ox,
                oy,
                w,
                h,
                current_res_mode,
                base_quality,
                capturer.backend(),
                if !delta { "jpeg" } else if h264_mode != H264Mode::Off { "delta+h264" } else { "delta" }
            ),
        );

        while is_capturing.load(Ordering::SeqCst) {
            let frame_start = Instant::now();

            // Check if Viewer requested monitor switch or resolution mode switch, or the input desktop changed
            let req_idx = active_monitor_idx.load(Ordering::Relaxed);
            let req_res_mode = host_resolution_mode.load(Ordering::Relaxed);
            let desktop_switched = core_engine::desktop::switch_to_input_desktop();
            if req_idx != current_idx || req_res_mode != current_res_mode || desktop_switched {
                monitors = enumerate_monitors();
                let count = monitors.len().max(1);
                current_idx = req_idx % count;
                active_monitor_idx.store(current_idx, Ordering::Relaxed);
                current_res_mode = req_res_mode;
                let bounds = monitors[current_idx];
                let (new_max_w, new_base_q) = resolution_mode_to_params(current_res_mode, bounds);
                max_width = new_max_w;
                base_quality = new_base_q;

                *host_monitor_bounds.lock().unwrap() = bounds;
                // Drop the old duplication first: an output allows only a few duplications at once.
                drop(capturer);
                capturer = DesktopCapturer::new_for_monitor(bounds, max_width);
                capturer.set_hide_viewer_cursor(local_cursor);
                encoder.reconfigure(capturer.width(), capturer.height(), base_quality);
                logger::info(
                    "desk_rt.display_component",
                    &format!(
                        "Capture reconfigured: monitor={}/{} stream={}x{} mode={} quality={} capture={} desktop_switch={}",
                        current_idx + 1,
                        count,
                        capturer.width(),
                        capturer.height(),
                        current_res_mode,
                        base_quality,
                        capturer.backend(),
                        desktop_switched
                    ),
                );
            }

            if local_cursor {
                let shape = cursor_shape();
                if shape != last_shape {
                    last_shape = shape;
                    let _ = tx_out.send(OutgoingMsg::Json(serde_json::json!({
                        "type": "relay-input",
                        "targetClean": partner_clean,
                        "event": { "action": "cursor-shape", "shape": shape }
                    })));
                }
            }
            if KEYFRAME_REQUESTED.swap(false, Ordering::SeqCst) {
                encoder.request_key();
            }

            // Backpressure gate: only capture & encode when the socket has finished flushing the previous frame
            let can_send = can_send_frame.load(Ordering::Acquire);
            encoder.set_congestion(congestion.observe(can_send, frame_start));
            if can_send {
                // DXGI blocks here until the desktop changes (or the tick ends), so an idle desktop costs ~nothing.
                let budget = target_interval.saturating_sub(frame_start.elapsed());
                let backend = capturer.backend();
                capturer.grab(budget.as_millis() as u32);
                if capturer.backend() != backend {
                    logger::warn("desk_rt.display_component", "DXGI duplication lost for good; falling back to GDI capture.");
                    encoder.request_key();
                }
                let frame = FrameInput {
                    pixels: capturer.pixels(),
                    stride: capturer.width() as usize,
                    dirty_hint: capturer.dirty_hint(),
                    moves: capturer.moves(),
                };
                if let Some(msg) = encoder.encode(&frame) {
                    if !h264_logged {
                        if let Some(name) = &encoder.h264_name {
                            h264_logged = true;
                            logger::info("desk_rt.display_component", &format!("H.264 motion encoder: {}", name));
                        }
                    }
                    can_send_frame.store(false, Ordering::Release);
                    let _ = tx_out.send(OutgoingMsg::BinaryFrame(msg));
                }
            }

            // Plain sleep: std uses a high-resolution waitable timer on Windows, so no busy-spin is needed.
            if let Some(remaining) = target_interval.checked_sub(frame_start.elapsed()) {
                thread::sleep(remaining);
            }
        }

        logger::info("desk_rt.display_component", "Host capture stopped.");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(width: i32, height: i32) -> MonitorBounds {
        MonitorBounds { left: 0, top: 0, width, height }
    }

    #[test]
    fn resolution_modes_fit_their_box_on_any_aspect_ratio() {
        assert_eq!(resolution_mode_to_params(720, mon(1920, 1080)).0, 1280);
        assert_eq!(resolution_mode_to_params(720, mon(1920, 1200)).0, 1152); // 1152x720
        assert_eq!(resolution_mode_to_params(720, mon(1080, 1920)).0, 405); // portrait: 405x720
        assert_eq!(resolution_mode_to_params(480, mon(3440, 1440)).0, 854); // ultrawide: 854x357
        assert_eq!(resolution_mode_to_params(0, mon(3840, 2160)).0, 0);
        assert_eq!(resolution_mode_to_params(999, mon(1920, 1080)), (1920, 82));
    }

    /// PSNR in dB between a BGRX source frame and the viewer's RGBA framebuffer.
    fn psnr(src: &[u32], rgba: &[u8]) -> f64 {
        let mut se = 0f64;
        for (i, &p) in src.iter().enumerate() {
            for (c, shift) in [16u32, 8, 0].iter().enumerate() {
                let d = ((p >> shift) & 0xFF) as f64 - rgba[i * 4 + c] as f64;
                se += d * d;
            }
        }
        let mse = se / (src.len() * 3) as f64;
        if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
    }

    fn shown(out: ViewerOutput) -> bool {
        matches!(out, ViewerOutput::Frame(..))
    }

    fn input(f: &[u32], stride: usize) -> FrameInput<'_> {
        FrameInput { pixels: f, stride, dirty_hint: None, moves: &[] }
    }

    /// Smooth gradient plus high-contrast "text" rows, roughly like a desktop.
    fn synthetic(w: usize, h: usize) -> Vec<u32> {
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if y % 24 < 12 && (x / 3 + y) % 5 == 0 { 0x0010_1010 } else { (((x * 255 / w) as u32) << 16) | (((y * 255 / h) as u32) << 8) | 0xC0 }
            })
            .collect()
    }

    #[test]
    fn delta_stream_reconstructs_frames_end_to_end() {
        let (w, h) = (320usize, 200usize);
        let mut frame = synthetic(w, h);
        let mut enc = StreamEncoder::new(w as i32, h as i32, 85, true);
        let mut viewer = ViewerDecoder::new();

        let key = enc.encode(&input(&frame, w)).unwrap();
        assert_eq!(FrameHeader::parse(&key).unwrap().0.kind, KIND_KEY);
        assert!(shown(viewer.process(vec![key])));
        assert!(enc.encode(&input(&frame, w)).is_none(), "static frame sends nothing");

        // Type a few "glyphs": each tick yields one small delta.
        for t in 0..5 {
            for y in 50..68 {
                for x in 40 + t * 11..50 + t * 11 {
                    frame[y * w + x] = 0;
                }
            }
            let msg = enc.encode(&input(&frame, w)).unwrap();
            assert_eq!(FrameHeader::parse(&msg).unwrap().0.kind, KIND_DELTA);
            assert!(msg.len() < 4000, "glyph delta is {} bytes", msg.len());
            assert!(shown(viewer.process(vec![msg])));
        }
        // Composited deltas must look as good as a fresh full JPEG of the same frame.
        let mut fresh = ViewerDecoder::new();
        let mut ref_enc = StreamEncoder::new(w as i32, h as i32, 85, true);
        let _ = fresh.process(vec![ref_enc.encode(&input(&frame, w)).unwrap()]);
        let (got, reference) = (psnr(&frame, &viewer.comp.fb), psnr(&frame, &fresh.comp.fb));
        assert!(got > 20.0 && got >= reference - 0.5, "delta psnr {got:.1} vs full-frame {reference:.1}");

        // A lost delta breaks the chain; the key-frame request restores it.
        frame[10 * w + 10] = 0x00FF_FFFF;
        let _lost = enc.encode(&input(&frame, w)).unwrap();
        frame[150 * w + 300] = 0x00FF_FFFF;
        let next = enc.encode(&input(&frame, w)).unwrap();
        assert!(matches!(viewer.process(vec![next]), ViewerOutput::NeedKey));
        enc.request_key();
        assert!(shown(viewer.process(vec![enc.encode(&input(&frame, w)).unwrap()])));
        assert!(psnr(&frame, &viewer.comp.fb) > 20.0);
    }

    #[test]
    fn h264_motion_then_sharp_key_end_to_end() {
        let (w, h) = (320usize, 192usize);
        let base = synthetic(w, h);
        let mut enc = StreamEncoder::new(w as i32, h as i32, 85, true);
        enc.set_h264(H264Mode::Auto);
        if !enc.wait_h264_ready() {
            return; // No Media Foundation H.264 on this machine: JPEG paths only.
        }
        let mut viewer = ViewerDecoder::new();
        let mut frame = base.clone();
        let mut kinds = Vec::new();
        // Full-screen horizontal motion for 20 ticks, then stillness.
        for t in 0..26 {
            if t < 20 {
                for y in 0..h {
                    for x in 0..w {
                        frame[y * w + x] = base[y * w + (x + t * 7) % w];
                    }
                }
            }
            if let Some(msg) = enc.encode(&input(&frame, w)) {
                let (hd, _) = FrameHeader::parse(&msg).unwrap();
                kinds.push(hd.kind);
                assert!(!matches!(viewer.process(vec![msg]), ViewerOutput::NeedKey), "chain broke at tick {t}");
            }
        }
        assert!(kinds.contains(&KIND_H264), "sustained motion switches to H.264: {kinds:?}");
        assert_eq!(*kinds.last().unwrap(), KIND_KEY, "motion ends with a sharp JPEG key frame");
        assert!(psnr(&frame, &viewer.comp.fb) > 20.0);
    }

    #[test]
    fn viewer_without_video_delta_gets_plain_jpegs() {
        let (w, h) = (160usize, 96usize);
        let mut frame = synthetic(w, h);
        let mut enc = StreamEncoder::new(w as i32, h as i32, 85, false);
        let first = enc.encode(&input(&frame, w)).unwrap();
        frame[5] = 0;
        let second = enc.encode(&input(&frame, w)).unwrap();
        for msg in [&first, &second] {
            assert_eq!(&msg[..3], &[0xFF, 0xD8, 0xFF], "standard JPEG for legacy viewers");
            assert!(!is_framed(msg));
            let (_, dw, dh) = decode_jpeg_bytes_to_pixel_buffer(msg).unwrap();
            assert_eq!((dw, dh), (w as u32, h as u32));
        }
    }

    #[test]
    fn inbox_keeps_deltas_and_drops_what_a_key_frame_replaces() {
        let inbox = VideoInbox::default();
        let delta = |seq| FrameHeader { kind: KIND_DELTA, flags: 0, seq, width: 4, height: 4, moves: vec![], patches: vec![] }.encode(&[]);
        inbox.push(delta(1));
        inbox.push(delta(2));
        assert_eq!(inbox.drain().len(), 2, "deltas are never overwritten");
        inbox.push(delta(3));
        inbox.push(core_engine::codec::delta::key_header(4, 4, 4).encode(&[]));
        inbox.push(delta(5));
        let got = inbox.drain();
        assert_eq!(got.len(), 2);
        assert_eq!(FrameHeader::parse(&got[0]).unwrap().0.kind, KIND_KEY);
    }

    #[test]
    fn congestion_follows_backpressure_wait() {
        let mut m = CongestionMeter::new();
        let t0 = Instant::now();
        assert_eq!(m.observe(true, t0), 0.0);
        let mut t = t0;
        for _ in 0..20 {
            m.observe(false, t);
            t += Duration::from_millis(400);
            m.observe(true, t);
        }
        assert!(m.observe(true, t) > 0.9);
    }

    // ---- Benchmark: `cargo test --release bench_video_pipeline -- --ignored --nocapture` ----

    #[derive(Clone, Copy, Debug)]
    enum Scenario {
        Static,
        TypingCursor,
        Scroll,
        WindowDrag,
    }

    fn fill_glyph(out: &mut [u32], w: usize, h: usize, x0: usize, y0: usize, bw: usize, bh: usize, seed: u32) {
        for y in y0..(y0 + bh).min(h) {
            for x in x0..(x0 + bw).min(w) {
                let v = ((x as u32).wrapping_mul(73856093) ^ (y as u32).wrapping_mul(19349663) ^ seed) % 3;
                out[y * w + x] = if v == 0 { 0x0020_2020 } else { 0x00F0_F0F0 };
            }
        }
    }

    /// Deterministic frame `t` of a scenario, built on top of a real desktop capture.
    fn scenario_frame(sc: Scenario, t: usize, base: &[u32], w: usize, h: usize, out: &mut Vec<u32>) {
        out.clear();
        out.extend_from_slice(base);
        match sc {
            Scenario::Static => {}
            Scenario::TypingCursor => {
                // ~10 chars/s typed on one line, a blinking caret and a mouse cursor moving 4 px per tick.
                let chars = (t / 6).min(120);
                for i in 0..chars {
                    fill_glyph(out, w, h, 120 + i * 11, h / 3, 10, 18, i as u32);
                }
                if (t / 30) % 2 == 0 {
                    fill_glyph(out, w, h, 120 + chars * 11, h / 3, 2, 18, 0);
                }
                let cx = (t * 4) % (w - 16);
                let cy = (h as f64 / 2.0 + (t as f64 / 20.0).sin() * 100.0) as usize;
                fill_glyph(out, w, h, cx, cy, 12, 19, 7);
            }
            Scenario::Scroll => {
                // A window (5/8 x 3/4 of the screen) scrolling its content 6 px per tick.
                let (x0, y0, ww, wh) = (w / 8, h / 10, w * 5 / 8, h * 3 / 4);
                for y in 0..wh {
                    let sy = (y + t * 6) % h;
                    out[(y0 + y) * w + x0..(y0 + y) * w + x0 + ww].copy_from_slice(&base[sy * w..sy * w + ww]);
                }
            }
            Scenario::WindowDrag => {
                // A window (1/3 x 3/8 of the screen) dragged 8 px right / 3 px down per tick.
                let (ww, wh) = (w / 3, h * 3 / 8);
                let x0 = (t * 8) % (w - ww);
                let y0 = (t * 3) % (h - wh);
                for y in 0..wh {
                    out[(y0 + y) * w + x0..(y0 + y) * w + x0 + ww].copy_from_slice(&base[(h / 4 + y) * w..(h / 4 + y) * w + ww]);
                }
            }
        }
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> *mut std::ffi::c_void;
        fn GetThreadTimes(h: *mut std::ffi::c_void, c: *mut u64, e: *mut u64, k: *mut u64, u: *mut u64) -> i32;
    }

    fn thread_cpu() -> Duration {
        let (mut c, mut e, mut k, mut u) = (0u64, 0u64, 0u64, 0u64);
        unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) };
        Duration::from_nanos((k + u) * 100)
    }

    /// The real capture loop (60 Hz pacing, capture + encode on one thread) on whatever the desktop shows.
    /// `cargo test --release bench_live_capture -- --ignored --nocapture`; BENCH_SECS (default 5).
    #[test]
    #[ignore]
    fn bench_live_capture() {
        let secs: u64 = std::env::var("BENCH_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
        let mode: usize = std::env::var("BENCH_MODE").ok().and_then(|v| v.parse().ok()).unwrap_or(1080);
        let bounds = enumerate_monitors()[0];
        let (max_w, quality) = resolution_mode_to_params(mode, bounds);
        println!("\nlive desktop, mode={} q={}, {} s each, 60 Hz pacing", mode, quality, secs);
        println!("{:<26} {:>9} {:>7} {:>13}", "pipeline", "KB/s", "frames", "CPU % (1 core)");
        for (gdi, delta, name) in [(true, false, "gdi + full jpeg (old)"), (false, false, "dxgi + full jpeg (legacy)"), (false, true, "dxgi + delta (new)")] {
            // SAFETY: single-threaded test setup; nothing else reads this variable concurrently.
            unsafe { std::env::set_var("OPENDESK_CAPTURE", if gdi { "gdi" } else { "auto" }) };
            let mut cap = DesktopCapturer::new_for_monitor(bounds, max_w);
            let mut enc = StreamEncoder::new(cap.width(), cap.height(), quality, delta);
            let tick = Duration::from_micros(16_666);
            let (mut bytes, mut frames) = (0usize, 0usize);
            let (start, cpu0) = (Instant::now(), thread_cpu());
            while start.elapsed() < Duration::from_secs(secs) {
                let t = Instant::now();
                cap.grab(tick.saturating_sub(t.elapsed()).as_millis() as u32);
                let f = FrameInput { pixels: cap.pixels(), stride: cap.width() as usize, dirty_hint: cap.dirty_hint(), moves: cap.moves() };
                if let Some(m) = enc.encode(&f) {
                    bytes += m.len();
                    frames += 1;
                }
                if let Some(rest) = tick.checked_sub(t.elapsed()) {
                    thread::sleep(rest);
                }
            }
            let cpu = (thread_cpu() - cpu0).as_secs_f64() / start.elapsed().as_secs_f64() * 100.0;
            println!("{:<26} {:>9.1} {:>7} {:>13.1}", format!("{name} [{}]", cap.backend()), bytes as f64 / 1024.0 / secs as f64, frames, cpu);
        }
        unsafe { std::env::remove_var("OPENDESK_CAPTURE") };
    }

    #[test]
    #[ignore]
    fn bench_video_pipeline() {
        let secs: usize = std::env::var("BENCH_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
        let mode: usize = std::env::var("BENCH_MODE").ok().and_then(|v| v.parse().ok()).unwrap_or(1080);
        let bounds = enumerate_monitors()[0];
        let (max_w, quality) = resolution_mode_to_params(mode, bounds);
        let mut gdi = core_engine::capture::GdiScreenCapturer::new_for_monitor(bounds, max_w);
        let (w, h) = (gdi.width as usize, gdi.height as usize);
        let t = Instant::now();
        for _ in 0..60 {
            core_engine::capture::ScreenCapturer::capture_frame(&mut gdi);
        }
        let gdi_ms = t.elapsed().as_secs_f64() * 1000.0 / 60.0;
        let base = gdi.pixels().to_vec();
        drop(gdi);

        // DXGI: first grab copies (and scales) the whole desktop; later grabs only touch changed regions,
        // and with nothing changing a zero-timeout grab returns at once.
        let mut dx = DesktopCapturer::new_for_monitor(bounds, max_w);
        let t = Instant::now();
        dx.grab(500);
        let dx_first = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        for _ in 0..60 {
            dx.grab(0);
        }
        let dx_idle = t.elapsed().as_secs_f64() * 1000.0 / 60.0;
        println!(
            "
screen {}x{} mode={} q={} | GDI capture {:.2} ms/frame | {} first frame {:.2} ms, idle grab {:.3} ms | {} s of 60 Hz ticks, unconstrained link",
            w, h, mode, quality, gdi_ms, dx.backend(), dx_first, dx_idle, secs
        );
        assert_eq!((dx.width() as usize, dx.height() as usize), (w, h), "DXGI and GDI agree on stream size");
        drop(dx);
        println!("{:<13} {:<6} {:>9} {:>7} {:>10} {:>9} {:>10} {:>10} {:>6}", "scenario", "mode", "KB/s", "frames", "enc ms/fr", "enc max", "host ms/s", "dec ms/fr", "PSNR");

        let ticks = secs * 60;
        let mut frame = Vec::new();
        for sc in [Scenario::Static, Scenario::TypingCursor, Scenario::Scroll, Scenario::WindowDrag] {
            for (delta, h264, label) in [(false, H264Mode::Off, "jpeg"), (true, H264Mode::Off, "delta"), (true, H264Mode::Auto, "d+h264"), (true, H264Mode::Always, "h264")] {
                let mut enc = StreamEncoder::new(w as i32, h as i32, quality, delta);
                enc.set_h264(h264);
                enc.wait_h264_ready();
                let mut viewer = ViewerDecoder::new();
                let (mut bytes, mut sent, mut enc_sent_us, mut enc_max, mut host_us, mut dec_us) = (0usize, 0usize, 0u64, 0u64, 0u64, 0u64);
                for tick in 0..ticks {
                    scenario_frame(sc, tick, &base, w, h, &mut frame);
                    let msg = enc.encode(&input(&frame, w));
                    host_us += enc.last.encode_us;
                    if let Some(msg) = msg {
                        bytes += msg.len();
                        sent += 1;
                        enc_sent_us += enc.last.encode_us;
                        enc_max = enc_max.max(enc.last.encode_us);
                        let td = Instant::now();
                        let _ = viewer.process(vec![msg]);
                        dec_us += td.elapsed().as_micros() as u64;
                    }
                }
                println!(
                    "{:<13} {:<6} {:>9.1} {:>7} {:>10.2} {:>9.2} {:>10.1} {:>10.2} {:>6.1}",
                    format!("{:?}", sc),
                    label,
                    bytes as f64 / 1024.0 / secs as f64,
                    sent,
                    enc_sent_us as f64 / 1000.0 / sent.max(1) as f64,
                    enc_max as f64 / 1000.0,
                    host_us as f64 / 1000.0 / secs as f64,
                    dec_us as f64 / 1000.0 / sent.max(1) as f64,
                    psnr(&frame, &viewer.comp.fb)
                );
                if let Some(name) = &enc.h264_name {
                    if sc as usize == 3 && h264 == H264Mode::Always {
                        println!("H.264 encoder: {name}");
                    }
                }
            }
        }
    }
}
