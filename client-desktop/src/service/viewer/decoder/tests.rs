use super::*;
use crate::service::viewer::registry::{Open, Registry};
use core_engine::capture::{DesktopCapturer, enumerate_monitors};
use core_engine::stream::{FrameInput, H264Mode, StreamEncoder};
use std::sync::atomic::AtomicU32;
use std::thread;

#[test]
fn hidden_session_skips_decoding_and_resyncs_with_a_key_frame_when_shown() {
    let r = Registry::new();
    r.set_multi(true);
    let Open::Opened(_a) = r.open("111111111", 1, 0, 2) else { panic!() };
    let Open::Opened(b) = r.open("222222222", 1, 0, 2) else { panic!() };
    r.set_focus("111111111");
    let (tx, rx) = std::sync::mpsc::channel();
    let shown = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&shown);
    let show = move |_: &Arc<Session>, _: Frame| {
        counter.fetch_add(1, Ordering::SeqCst);
    };
    spawn(Arc::clone(&b), tx, show, |_, _| {}, || {});
    b.inbox.push(core_engine::codec::delta::key_header(4, 4, 4).encode(&[]));
    thread::sleep(Duration::from_millis(50));
    assert!(b.inbox.is_empty(), "hidden tab drains its inbox");
    assert_eq!(shown.load(Ordering::SeqCst), 0);
    assert!(rx.try_recv().is_err());
    r.set_focus("222222222");
    let msg = rx.recv_timeout(Duration::from_secs(2)).expect("key frame request");
    let Outgoing::Message(ClientMessage::RelayInput { target_clean, event }) = msg else { panic!() };
    assert_eq!((target_clean.as_str(), event), ("222222222", InputEvent::VideoKeyframe));
    r.remove("222222222");
}

#[test]
fn perf_line_reports_the_window() {
    let mut p = ViewerPerf::default();
    for _ in 0..590 {
        p.process.record(Duration::from_micros(2400));
        p.handoff.record(Duration::from_micros(100));
    }
    p.shown += 590;
    p.bytes += 3_062_500;
    p.batches += 600;
    assert_eq!(
        p.take_line("123456789", 10.0),
        "session=123456789 fps=59.0 process avg/max=2.4/2.4ms handoff=0.1/0.1ms kbps=2450 batches=600"
    );
    assert_eq!(p.batches.in_window(), 0);
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
            if y % 24 < 12 && (x / 3 + y) % 5 == 0 {
                0x0010_1010
            } else {
                (((x * 255 / w) as u32) << 16) | (((y * 255 / h) as u32) << 8) | 0xC0
            }
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

/// `tile-lossless`: text typed on a plain background arrives pixel-exact, photo-like blocks stay JPEG, and
/// both kinds can share one delta.
#[test]
fn lossless_tiles_reproduce_text_exactly_end_to_end() {
    let (w, h) = (320usize, 200usize);
    // Plain window background with a gradient "photo" in the right half.
    let mut frame: Vec<u32> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            if x >= 192 {
                (((x * 7 + y * 3) as u32 & 0xFF) << 16) | (((y as u32 * 5) & 0xFF) << 8) | 0x40
            } else {
                0x00F3_F3F3
            }
        })
        .collect();
    let mut enc = StreamEncoder::new(w as i32, h as i32, 85, true);
    enc.set_lossless(true);
    let mut viewer = ViewerDecoder::new();
    assert!(shown(viewer.process(vec![enc.encode(&input(&frame, w)).unwrap()])));

    let text = |x: usize, y: usize| (x * 7 + y * 13) % 5 < 2; // dense two-tone "glyphs"
    for t in 0..4 {
        for y in 40..56 {
            for x in 10 + t * 30..34 + t * 30 {
                if text(x, y) {
                    frame[y * w + x] = 0x0020_2020;
                }
            }
        }
        // The last tick also touches the photo half: one delta with both atlases.
        if t == 3 {
            for y in 100..140 {
                for x in 200..260 {
                    frame[y * w + x] ^= 0x0033_1100;
                }
            }
        }
        let msg = enc.encode(&input(&frame, w)).unwrap();
        let (hdr, _) = FrameHeader::parse(&msg).unwrap();
        assert_eq!((hdr.kind, hdr.flags & FLAG_LOSSLESS), (KIND_DELTA, FLAG_LOSSLESS));
        assert!(shown(viewer.process(vec![msg])));
    }
    // Still screen: the refine pass resends the rest of the plain background (key-frame JPEG) pixel-exact.
    let mut refined = false;
    for _ in 0..10 {
        if let Some(msg) = enc.encode(&input(&frame, w)) {
            assert_eq!(FrameHeader::parse(&msg).unwrap().0.flags & FLAG_LOSSLESS, FLAG_LOSSLESS);
            assert!(shown(viewer.process(vec![msg])));
            refined = true;
            break;
        }
    }
    assert!(refined, "no refine pass");
    // Every pixel left of the photo is exact.
    for y in 0..h {
        for x in 0..192 {
            let (p, o) = (frame[y * w + x], (y * w + x) * 4);
            let got = [viewer.comp.fb[o], viewer.comp.fb[o + 1], viewer.comp.fb[o + 2]];
            assert_eq!(got, [(p >> 16) as u8, (p >> 8) as u8, p as u8], "pixel ({x}, {y})");
        }
    }
    assert!(psnr(&frame, &viewer.comp.fb) > 20.0);

    // A viewer without the capability never gets lossless tiles.
    let mut plain = StreamEncoder::new(w as i32, h as i32, 85, true);
    let _key = plain.encode(&input(&frame, w)).unwrap();
    frame[45 * w + 12] = 0;
    let msg = plain.encode(&input(&frame, w)).unwrap();
    assert_eq!(FrameHeader::parse(&msg).unwrap().0.flags & FLAG_LOSSLESS, 0);
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
        let (_, dw, dh) = decode_jpeg_rgba(msg).unwrap();
        assert_eq!((dw, dh), (w as u32, h as u32));
    }
}

