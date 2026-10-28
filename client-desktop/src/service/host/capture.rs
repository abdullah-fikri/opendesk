//! Host capture loop: captures the selected monitor at up to 60 FPS, encodes it for the viewer and announces
//! monitor and cursor changes. Runs on its own thread for one hosted session.

use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use core_engine::capture::{DesktopCapturer, MonitorBounds, cursor_shape, enumerate_monitors};
use core_engine::stream::{FrameInput, H264Mode, StreamEncoder};

use crate::domain::perf_stats::{Counter, PerfWindow, STATS_INTERVAL, Stage, kbps};
use crate::domain::resolution;
use crate::infra::{logger, power};
use crate::protocol::{HostStats, InputEvent, MonitorInfo, Outgoing};
use crate::service::context::Ctx;

/// How often the loop re-enumerates monitors to notice plugged, unplugged or resized displays.
const MONITOR_POLL: Duration = Duration::from_secs(2);
/// 60 FPS pacing.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_666);
/// How often a tick re-checks the backpressure gate while the previous frame is still on its way.
const GATE_POLL: Duration = Duration::from_millis(1);

/// Turns how long the loop waited for the backpressure gate into a 0..1 congestion level:
/// waits under ~40 ms are normal pacing, ~250 ms or more means the link cannot keep up.
pub struct CongestionMeter {
    blocked_since: Option<Instant>,
    ewma_ms: f32,
}

impl CongestionMeter {
    /// A meter for an idle link.
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
            (true, None) => self.ewma_ms *= 0.98, // an idle link recovers slowly
            (false, Some(_)) => {}
        }
        self.level()
    }

    /// The level as of the last `observe`.
    pub fn level(&self) -> f32 {
        ((self.ewma_ms - 40.0) / 210.0).clamp(0.0, 1.0)
    }
}

/// Capture-loop counters for one `host.perf` window (fields documented in `domain::perf_stats`).
#[derive(Default)]
struct HostPerf {
    grab: Stage,
    encode: Stage,
    gate: Stage,
    sent: Counter,
    bytes: Counter,
    idle: Counter,
    gate_skips: Counter,
}

impl HostPerf {
    /// The window's log line; resets the counters.
    fn take_line(&mut self, secs: f64, h264: Option<&str>) -> String {
        let h264 = h264.map_or_else(|| "off".to_string(), |name| format!("on({name})"));
        format!(
            "fps={:.1} target={:.0} grab avg/max={} encode={} gate={} kbps={} idle_frames={} gate_skips={} h264={h264}",
            self.sent.take() as f64 / secs,
            1.0 / FRAME_INTERVAL.as_secs_f64(),
            self.grab.take_summary(),
            self.encode.take_summary(),
            self.gate.take_summary(),
            kbps(self.bytes.take(), secs),
            self.idle.take(),
            self.gate_skips.take(),
        )
    }

    /// The `host-stats` summary since the previous one, independent of the log window.
    fn take_stats(&mut self, secs: f64, congestion: f32, encoder: Option<&str>) -> HostStats {
        let (grab, encode) = (self.grab.take_lap(), self.encode.take_lap());
        self.gate.take_lap();
        HostStats {
            fps: (self.sent.take_lap() as f64 / secs) as f32,
            target_fps: (1.0 / FRAME_INTERVAL.as_secs_f64()).round() as u32,
            grab_ms: grab.avg_ms as f32,
            grab_max_ms: grab.max_ms as f32,
            encode_ms: encode.avg_ms as f32,
            encode_max_ms: encode.max_ms as f32,
            kbps: kbps(self.bytes.take_lap(), secs),
            idle: self.idle.take_lap() as u32,
            gate_skips: self.gate_skips.take_lap() as u32,
            congestion,
            encoder: encoder.unwrap_or_default().to_string(),
        }
    }
}

/// The `monitors` event: monitor list (primary first, as `enumerate_monitors` orders it) and the streamed one.
pub fn monitors_event(monitors: &[MonitorBounds], active: usize) -> InputEvent {
    let list = monitors
        .iter()
        .enumerate()
        .map(|(i, m)| MonitorInfo { index: i as u64, width: m.width as i64, height: m.height as i64, primary: i == 0 })
        .collect();
    InputEvent::Monitors { list, active: active as u64 }
}

