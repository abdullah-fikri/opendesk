#![allow(non_camel_case_types, non_snake_case, dead_code)]
#![windows_subsystem = "console"]

mod capture;
mod codec;
mod diff;
mod input;

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use capture::{GdiScreenCapturer, ScreenCapturer};
use codec::{to_base64, JpegEncoder};
use diff::DirtyRectDetector;
use input::InputInjector;

fn parse_json_value(json_str: &str, key: &str) -> Option<String> {
    let search = format!("\"{}\":", key);
    if let Some(pos) = json_str.find(&search) {
        let rest = json_str[pos + search.len()..].trim_start();
        if rest.starts_with('"') {
            let inner = &rest[1..];
            if let Some(end) = inner.find('"') {
                return Some(inner[..end].to_string());
            }
        } else {
            let mut val = String::new();
            for c in rest.chars() {
                if c.is_ascii_digit() || c == '-' || c == '.' {
                    val.push(c);
                } else {
                    break;
                }
            }
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

fn main() {
    let mut capturer = GdiScreenCapturer::new();
    let injector = Arc::new(InputInjector::new());
    let encoder = JpegEncoder::new(85);
    let mut diff_detector = DirtyRectDetector::new(capturer.width, capturer.height);

    eprintln!(
        "   info                    core_engine  {:5}                           desk_rt.capture - Initialized {}x{} @ Q85",
        std::process::id(),
        capturer.width,
        capturer.height
    );

    let is_capturing = Arc::new(AtomicBool::new(false));
    let is_capturing_thread = Arc::clone(&is_capturing);

    // Stdin Input & Command Listener Thread
    let injector_listener = Arc::clone(&injector);
    thread::spawn(move || {
        let stdin = io::stdin();
        let handle = stdin.lock();

        for line_res in handle.lines() {
            let line = match line_res {
                Ok(l) => l,
                Err(_) => break,
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(cmd) = parse_json_value(trimmed, "cmd") {
                match cmd.as_str() {
                    "start_capture" => {
                        is_capturing_thread.store(true, Ordering::SeqCst);
                    }
                    "stop_capture" => {
                        is_capturing_thread.store(false, Ordering::SeqCst);
                    }
                    "input" => {
                        let action = parse_json_value(trimmed, "action").unwrap_or_default();
                        let x: i32 = parse_json_value(trimmed, "x")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let y: i32 = parse_json_value(trimmed, "y")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let button: i32 = parse_json_value(trimmed, "button")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let key_code: u8 = parse_json_value(trimmed, "keyCode")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let delta_y: i32 = parse_json_value(trimmed, "deltaY")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);

                        match action.as_str() {
                            "mousemove" => {
                                injector_listener.set_cursor_pos(x, y);
                            }
                            "mousedown" => {
                                injector_listener.set_cursor_pos(x, y);
                                injector_listener.mouse_down(button);
                            }
                            "mouseup" => {
                                injector_listener.set_cursor_pos(x, y);
                                injector_listener.mouse_up(button);
                            }
                            "wheel" => {
                                injector_listener.set_cursor_pos(x, y);
                                injector_listener.mouse_wheel(delta_y);
                            }
                            "keydown" => {
                                if key_code > 0 {
                                    injector_listener.key_down(key_code);
                                }
                            }
                            "keyup" => {
                                if key_code > 0 {
                                    injector_listener.key_up(key_code);
                                }
                            }
                            _ => {}
                        }
                    }
                    "clipboard" => {
                        if let Some(text) = parse_json_value(trimmed, "text") {
                            injector_listener.set_clipboard_text(&text);
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    // Capture & Broadcast Loop
    let stdout = io::stdout();
    let mut out_handle = stdout.lock();
    let target_interval = Duration::from_millis(16); // 60 FPS target
    let mut frame_seq: u64 = 0;

    loop {
        if is_capturing.load(Ordering::SeqCst) {
            let start = Instant::now();
            let (hbm, w, h) = capturer.capture_frame();

            let sample = capturer.sample_grid(w, h);
            let (has_changed, _) = diff_detector.detect_changes(&sample);

            // Send keyframe every 60 frames or whenever changes detected
            if has_changed || frame_seq % 60 == 0 {
                frame_seq += 1;
                if let Some(encoded) = encoder.encode_hbitmap(hbm) {
                    let b64 = to_base64(&encoded);
                    let _ = writeln!(out_handle, "FRAME:{}", b64);
                    let _ = out_handle.flush();
                }
            }

            let elapsed = start.elapsed();
            if elapsed < target_interval {
                thread::sleep(target_interval - elapsed);
            }
        } else {
            diff_detector.reset();
            thread::sleep(Duration::from_millis(50));
        }
    }
}