#[test]
fn inbox_keeps_deltas_and_drops_what_a_key_frame_replaces() {
    let inbox = VideoInbox::default();
    let delta = |seq| {
        FrameHeader { kind: KIND_DELTA, flags: 0, seq, width: 4, height: 4, moves: vec![], patches: vec![] }.encode(&[])
    };
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
fn reused_output_buffers_never_change_a_frame_already_handed_out() {
    let jpeg = |w: u32, h: u32, v: u8| {
        let mut out = Vec::new();
        image::RgbImage::from_pixel(w, h, image::Rgb([v, v, v]))
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
            .unwrap();
        out
    };
    let mut viewer = ViewerDecoder::new();
    let mut held = Vec::new();
    for (i, v) in [0u8, 255, 0, 255, 0, 255, 0].into_iter().enumerate() {
        let ViewerOutput::Frame(buf, ..) = viewer.process(vec![jpeg(8, 8, v)]) else { panic!() };
        assert_eq!(buf.as_bytes(), &viewer.comp.fb[..]);
        if i % 2 == 0 {
            held.push((buf, viewer.comp.fb.clone())); // the UI still shows every other frame
        }
    }
    for (buf, fb) in &held {
        assert_eq!(buf.as_bytes(), &fb[..], "a frame the UI holds is never rewritten");
    }
    assert_eq!(viewer.pool.len(), ViewerDecoder::POOL);
    let ViewerOutput::Frame(buf, w, h, rows) = viewer.process(vec![jpeg(16, 4, 128)]) else { panic!() };
    assert_eq!((buf.width(), buf.height(), w, h), (16, 4, 16, 4), "a new stream size resets the pool");
    assert_eq!(rows, None, "the first frame of a size is whole");
    assert_eq!(viewer.pool.len(), 1);

    // Buffers the UI has released are refilled in place, not reallocated.
    let ptrs = |v: &ViewerDecoder| v.pool.iter().map(|b| b.as_bytes().as_ptr()).collect::<Vec<_>>();
    for _ in 0..ViewerDecoder::POOL {
        let _ = viewer.process(vec![jpeg(16, 4, 7)]);
    }
    let before = ptrs(&viewer);
    for _ in 0..ViewerDecoder::POOL {
        let _ = viewer.process(vec![jpeg(16, 4, 9)]);
    }
    assert_eq!(ptrs(&viewer), before);
}

#[test]
fn reused_buffers_copy_only_changed_rows_yet_always_match_the_framebuffer() {
    let (w, h) = (8u32, 16u32);
    let mut viewer = ViewerDecoder::new();
    viewer.comp.set_full(None, vec![0; (w * h * 4) as usize], w, h);
    viewer.dirty = Some((0, h as usize));
    let mut held = Vec::new();
    let mut seed = 7u32;
    // What a texture fed only the reported rows shows (V2): always the framebuffer.
    let mut screen = vec![0u8; (w * h * 4) as usize];
    for step in 0..200u32 {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let y0 = (seed >> 8) as usize % h as usize;
        let y1 = (y0 + 1 + (seed >> 16) as usize % 4).min(h as usize);
        let row = w as usize * 4;
        viewer.comp.fb[y0 * row..y1 * row].fill(step as u8);
        viewer.dirty = union(viewer.dirty, Some((y0, y1)));
        let (buf, rows) = viewer.output();
        assert_eq!(buf.as_bytes(), &viewer.comp.fb[..], "step {step}");
        let (a, b) = rows.unwrap_or((0, h as usize));
        screen[a * row..b * row].copy_from_slice(&buf.as_bytes()[a * row..b * row]);
        assert_eq!(screen, viewer.comp.fb, "step {step}: rows {rows:?} miss a change");
        if step % 3 == 0 {
            held.push((buf, viewer.comp.fb.clone()));
            if held.len() > 2 {
                held.remove(0);
            }
        }
    }
    for (buf, fb) in &held {
        assert_eq!(buf.as_bytes(), &fb[..], "a frame the UI holds is never rewritten");
    }
}

#[test]
fn delta_rows_cover_moves_and_patches_clipped_to_the_frame() {
    use core_engine::codec::frame::{MoveRect, Patch};
    let mut h = core_engine::codec::delta::key_header(1, 8, 100);
    assert_eq!(delta_rows(&h), None);
    h.moves.push(MoveRect { src_x: 0, src_y: 50, dst_x: 0, dst_y: 40, w: 8, h: 10 });
    h.patches.push(Patch { dst_x: 0, dst_y: 90, w: 8, h: 32, atlas_x: 0, atlas_y: 0 });
    assert_eq!(delta_rows(&h), Some((40, 100)));
}

#[test]
fn inbox_push_wakes_a_waiting_drain() {
    let inbox = Arc::new(VideoInbox::default());
    let pusher = Arc::clone(&inbox);
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        pusher.push(core_engine::codec::delta::key_header(4, 4, 4).encode(&[]));
    });
    let start = Instant::now();
    let got = inbox.drain_wait(Duration::from_secs(5));
    assert_eq!(got.msgs.len(), 1);
    assert!(got.received.is_some_and(|t| t >= start), "arrival time of the newest message");
    assert!(start.elapsed() < Duration::from_secs(2), "woken by the push, not the timeout");
    t.join().unwrap();
}

