use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_engine::capture::{enumerate_monitors, GdiScreenCapturer, MonitorBounds, ScreenCapturer};
use core_engine::codec::{from_base64, JpegEncoder};
use core_engine::diff::DirtyRectDetector;
use image::ImageReader;
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::logger;
use crate::protocol::OutgoingMsg;

pub fn decode_jpeg_bytes_to_pixel_buffer(bytes: &[u8]) -> Option<(SharedPixelBuffer<Rgba8Pixel>, u32, u32)> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let img = reader.decode().ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(img.as_raw());
    Some((buffer, width, height))
}

pub fn decode_b64_to_pixel_buffer(b64: &str) -> Option<(SharedPixelBuffer<Rgba8Pixel>, u32, u32)> {
    let bytes = from_base64(b64)?;
    decode_jpeg_bytes_to_pixel_buffer(&bytes)
}

pub fn spawn_host_capture_loop(
    _partner_clean: String,
    is_capturing: Arc<AtomicBool>,
    active_monitor_idx: Arc<AtomicUsize>,
    host_monitor_bounds: Arc<Mutex<MonitorBounds>>,
    can_send_frame: Arc<AtomicBool>,
    tx_out: Sender<OutgoingMsg>,
) {
    thread::spawn(move || {
        let mut monitors = enumerate_monitors();
        let mut current_idx = active_monitor_idx.load(Ordering::SeqCst) % monitors.len().max(1);
        let initial_bounds = monitors[current_idx];
        *host_monitor_bounds.lock().unwrap() = initial_bounds;

        // 1:1 Native Resolution up to 1920px wide + Quality 85 for razor-sharp desktop text
        let mut capturer = GdiScreenCapturer::new_for_monitor(initial_bounds, 1920);
        let encoder = JpegEncoder::new(85);
        let mut diff = DirtyRectDetector::new(capturer.width, capturer.height);
        let target_interval = Duration::from_micros(16_666); // 60.0 FPS pacing
        let mut idle_ticks: u64 = 0;

        logger::info(
            "desk_rt.display_component",
            &format!(
                "Host capture started: monitor={} native={}x{}+({},{}) stream={}x{} quality=85 fps=60",
                current_idx,
                capturer.screen_width,
                capturer.screen_height,
                capturer.offset_x,
                capturer.offset_y,
                capturer.width,
                capturer.height
            ),
        );

        while is_capturing.load(Ordering::SeqCst) {
            let frame_start = Instant::now();

            // Check if Viewer requested monitor switch
            let req_idx = active_monitor_idx.load(Ordering::Relaxed);
            if req_idx != current_idx {
                monitors = enumerate_monitors();
                let count = monitors.len().max(1);
                current_idx = req_idx % count;
                active_monitor_idx.store(current_idx, Ordering::Relaxed);
                let bounds = monitors[current_idx];
                *host_monitor_bounds.lock().unwrap() = bounds;
                capturer = GdiScreenCapturer::new_for_monitor(bounds, 1920);
                diff = DirtyRectDetector::new(capturer.width, capturer.height);
                idle_ticks = 60; // Force immediate frame send on monitor switch
                logger::info(
                    "desk_rt.display_component",
                    &format!(
                        "Switched to monitor {}/{}: {}x{} at ({},{})",
                        current_idx + 1,
                        count,
                        bounds.width,
                        bounds.height,
                        bounds.left,
                        bounds.top
                    ),
                );
            }

            // Backpressure gate: only capture & encode when the socket has finished flushing the previous frame
            if can_send_frame.load(Ordering::Acquire) {
                let (hbm, w, h) = capturer.capture_frame();
                let sample = capturer.sample_grid(w, h);
                let (changed, _) = diff.detect_changes(&sample);

                if changed || idle_ticks >= 30 {
                    idle_ticks = 0;
                    if let Some(jpeg) = encoder.encode_hbitmap(hbm) {
                        can_send_frame.store(false, Ordering::Release);
                        let _ = tx_out.send(OutgoingMsg::BinaryFrame(jpeg));
                    }
                } else {
                    idle_ticks += 1;
                }
            }

            let elapsed = frame_start.elapsed();
            if elapsed < target_interval {
                let remaining = target_interval - elapsed;
                if remaining > Duration::from_millis(2) {
                    thread::sleep(remaining - Duration::from_millis(1));
                }
                while frame_start.elapsed() < target_interval {
                    std::hint::spin_loop();
                }
            }
        }

        logger::info("desk_rt.display_component", "Host capture stopped.");
    });
}