fn params(mode: usize, bounds: MonitorBounds) -> (i32, u32) {
    resolution::capture_params(mode, bounds.width, bounds.height)
}

/// H.264 policy: `OPENDESK_VIDEO_H264=0` disables it, `=always` uses it for every changed frame, otherwise it
/// is used for motion when the viewer supports it.
fn h264_mode(viewer_supports: bool) -> H264Mode {
    match std::env::var("OPENDESK_VIDEO_H264").unwrap_or_default().as_str() {
        "0" => H264Mode::Off,
        "always" => H264Mode::Always,
        _ if viewer_supports => H264Mode::Auto,
        _ => H264Mode::Off,
    }
}

/// Starts the loop for `partner`; it runs until the hosted session's `generation` is superseded.
pub fn spawn(ctx: Ctx, partner: String, generation: u64) {
    thread::spawn(move || run(&ctx, &partner, generation));
}

fn run(ctx: &Ctx, partner: &str, generation: u64) {
    let host = &ctx.host;
    // Full speed for the whole session, before the first capture.
    let _awake = power::awake();
    power::raise_thread_priority();
    // Follow the secure desktop (UAC / lock screen) when running as SYSTEM; a no-op otherwise.
    core_engine::desktop::switch_to_input_desktop();
    let mut monitors = enumerate_monitors();
    let mut current_idx = host.monitor_index.load(Ordering::SeqCst) % monitors.len().max(1);
    let initial_bounds = monitors[current_idx];
    *host.monitor_bounds.lock().unwrap() = initial_bounds;

    let mut current_mode = host.resolution_mode.load(Ordering::SeqCst);
    let (mut max_width, mut base_quality) = params(current_mode, initial_bounds);
    let caps = *host.peer_caps.lock().unwrap();
    let local_cursor = caps.cursor_shape;
    let h264 = h264_mode(caps.video_h264);
    host.keyframe_requested.store(false, Ordering::SeqCst);

    let mut capturer = DesktopCapturer::new_for_monitor(initial_bounds, max_width);
    capturer.set_hide_viewer_cursor(local_cursor);
    let mut encoder = StreamEncoder::new(capturer.width(), capturer.height(), base_quality, caps.video_delta);
    encoder.set_h264(h264);
    let mut h264_logged = false;
    let mut last_shape = u8::MAX;
    let mut congestion = CongestionMeter::new();

    let (ox, oy, sw, sh, w, h) = capturer.geometry();
    let video = if !caps.video_delta {
        "jpeg"
    } else if h264 != H264Mode::Off {
        "delta+h264"
    } else {
        "delta"
    };
    logger::info(
        "host.capture",
        &format!(
            "Capture started: monitor={current_idx} native={sw}x{sh}+({ox},{oy}) stream={w}x{h} mode={current_mode} quality={base_quality} capture={} video={video}",
            capturer.backend()
        ),
    );

    // Announced at start and whenever the list or the streamed monitor changes.
    let mut sent_monitors: Option<InputEvent> = None;
    let mut monitors_dirty = true;
    let mut last_monitor_poll = Instant::now();
    let mut perf = HostPerf::default();
    let mut perf_window = PerfWindow::new(last_monitor_poll);
    let mut stats_window = PerfWindow::every(last_monitor_poll, STATS_INTERVAL);

    while host.is_current(generation) {
        let frame_start = Instant::now();
        if let Some(secs) = perf_window.due(frame_start) {
            let h264 = if h264 == H264Mode::Off { None } else { Some(encoder.h264_name.as_deref().unwrap_or("idle")) };
            logger::info("host.perf", &perf.take_line(secs, h264));
        }
        // Laps run while the overlay is hidden too, so its first summary covers only the last second.
        if let Some(secs) = stats_window.due(frame_start) {
            let stats = perf.take_stats(secs, congestion.level(), encoder.h264_name.as_deref());
            if host.stats_requested.load(Ordering::Relaxed) {
                ctx.send_input(partner, InputEvent::HostStats(stats));
            }
        }

        // Monitor switch, resolution change or a new input desktop all rebuild the pipeline.
        let req_idx = host.monitor_index.load(Ordering::Relaxed);
        let req_mode = host.resolution_mode.load(Ordering::Relaxed);
        let desktop_switched = core_engine::desktop::switch_to_input_desktop();
        let mode_only = req_mode != current_mode && req_idx == current_idx && !desktop_switched;
        if mode_only && params(req_mode, *host.monitor_bounds.lock().unwrap()) == (max_width, base_quality) {
            // Same stream size and quality (e.g. a resize the aspect ratio absorbs): keep the pipeline.
            current_mode = req_mode;
        } else if req_idx != current_idx || req_mode != current_mode || desktop_switched {
            monitors = enumerate_monitors();
            let count = monitors.len().max(1);
            current_idx = req_idx % count;
            host.monitor_index.store(current_idx, Ordering::Relaxed);
            current_mode = req_mode;
            let bounds = monitors[current_idx];
            (max_width, base_quality) = params(current_mode, bounds);
            *host.monitor_bounds.lock().unwrap() = bounds;
            // Drop the old duplication first: an output allows only a few duplications at once.
            drop(capturer);
            capturer = DesktopCapturer::new_for_monitor(bounds, max_width);
            capturer.set_hide_viewer_cursor(local_cursor);
            encoder.reconfigure(capturer.width(), capturer.height(), base_quality);
            monitors_dirty = true;
            logger::info(
                "host.capture",
                &format!(
                    "Capture reconfigured: monitor={}/{count} stream={}x{} mode={current_mode} quality={base_quality} capture={} desktop_switch={desktop_switched}",
                    current_idx + 1,
                    capturer.width(),
                    capturer.height(),
                    capturer.backend()
                ),
            );
        }

        if frame_start.duration_since(last_monitor_poll) >= MONITOR_POLL {
            last_monitor_poll = frame_start;
            let fresh = enumerate_monitors();
            if fresh.len() != monitors.len()
                || fresh
                    .iter()
                    .zip(&monitors)
                    .any(|(a, b)| (a.left, a.top, a.width, a.height) != (b.left, b.top, b.width, b.height))
            {
                monitors = fresh;
                monitors_dirty = true;
            }
        }
        if monitors_dirty {
            monitors_dirty = false;
            let event = monitors_event(&monitors, current_idx);
            if sent_monitors.as_ref() != Some(&event) {
                ctx.send_input(partner, event.clone());
                sent_monitors = Some(event);
            }
        }

        if local_cursor {
            let shape = cursor_shape();
            if shape != last_shape {
                last_shape = shape;
                ctx.send_input(partner, InputEvent::CursorShape { shape: shape as i32 });
            }
        }
        if host.keyframe_requested.swap(false, Ordering::SeqCst) {
            encoder.request_key();
        }

        // The previous frame usually leaves within a few milliseconds: wait for it inside this tick instead of
        // skipping the whole tick, which would halve the frame rate whenever sending takes a moment.
        let mut can_send = host.can_send_frame.load(Ordering::Acquire);
        let mut level = congestion.observe(can_send, frame_start);
        if !can_send {
            let gate_start = Instant::now();
            while !can_send && frame_start.elapsed() + GATE_POLL < FRAME_INTERVAL && host.is_current(generation) {
                thread::sleep(GATE_POLL);
                can_send = host.can_send_frame.load(Ordering::Acquire);
            }
            let now = Instant::now();
            perf.gate.record(now - gate_start);
            if can_send {
                level = congestion.observe(true, now);
            } else {
                perf.gate_skips += 1;
            }
        } else {
            perf.gate.record(Duration::ZERO);
        }
        encoder.set_congestion(level);
        if can_send {
            // DXGI blocks here until the desktop changes (or the tick ends), so an idle desktop costs ~nothing.
            let budget = FRAME_INTERVAL.saturating_sub(frame_start.elapsed());
            let backend = capturer.backend();
            let grab_start = Instant::now();
            capturer.grab(budget.as_millis() as u32);
            perf.grab.record(grab_start.elapsed());
            if capturer.backend() != backend {
                logger::warn("host.capture", "DXGI duplication lost for good; falling back to GDI capture.");
                encoder.request_key();
            }
            let frame = FrameInput {
                pixels: capturer.pixels(),
                stride: capturer.width() as usize,
                dirty_hint: capturer.dirty_hint(),
                moves: capturer.moves(),
            };
            let sent = encoder.encode(&frame);
            // `encode` times itself; reuse that instead of another clock read.
            perf.encode.record(Duration::from_micros(encoder.last.encode_us));
            if let Some(msg) = sent {
                perf.sent += 1;
                perf.bytes += msg.len() as u64;
                if !h264_logged && let Some(name) = &encoder.h264_name {
                    h264_logged = true;
                    logger::info("host.capture", &format!("H.264 motion encoder: {name}"));
                }
                host.can_send_frame.store(false, Ordering::Release);
                let _ = ctx.outbox.send(Outgoing::VideoFrame(msg));
            } else {
                perf.idle += 1;
            }
        }

        // std sleeps on a high-resolution waitable timer on Windows, so no busy-spin is needed.
        if let Some(remaining) = FRAME_INTERVAL.checked_sub(frame_start.elapsed()) {
            thread::sleep(remaining);
        }
    }

    // The session is over: lift any key its viewer left pressed on this machine.
    host.release_all_keys(&ctx.injector);
    logger::info("host.capture", "Capture stopped.");
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn perf_line_reports_the_window() {
        let mut p = HostPerf::default();
        for _ in 0..589 {
            p.grab.record(Duration::from_micros(1200));
            p.encode.record(Duration::from_micros(3100));
            p.gate.record(Duration::from_micros(400));
        }
        p.grab.record(Duration::from_millis(4));
        p.sent += 589;
        p.bytes += 3_062_500;
        p.idle += 12;
        let line = p.take_line(10.0, Some("Intel QSV (hardware MFT)"));
        assert_eq!(
            line,
            "fps=58.9 target=60 grab avg/max=1.2/4.0ms encode=3.1/3.1ms gate=0.4/0.4ms kbps=2450 idle_frames=12 gate_skips=0 h264=on(Intel QSV (hardware MFT))"
        );
        assert!(p.take_line(10.0, None).starts_with("fps=0.0 target=60 grab avg/max=0.0/0.0ms"));
    }

    #[test]
    fn stats_summary_is_independent_of_the_log_window() {
        let mut p = HostPerf::default();
        p.grab.record(Duration::from_micros(1000));
        p.grab.record(Duration::from_micros(3000));
        p.encode.record(Duration::from_micros(2500));
        p.sent += 30;
        p.bytes += 250_000;
        p.gate_skips += 3;
        p.take_line(10.0, None);
        let s = p.take_stats(0.5, 0.25, Some("Intel QSV (hardware MFT)"));
        assert_eq!(
            s,
            HostStats {
                fps: 60.0,
                target_fps: 60,
                grab_ms: 2.0,
                grab_max_ms: 3.0,
                encode_ms: 2.5,
                encode_max_ms: 2.5,
                kbps: 4000,
                idle: 0,
                gate_skips: 3,
                congestion: 0.25,
                encoder: "Intel QSV (hardware MFT)".into(),
            }
        );
        assert_eq!(p.take_stats(1.0, 0.0, None), HostStats { target_fps: 60, ..Default::default() });
    }

    #[test]
    fn monitors_event_lists_primary_first() {
        let mons = [
            MonitorBounds { left: 0, top: 0, width: 1920, height: 1080 },
            MonitorBounds { left: 1920, top: 0, width: 2560, height: 1440 },
        ];
        let InputEvent::Monitors { list, active } = monitors_event(&mons, 1) else { panic!() };
        assert_eq!((list.len(), active, list[0].primary, list[1].width), (2, 1, true, 2560));
    }
}