#[test]
fn inbox_drain_wait_times_out_empty_and_wake_returns_early() {
    let inbox = Arc::new(VideoInbox::default());
    let start = Instant::now();
    assert!(inbox.drain_wait(Duration::from_millis(30)).msgs.is_empty());
    assert!(start.elapsed() >= Duration::from_millis(25));

    let waker = Arc::clone(&inbox);
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        waker.wake();
    });
    let start = Instant::now();
    assert!(inbox.drain_wait(Duration::from_secs(5)).msgs.is_empty());
    assert!(start.elapsed() < Duration::from_secs(2), "woken before the timeout");
    t.join().unwrap();
}

// ---- Benchmark: `cargo test --release bench_video_pipeline -- --ignored --nocapture` ----

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Static,
    TypingCursor,
    Scroll,
    WindowDrag,
}

#[allow(clippy::too_many_arguments)]
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
            if (t / 30).is_multiple_of(2) {
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
                out[(y0 + y) * w + x0..(y0 + y) * w + x0 + ww]
                    .copy_from_slice(&base[(h / 4 + y) * w..(h / 4 + y) * w + ww]);
            }
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThread() -> *mut std::ffi::c_void;
    fn GetThreadTimes(h: *mut std::ffi::c_void, c: *mut u64, e: *mut u64, k: *mut u64, u: *mut u64) -> i32;
}

#[cfg(windows)]
fn thread_cpu() -> Duration {
    let (mut c, mut e, mut k, mut u) = (0u64, 0u64, 0u64, 0u64);
    unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) };
    Duration::from_nanos((k + u) * 100)
}

