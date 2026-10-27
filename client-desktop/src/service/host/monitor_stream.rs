//! Monitor-wall thumbnails (see `docs/features/monitor-wall.md` at the repository root). While the relay says an
//! administrator watches this machine, the primary monitor is captured (GDI on Windows, X11 on Linux) a few times per second, scaled
//! to tile size, and sent as a plain JPEG whenever it changed. The relay sends the newest one to every watcher,
//! however many there are.
//!
//! View only: nothing here reads input, and the stream is independent of the hosted session, whose capture loop
//! keeps DXGI to itself. Windows hosts are unattended, so a watch follows the same rule as a session: no prompt,
//! but the status line and the log say this screen is being watched.

use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use core_engine::capture::{MonitorBounds, ThumbnailCapturer, enumerate_monitors};
use core_engine::codec::JpegEncoder;

use crate::infra::logger;
use crate::protocol::{Outgoing, binary};
use crate::service::context::{AppContext, Ctx};

/// JPEG quality of thumbnails: text stays legible at tile size, a 640 px desktop is typically 15-40 KB.
const QUALITY: u32 = 50;
/// How often the primary monitor's geometry is re-checked.
const MONITOR_POLL: Duration = Duration::from_secs(2);

/// What the relay asked for, clamped to what this host is willing to spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub max_width: i32,
    pub fps: u32,
}

impl Profile {
    /// Limits regardless of the request: 160..=960 px wide, 1..=5 frames per second (defaults 640 px, 2 fps).
    pub fn clamped(max_width: i64, fps: i64) -> Self {
        let max_width = if max_width <= 0 { 640 } else { max_width.clamp(160, 960) as i32 };
        let fps = if fps <= 0 { 2 } else { fps.clamp(1, 5) as u32 };
        Self { max_width, fps }
    }

    fn interval(self) -> Duration {
        Duration::from_millis(1000 / self.fps as u64)
    }
}

/// Starts (or restarts, so the relay gets a fresh first frame) the thumbnail stream.
pub fn start(ctx: &Ctx, profile: Profile) {
    let generation = ctx.host.monitor_generation.fetch_add(1, Ordering::SeqCst) + 1;
    if !ctx.host.monitored.swap(true, Ordering::SeqCst) {
        ctx.ui.status("An administrator is viewing this screen (view only).".into());
    }
    logger::info(
        "host.monitor",
        &format!("Monitor-wall stream started: {}px, {} fps.", profile.max_width, profile.fps),
    );
    let ctx = std::sync::Arc::clone(ctx);
    crate::infra::thread::spawn_named("host-monitor-stream", move || run(&ctx, generation, profile));
}

/// Stops the thumbnail stream (nobody watches, or the relay connection ended).
pub fn stop(ctx: &AppContext) {
    if ctx.host.monitored.swap(false, Ordering::SeqCst) {
        ctx.host.monitor_generation.fetch_add(1, Ordering::SeqCst);
        logger::info("host.monitor", "Monitor-wall stream stopped.");
    }
}

fn primary() -> MonitorBounds {
    enumerate_monitors()[0]
}

fn same(a: MonitorBounds, b: MonitorBounds) -> bool {
    (a.left, a.top, a.width, a.height) == (b.left, b.top, b.width, b.height)
}

fn run(ctx: &Ctx, generation: u64, profile: Profile) {
    let current = || ctx.host.monitor_generation.load(Ordering::SeqCst) == generation;
    // Follow the secure desktop (UAC / lock screen) when running as SYSTEM; a no-op otherwise.
    core_engine::desktop::switch_to_input_desktop();
    let mut bounds = primary();
    let mut capturer = ThumbnailCapturer::new_for_monitor(bounds, profile.max_width);
    let encoder = JpegEncoder::new(QUALITY);
    // The last image sent: an unchanged screen costs one comparison and nothing on the wire.
    let mut sent: Vec<u32> = Vec::new();
    let mut last_poll = Instant::now();

    while current() {
        let tick = Instant::now();
        let switched = core_engine::desktop::switch_to_input_desktop();
        if switched || tick.duration_since(last_poll) >= MONITOR_POLL {
            last_poll = tick;
            let fresh = primary();
            if switched || !same(fresh, bounds) {
                bounds = fresh;
                capturer = ThumbnailCapturer::new_for_monitor(bounds, profile.max_width);
                sent.clear();
            }
        }
        capturer.capture_frame();
        let pixels = capturer.pixels();
        if pixels != sent.as_slice() {
            let (w, h) = (capturer.width, capturer.height);
            if let Some(jpeg) = encoder.encode_bgra(pixels, w, h, w as usize) {
                let _ = ctx.outbox.send(Outgoing::MonitorFrame(binary::monitor_frame(&jpeg)));
            }
            sent.clear();
            sent.extend_from_slice(pixels);
        }
        if let Some(rest) = profile.interval().checked_sub(tick.elapsed()) {
            thread::sleep(rest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_is_clamped_to_host_limits() {
        assert_eq!(Profile::clamped(640, 2), Profile { max_width: 640, fps: 2 });
        assert_eq!(Profile::clamped(0, 0), Profile { max_width: 640, fps: 2 });
        assert_eq!(Profile::clamped(10_000, 60), Profile { max_width: 960, fps: 5 });
        assert_eq!(Profile::clamped(16, 1), Profile { max_width: 160, fps: 1 });
        assert_eq!(Profile::clamped(640, 4).interval(), Duration::from_millis(250));
    }

    #[test]
    fn start_and_stop_follow_the_relay() {
        let (ctx, rx) = crate::service::test_support::context_with_outbox();
        start(&ctx, Profile::clamped(320, 5));
        let first = rx.recv_timeout(Duration::from_secs(5)).expect("first thumbnail is sent at once");
        let Outgoing::MonitorFrame(frame) = first else { panic!("unexpected {first:?}") };
        assert!(binary::is_monitor(&frame) && frame[1..3] == [0xFF, 0xD8], "tagged plain JPEG");
        stop(&ctx);
        assert!(!ctx.host.monitored.load(Ordering::SeqCst));
        thread::sleep(Duration::from_millis(400));
        while rx.try_recv().is_ok() {}
        thread::sleep(Duration::from_millis(400));
        assert!(rx.try_recv().is_err(), "stream stopped");
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

    /// Cost of one thumbnail on this desktop (GDI's StretchBlt costs less CPU than a native BitBlt plus a box
    /// filter, and both wait about the same on the compositor):
    /// `cargo test --release bench_monitor_stream -- --ignored --nocapture` (BENCH_FRAMES, default 20).
    #[test]
    #[ignore]
    fn bench_monitor_stream() {
        let frames: u32 = std::env::var("BENCH_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
        let bounds = primary();
        let mut capturer = ThumbnailCapturer::new_for_monitor(bounds, 640);
        let encoder = JpegEncoder::new(QUALITY);
        let (mut wall, mut cpu, mut bytes) = (Duration::ZERO, Duration::ZERO, 0usize);
        for _ in 0..frames {
            let (t, c) = (Instant::now(), thread_cpu());
            capturer.capture_frame();
            let (w, h) = (capturer.width, capturer.height);
            bytes += encoder.encode_bgra(capturer.pixels(), w, h, w as usize).map_or(0, |j| j.len());
            wall += t.elapsed();
            cpu += thread_cpu() - c;
            thread::sleep(Duration::from_millis(100));
        }
        let n = frames as f64;
        println!(
            "\nthumbnail {}x{} from {}x{}: wall {:.2} ms, cpu {:.2} ms, {:.1} KB/frame -> at 2 fps, every frame changed: {:.2} % of one core, {:.1} KB/s",
            capturer.width,
            capturer.height,
            bounds.width,
            bounds.height,
            wall.as_secs_f64() * 1000.0 / n,
            cpu.as_secs_f64() * 1000.0 / n,
            bytes as f64 / 1024.0 / n,
            cpu.as_secs_f64() / n * 2.0 * 100.0,
            bytes as f64 / 1024.0 / n * 2.0
        );
    }
}