#[cfg(not(windows))]
fn thread_cpu() -> Duration {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// The real capture loop (60 Hz pacing, capture + encode on one thread) on whatever the desktop shows.
/// `cargo test --release bench_live_capture -- --ignored --nocapture`; BENCH_SECS (default 5).
#[test]
#[ignore]
fn bench_live_capture() {
    let secs: u64 = std::env::var("BENCH_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    let mode: usize = std::env::var("BENCH_MODE").ok().and_then(|v| v.parse().ok()).unwrap_or(1080);
    let bounds = enumerate_monitors()[0];
    let (max_w, quality) = crate::domain::resolution::capture_params(mode, bounds.width, bounds.height);
    println!("\nlive desktop, mode={} q={}, {} s each, 60 Hz pacing", mode, quality, secs);
    println!("{:<26} {:>9} {:>7} {:>13}", "pipeline", "KB/s", "frames", "CPU % (1 core)");
    for (gdi, delta, name) in [
        (true, false, "gdi + full jpeg (old)"),
        (false, false, "dxgi + full jpeg (legacy)"),
        (false, true, "dxgi + delta (new)"),
    ] {
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
            let f = FrameInput {
                pixels: cap.pixels(),
                stride: cap.width() as usize,
                dirty_hint: cap.dirty_hint(),
                moves: cap.moves(),
            };
            if let Some(m) = enc.encode(&f) {
                bytes += m.len();
                frames += 1;
            }
            if let Some(rest) = tick.checked_sub(t.elapsed()) {
                thread::sleep(rest);
            }
        }
        let cpu = (thread_cpu() - cpu0).as_secs_f64() / start.elapsed().as_secs_f64() * 100.0;
        println!(
            "{:<26} {:>9.1} {:>7} {:>13.1}",
            format!("{name} [{}]", cap.backend()),
            bytes as f64 / 1024.0 / secs as f64,
            frames,
            cpu
        );
    }
    unsafe { std::env::remove_var("OPENDESK_CAPTURE") };
}

#[test]
#[ignore]
fn bench_video_pipeline() {
    let secs: usize = std::env::var("BENCH_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let mode: usize = std::env::var("BENCH_MODE").ok().and_then(|v| v.parse().ok()).unwrap_or(1080);
    let bounds = enumerate_monitors()[0];
    let (max_w, quality) = crate::domain::resolution::capture_params(mode, bounds.width, bounds.height);
    // Full-frame capture cost: GDI on Windows, the X11 capturer elsewhere.
    #[cfg(windows)]
    let mut gdi = core_engine::capture::GdiScreenCapturer::new_for_monitor(bounds, max_w);
    #[cfg(not(windows))]
    let mut gdi = DesktopCapturer::new_for_monitor(bounds, max_w);
    let (w, h) = (gdi.width as usize, gdi.height as usize);
    let t = Instant::now();
    for _ in 0..60 {
        #[cfg(windows)]
        gdi.capture_frame();
        #[cfg(not(windows))]
        gdi.grab(0);
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
    println!(
        "{:<13} {:<6} {:>9} {:>7} {:>10} {:>9} {:>10} {:>10} {:>6}",
        "scenario", "mode", "KB/s", "frames", "enc ms/fr", "enc max", "host ms/s", "dec ms/fr", "PSNR"
    );

    let ticks = secs * 60;
    let mut frame = Vec::new();
    for sc in [Scenario::Static, Scenario::TypingCursor, Scenario::Scroll, Scenario::WindowDrag] {
        for (delta, h264, label) in [
            (false, H264Mode::Off, "jpeg"),
            (true, H264Mode::Off, "delta"),
            (true, H264Mode::Auto, "d+h264"),
            (true, H264Mode::Always, "h264"),
        ] {
            let mut enc = StreamEncoder::new(w as i32, h as i32, quality, delta);
            enc.set_h264(h264);
            enc.set_lossless(std::env::var("BENCH_LOSSLESS").is_ok_and(|v| v == "1"));
            enc.wait_h264_ready();
            let mut viewer = ViewerDecoder::new();
            let (mut bytes, mut sent, mut enc_sent_us, mut enc_max, mut host_us, mut dec_us) =
                (0usize, 0usize, 0u64, 0u64, 0u64, 0u64);
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
            if let Some(name) = &enc.h264_name
                && sc as usize == 3
                && h264 == H264Mode::Always
            {
                println!("H.264 encoder: {name}");
            }
        }
    }
}
