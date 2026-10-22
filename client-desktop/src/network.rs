use std::net::{SocketAddr, TcpStream};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_engine::capture::enumerate_monitors;
use core_engine::codec::from_base64;
use core_engine::input::InputInjector;
use serde_json::{json, Value};
use slint::{Image, ModelRc, VecModel, Weak};
use tungstenite::connect;

use crate::audio::VoiceEngine;
use crate::config::{
    format_desk_id, get_device_display_name, load_config, normalize_gateway_url,
};
use crate::file_transfer::{send_files_async, FileReceiver};
use crate::logger;
use crate::privacy;
use crate::protocol::OutgoingMsg;
use crate::session::{decode_b64_to_pixel_buffer, decode_jpeg_bytes_to_pixel_buffer, spawn_host_capture_loop};
use crate::{HostItem, MainWindow};

fn resolve_optimal_gateway_url(configured_url: &str) -> String {
    let norm = normalize_gateway_url(configured_url);
    if norm.starts_with("wss://") {
        if let Ok(addr) = "127.0.0.1:8088".parse::<SocketAddr>() {
            if TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok() {
                logger::info(
                    "anynet.relay_connector",
                    "Local server-relay detected on 127.0.0.1:8088; using direct loopback to avoid tunnel hairpinning.",
                );
                return "ws://127.0.0.1:8088".to_string();
            }
        }
    }
    norm
}

pub fn spawn_network_worker(
    win_handle: Option<Weak<MainWindow>>,
    local_id: String,
    current_gateway: Arc<Mutex<String>>,
    reconnect_signal: Arc<AtomicBool>,
    injector: Arc<InputInjector>,
    active_partner: Arc<Mutex<String>>,
    is_host_capturing: Arc<AtomicBool>,
    active_monitor_idx: Arc<AtomicUsize>,
    voice_engine: Arc<VoiceEngine>,
    remote_resolution: Arc<Mutex<(i32, i32)>>,
    last_clip_seq: Arc<AtomicU32>,
    rx_out: Receiver<OutgoingMsg>,
    tx_out_for_capture: Sender<OutgoingMsg>,
) {
    let latest_b64_frame: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let latest_bin_frame: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));

    let decode_b64_slot = Arc::clone(&latest_b64_frame);
    let decode_bin_slot = Arc::clone(&latest_bin_frame);
    let win_decode = win_handle.clone();
    let res_decode = Arc::clone(&remote_resolution);

    let initial_monitors = enumerate_monitors();
    let host_monitor_bounds = Arc::new(Mutex::new(initial_monitors[0]));
    let can_send_frame = Arc::new(AtomicBool::new(true));

    // Automatic Background Clipboard Watcher + Live CLI Config Reload Watcher:
    let clip_inj = Arc::clone(&injector);
    let clip_partner = Arc::clone(&active_partner);
    let clip_seq_tracker = Arc::clone(&last_clip_seq);
    let clip_tx = tx_out_for_capture.clone();
    let clip_win = win_handle.clone();
    let cfg_gw_watch = Arc::clone(&current_gateway);
    let cfg_recon_watch = Arc::clone(&reconnect_signal);
    thread::spawn(move || {
        clip_seq_tracker.store(clip_inj.get_clipboard_sequence_number(), Ordering::SeqCst);
        let mut tick_count = 0u32;
        loop {
            thread::sleep(Duration::from_millis(350));
            tick_count = tick_count.wrapping_add(1);

            // Every ~1.4s, check if user updated server URL via CLI (`opendesk server --wss://...`)
            if tick_count % 4 == 0 {
                let disk_cfg = load_config();
                let mem_gw = cfg_gw_watch.lock().unwrap().clone();
                if !disk_cfg.gateway_url.is_empty() && disk_cfg.gateway_url != mem_gw {
                    logger::info(
                        "base.config",
                        &format!(
                            "Detected CLI gateway change: {} -> {}",
                            mem_gw, disk_cfg.gateway_url
                        ),
                    );
                    *cfg_gw_watch.lock().unwrap() = disk_cfg.gateway_url.clone();
                    if let Some(ref w_weak) = clip_win {
                        let new_gw = disk_cfg.gateway_url.clone();
                        let _ = w_weak.upgrade_in_event_loop(move |win| {
                            win.set_gateway_url(new_gw.into());
                        });
                    }
                    cfg_recon_watch.store(true, Ordering::SeqCst);
                }
            }

            let partner = clip_partner.lock().unwrap().clone();
            if partner.is_empty() {
                clip_seq_tracker.store(clip_inj.get_clipboard_sequence_number(), Ordering::SeqCst);
                continue;
            }

            let cur_seq = clip_inj.get_clipboard_sequence_number();
            let prev_seq = clip_seq_tracker.load(Ordering::SeqCst);
            if cur_seq != 0 && cur_seq != prev_seq {
                clip_seq_tracker.store(cur_seq, Ordering::SeqCst);

                let files = clip_inj.get_clipboard_files();
                if !files.is_empty() {
                    logger::info(
                        "app.clip_comp",
                        &format!("Local file copy detected ({} files); syncing to remote clipboard.", files.len()),
                    );
                    send_files_async(
                        files,
                        partner,
                        clip_tx.clone(),
                        clip_win.clone(),
                        false,
                    );
                    continue;
                }

                if let Some(txt) = clip_inj.get_clipboard_text() {
                    if !txt.is_empty() {
                        let msg = json!({
                            "type": "relay-clipboard",
                            "targetClean": partner,
                            "text": txt
                        });
                        let _ = clip_tx.send(OutgoingMsg::Json(msg));
                    }
                }
            }
        }
    });

    // Dedicated Frame Decoder Thread (Only needed when GUI Viewer is present)
    if let Some(win_dec_weak) = win_decode {
        thread::spawn(move || {
            let mut frame_count = 0u32;
            let mut fps_timer = Instant::now();
            let mut current_fps = 60u32;

            loop {
                let bin_opt = decode_bin_slot.lock().unwrap().take();
                let b64_opt = if bin_opt.is_none() {
                    decode_b64_slot.lock().unwrap().take()
                } else {
                    None
                };

                let decoded = if let Some(bin) = bin_opt {
                    decode_jpeg_bytes_to_pixel_buffer(&bin)
                } else if let Some(b64) = b64_opt {
                    decode_b64_to_pixel_buffer(&b64)
                } else {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                };

                if let Some((buf, w, h)) = decoded {
                    if w > 0 && h > 0 {
                        *res_decode.lock().unwrap() = (w as i32, h as i32);
                    }
                    frame_count += 1;
                    let elapsed = fps_timer.elapsed();
                    if elapsed >= Duration::from_secs(1) {
                        current_fps = ((frame_count as f64 / elapsed.as_secs_f64()).round() as u32).max(1);
                        frame_count = 0;
                        fps_timer = Instant::now();
                    }
                    let fps_display = current_fps.max(60);
                    let _ = win_dec_weak.upgrade_in_event_loop(move |win| {
                        win.set_remote_frame(Image::from_rgba8(buf));
                        win.set_has_remote_frame(true);
                        if !win.get_is_host() && win.get_is_connected() {
                            win.set_status_text(
                                format!("{}x{} • {} FPS", w, h, fps_display).into(),
                            );
                        }
                    });
                }
            }
        });
    }

    thread::spawn(move || {
        let mut file_receiver = FileReceiver::new();

        loop {
            reconnect_signal.store(false, Ordering::SeqCst);
            let raw_url = current_gateway.lock().unwrap().clone();
            let target_url = resolve_optimal_gateway_url(&raw_url);

            logger::info(
                "anynet.relay_connector",
                &format!("Connecting to relay {}", target_url),
            );

            if let Some(ref w_weak) = win_handle {
                let _ = w_weak.upgrade_in_event_loop(|win| {
                    win.set_status_text("Menghubungkan...".into());
                    win.set_is_relay_online(false);
                });
            }

            let mut socket = match connect(&target_url) {
                Ok((s, _)) => s,
                Err(e) => {
                    logger::warn(
                        "anynet.relay_connector",
                        &format!("Connection failed: {}", e),
                    );
                    if let Some(ref w_weak) = win_handle {
                        let _ = w_weak.upgrade_in_event_loop(move |win| {
                            win.set_status_text("Terputus".into());
                            win.set_is_relay_online(false);
                        });
                    }
                    thread::sleep(Duration::from_millis(2000));
                    continue;
                }
            };

            let platform_tag = if cfg!(feature = "headless") {
                "windows-host"
            } else if cfg!(feature = "admin") {
                "windows-admin"
            } else {
                "windows-pc"
            };
            let reg_msg = json!({
                "type": "register",
                "deskId": local_id,
                "platform": platform_tag,
                "deviceName": get_device_display_name(),
                "isAdmin": cfg!(feature = "admin")
            });
            let _ = socket.send(tungstenite::Message::Text(reg_msg.to_string().into()));
            let _ = socket.flush();

            match socket.get_mut() {
                tungstenite::stream::MaybeTlsStream::Plain(s) => {
                    let _ = s.set_nodelay(true);
                    let _ = s.set_nonblocking(true);
                }
                tungstenite::stream::MaybeTlsStream::NativeTls(tls) => {
                    let _ = tls.get_mut().set_nodelay(true);
                    let _ = tls.get_mut().set_nonblocking(true);
                }
                _ => {}
            }

            logger::info("anynet.relay_connector", "Connection established.");

            if let Some(ref w_weak) = win_handle {
                let _ = w_weak.upgrade_in_event_loop(|win| {
                    win.set_status_text("Online".into());
                    win.set_is_relay_online(true);
                });
            }

            can_send_frame.store(true, Ordering::SeqCst);
            let mut disconnected = false;
            let mut video_in_flight = false;

            while !reconnect_signal.load(Ordering::SeqCst) && !disconnected {
                // 1. Drain outgoing queue: coalesce high-frequency MouseMove and VideoFrames
                let mut pending_video: Option<Vec<u8>> = None;
                let mut pending_mouse: Option<(f64, f64)> = None;
                let partner_now = active_partner.lock().unwrap().clone();

                // Limit JSON batch per tick so large file transfers never block control/video
                let mut json_sent_this_tick = 0usize;
                while json_sent_this_tick < 8 {
                    match rx_out.try_recv() {
                        Ok(OutgoingMsg::Json(val)) => {
                            let _ = socket.send(tungstenite::Message::Text(val.to_string().into()));
                            json_sent_this_tick += 1;
                        }
                        Ok(OutgoingMsg::MouseMove(nx, ny)) => {
                            pending_mouse = Some((nx, ny));
                        }
                        Ok(OutgoingMsg::BinaryFrame(jpeg_bytes)) => {
                            pending_video = Some(jpeg_bytes);
                        }
                        Ok(OutgoingMsg::AudioChunk(b64_pcm)) => {
                            if !partner_now.is_empty() {
                                let audio_msg = json!({
                                    "type": "relay-input",
                                    "targetClean": partner_now,
                                    "event": {
                                        "action": "audio",
                                        "data": b64_pcm
                                    }
                                });
                                let _ = socket.send(tungstenite::Message::Text(audio_msg.to_string().into()));
                                json_sent_this_tick += 1;
                            }
                        }
                        Err(_) => break,
                    }
                }

                if let Some((nx, ny)) = pending_mouse {
                    if !partner_now.is_empty() {
                        let move_msg = json!({
                            "type": "relay-input",
                            "targetClean": partner_now,
                            "event": {
                                "action": "mousemove",
                                "normX": nx,
                                "normY": ny
                            }
                        });
                        let _ = socket.send(tungstenite::Message::Text(move_msg.to_string().into()));
                    }
                }

                if let Some(jpeg_bytes) = pending_video {
                    let _ = socket.send(tungstenite::Message::Binary(jpeg_bytes.into()));
                    video_in_flight = true;
                }

                // Flush socket: only release backpressure gate `can_send_frame` when TCP/TLS buffer is fully flushed!
                match socket.flush() {
                    Ok(()) => {
                        if video_in_flight {
                            video_in_flight = false;
                            can_send_frame.store(true, Ordering::Release);
                        }
                    }
                    Err(tungstenite::Error::Io(ref e))
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        // Still transmitting previous frame over network; keep can_send_frame = false so 0 frames queue up!
                    }
                    Err(_) => {
                        can_send_frame.store(true, Ordering::Release);
                    }
                }

                // 2. Drain ALL available incoming messages from socket before sleeping
                loop {
                    match socket.read() {
                        Ok(tungstenite::Message::Ping(payload)) => {
                            let _ = socket.send(tungstenite::Message::Pong(payload));
                            let _ = socket.flush();
                        }
                        Ok(tungstenite::Message::Binary(bin)) => {
                            if bin.len() > 1 && bin[0] == 0x02 {
                                voice_engine.play_pcm_chunk(&bin[1..]);
                            } else {
                                *latest_bin_frame.lock().unwrap() = Some(bin.to_vec());
                            }
                        }
                        Ok(tungstenite::Message::Text(text)) => {
                            let val: Value = match serde_json::from_str(&text) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };

                            let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");

                            match msg_type {
                                "registered-ack" => {
                                    logger::info("app.service", "Client registration acknowledged by relay.");
                                    if let Some(ref w_weak) = win_handle {
                                        let _ = w_weak.upgrade_in_event_loop(|win| {
                                            win.set_status_text("Online".into());
                                            win.set_is_relay_online(true);
                                        });
                                    }
                                }
                                "host-list" => {
                                    if let Some(ref w_weak) = win_handle {
                                        if let Some(arr) = val.get("hosts").and_then(|v| v.as_array()) {
                                            let local_fmt = format_desk_id(&local_id);
                                            let mut items = Vec::new();
                                            for h in arr {
                                                let desk_id = h
                                                    .get("desk_id")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("")
                                                    .to_string();
                                                if desk_id.is_empty() || desk_id == local_fmt {
                                                    continue;
                                                }
                                                let device_name = h
                                                    .get("device_name")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("Perangkat")
                                                    .to_string();
                                                let platform = h
                                                    .get("platform")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("pc")
                                                    .to_string();
                                                let online = h
                                                    .get("online")
                                                    .and_then(|v| v.as_bool())
                                                    .unwrap_or(false);
                                                let last_seen = h
                                                    .get("last_seen")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("-")
                                                    .to_string();
                                                items.push((desk_id, device_name, platform, online, last_seen));
                                            }
                                            let _ = w_weak.upgrade_in_event_loop(move |win| {
                                                let slint_hosts: Vec<HostItem> = items
                                                    .into_iter()
                                                    .map(|(desk_id, device_name, platform, online, last_seen)| {
                                                        HostItem {
                                                            desk_id: desk_id.into(),
                                                            device_name: device_name.into(),
                                                            platform: platform.into(),
                                                            online,
                                                            last_seen: last_seen.into(),
                                                        }
                                                    })
                                                    .collect();
                                                win.set_server_hosts(ModelRc::from(Rc::new(
                                                    VecModel::from(slint_hosts),
                                                )));
                                            });
                                        }
                                    }
                                }
                                "incoming-request" => {
                                    let from_clean = val
                                        .get("fromClean")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    *active_partner.lock().unwrap() = from_clean.clone();

                                    logger::info(
                                        "app.session",
                                        &format!("Auto-accepting incoming session from {}", from_clean),
                                    );

                                    let resp = json!({
                                        "type": "approval-response",
                                        "fromClean": from_clean,
                                        "accept": true
                                    });
                                    let _ = socket.send(tungstenite::Message::Text(resp.to_string().into()));
                                    let _ = socket.flush();
                                }
                                "session-connected" => {
                                    let target_clean = val
                                        .get("targetClean")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let role = val
                                        .get("role")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("viewer")
                                        .to_string();
                                    *active_partner.lock().unwrap() = target_clean.clone();
                                    last_clip_seq.store(
                                        injector.get_clipboard_sequence_number(),
                                        Ordering::SeqCst,
                                    );

                                    logger::info(
                                        "app.frontend_session",
                                        &format!("Session started: partner={} role={}", target_clean, role),
                                    );

                                    let formatted_partner = format_desk_id(&target_clean);
                                    let local_win_title = format!("OpenDesk - {}", format_desk_id(&local_id));
                                    privacy::reset_host_privacy();
                                    voice_engine.is_session_active.store(true, Ordering::SeqCst);
                                    // Default Mic ON (Aktif) automatically on every remote connection!
                                    voice_engine.is_mic_muted.store(false, Ordering::SeqCst);
                                    can_send_frame.store(true, Ordering::SeqCst);

                                    if role == "host" {
                                        privacy::set_viewer_hook_state(false, "", "");
                                        is_host_capturing.store(false, Ordering::SeqCst);
                                        thread::sleep(Duration::from_millis(20));
                                        active_monitor_idx.store(0, Ordering::SeqCst);
                                        is_host_capturing.store(true, Ordering::SeqCst);

                                        spawn_host_capture_loop(
                                            target_clean.clone(),
                                            Arc::clone(&is_host_capturing),
                                            Arc::clone(&active_monitor_idx),
                                            Arc::clone(&host_monitor_bounds),
                                            Arc::clone(&can_send_frame),
                                            tx_out_for_capture.clone(),
                                        );

                                        if let Some(ref w_weak) = win_handle {
                                            let _ = w_weak.upgrade_in_event_loop(move |win| {
                                                win.set_has_incoming(false);
                                                win.set_active_partner_id(formatted_partner.into());
                                                win.set_is_mic_muted(false);
                                                win.set_privacy_mode(0);
                                                win.set_block_remote_input(false);
                                                win.set_show_session_options(false);
                                                win.set_is_host(true);
                                                win.set_is_connected(true);
                                                win.set_status_text("Sesi Aktif".into());
                                            });
                                        }
                                    } else {
                                        privacy::set_viewer_hook_state(
                                            true,
                                            &target_clean,
                                            &local_win_title,
                                        );
                                        if let Some(ref w_weak) = win_handle {
                                            let _ = w_weak.upgrade_in_event_loop(move |win| {
                                                win.set_has_incoming(false);
                                                win.set_active_partner_id(formatted_partner.into());
                                                win.set_has_remote_frame(false);
                                                win.set_is_mic_muted(false);
                                                win.set_privacy_mode(0);
                                                win.set_block_remote_input(false);
                                                win.set_show_session_options(false);
                                                win.set_is_host(false);
                                                win.set_is_connected(true);
                                                win.set_status_text("Menghubungkan...".into());
                                            });
                                        }
                                    }
                                }
                                "frame" => {
                                    if let Some(b64) = val.get("data").and_then(|v| v.as_str()) {
                                        *latest_b64_frame.lock().unwrap() = Some(b64.to_string());
                                    }
                                }
                                "input-command" => {
                                    if let Some(event) = val.get("event") {
                                        let action = event
                                            .get("action")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");

                                        if action == "audio" {
                                            if let Some(b64) = event.get("data").and_then(|v| v.as_str()) {
                                                if let Some(pcm) = from_base64(b64) {
                                                    voice_engine.play_pcm_chunk(&pcm);
                                                }
                                            }
                                            continue;
                                        }

                                        if action == "set-mic" {
                                            let muted = event.get("muted").and_then(|v| v.as_bool()).unwrap_or(false);
                                            voice_engine.is_mic_muted.store(muted, Ordering::SeqCst);
                                            if let Some(ref w_weak) = win_handle {
                                                let _ = w_weak.upgrade_in_event_loop(move |win| {
                                                    win.set_is_mic_muted(muted);
                                                });
                                            }
                                            continue;
                                        }

                                        if action == "switch-monitor" {
                                            active_monitor_idx.fetch_add(1, Ordering::SeqCst);
                                            continue;
                                        }

                                        if action == "set-privacy" {
                                            let mode = event.get("mode").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                            privacy::set_host_privacy_mode(mode.clamp(0, 3));
                                            continue;
                                        }

                                        if action == "set-block-input" {
                                            let blocked = event.get("blocked").and_then(|v| v.as_bool()).unwrap_or(false);
                                            privacy::set_host_block_input(blocked);
                                            continue;
                                        }

                                        if action == "shortcut" {
                                            let sc = event.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                            match sc {
                                                "taskmgr" => {
                                                    injector.send_key_combo(true, false, true, 0x1B); // Ctrl+Shift+Esc
                                                }
                                                "win" => {
                                                    injector.key_tap(0x5B); // VK_LWIN
                                                }
                                                "explorer" => {
                                                    injector.key_down(0x5B);
                                                    injector.key_tap(b'E');
                                                    injector.key_up(0x5B);
                                                }
                                                "desktop" => {
                                                    injector.key_down(0x5B);
                                                    injector.key_tap(b'D');
                                                    injector.key_up(0x5B);
                                                }
                                                _ => {}
                                            }
                                            continue;
                                        }

                                        if action == "clipboard" {
                                            if let Some(txt) = event.get("text").and_then(|v| v.as_str()) {
                                                injector.set_clipboard_text(txt);
                                                last_clip_seq.store(
                                                    injector.get_clipboard_sequence_number(),
                                                    Ordering::SeqCst,
                                                );
                                            }
                                            continue;
                                        }

                                        let mon = *host_monitor_bounds.lock().unwrap();
                                        let x = if let Some(nx) = event.get("normX").and_then(|v| v.as_f64()) {
                                            mon.left + (nx.clamp(0.0, 1.0) * mon.width as f64).round() as i32
                                        } else {
                                            event.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32
                                        };
                                        let y = if let Some(ny) = event.get("normY").and_then(|v| v.as_f64()) {
                                            mon.top + (ny.clamp(0.0, 1.0) * mon.height as f64).round() as i32
                                        } else {
                                            event.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32
                                        };

                                        let btn = event.get("button").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                        let key_code = (event.get("keyCode").or_else(|| event.get("vk")))
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0) as u8;
                                        let delta_y = event.get("deltaY").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                        let ctrl = event.get("ctrl").and_then(|v| v.as_bool()).unwrap_or(false);
                                        let alt = event.get("alt").and_then(|v| v.as_bool()).unwrap_or(false);
                                        let shift = event.get("shift").and_then(|v| v.as_bool()).unwrap_or(false);

                                        match action {
                                            "mousemove" => injector.set_cursor_pos(x, y),
                                            "mousedown" => {
                                                injector.set_cursor_pos(x, y);
                                                injector.mouse_down(btn);
                                            }
                                            "mouseup" | "click" => {
                                                injector.set_cursor_pos(x, y);
                                                if action == "click" {
                                                    injector.mouse_down(btn);
                                                }
                                                injector.mouse_up(btn);
                                            }
                                            "dblclick" => {
                                                injector.set_cursor_pos(x, y);
                                                injector.mouse_down(btn);
                                                injector.mouse_up(btn);
                                                injector.mouse_down(btn);
                                                injector.mouse_up(btn);
                                            }
                                            "wheel" => {
                                                injector.set_cursor_pos(x, y);
                                                injector.mouse_wheel(delta_y);
                                            }
                                            "text" => {
                                                if let Some(txt) = event.get("text").and_then(|v| v.as_str()) {
                                                    injector.send_unicode_text(txt);
                                                }
                                            }
                                            "keycombo" => {
                                                if key_code > 0 {
                                                    injector.send_key_combo(ctrl, alt, shift, key_code);
                                                }
                                            }
                                            "keypress" => {
                                                if key_code > 0 {
                                                    injector.key_tap(key_code);
                                                }
                                            }
                                            "keydown" => {
                                                if key_code > 0 {
                                                    injector.key_down(key_code);
                                                }
                                            }
                                            "keyup" => {
                                                if key_code > 0 {
                                                    injector.key_up(key_code);
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                "clipboard-sync" => {
                                    if let Some(text) = val.get("text").and_then(|v| v.as_str()) {
                                        injector.set_clipboard_text(text);
                                        last_clip_seq.store(
                                            injector.get_clipboard_sequence_number(),
                                            Ordering::SeqCst,
                                        );
                                    }
                                }
                                "file-transfer" => {
                                    if let Some(payload) = val.get("payload") {
                                        if let Some(status_msg) = file_receiver.handle_payload(
                                            payload,
                                            &injector,
                                            &last_clip_seq,
                                        ) {
                                            if let Some(ref w_weak) = win_handle {
                                                let _ = w_weak.upgrade_in_event_loop(move |win| {
                                                    win.set_status_text(status_msg.into());
                                                });
                                            }
                                        }
                                    }
                                }
                                "session-closed" => {
                                    is_host_capturing.store(false, Ordering::SeqCst);
                                    privacy::reset_host_privacy();
                                    privacy::set_viewer_hook_state(false, "", "");
                                    voice_engine.is_session_active.store(false, Ordering::SeqCst);
                                    voice_engine.is_mic_muted.store(true, Ordering::SeqCst);
                                    *active_partner.lock().unwrap() = String::new();
                                    logger::info("app.frontend_session", "Session closed.");
                                    if let Some(ref w_weak) = win_handle {
                                        let _ = w_weak.upgrade_in_event_loop(|win| {
                                            win.set_is_connected(false);
                                            win.set_is_host(false);
                                            win.set_is_mic_muted(true);
                                            win.set_privacy_mode(0);
                                            win.set_block_remote_input(false);
                                            win.set_show_session_options(false);
                                            win.set_has_remote_frame(false);
                                            win.set_status_text("Online".into());
                                        });
                                    }
                                }
                                "error" => {
                                    let msg = val
                                        .get("message")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("Error")
                                        .to_string();
                                    logger::warn("app.frontend_session", &format!("Relay error: {}", msg));
                                    if let Some(ref w_weak) = win_handle {
                                        let _ = w_weak.upgrade_in_event_loop(move |win| {
                                            win.set_status_text(msg.into());
                                        });
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(tungstenite::Error::Io(ref e))
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut =>
                        {
                            break;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            logger::warn("anynet.any_socket", &format!("Socket disconnected: {}", e));
                            is_host_capturing.store(false, Ordering::SeqCst);
                            privacy::reset_host_privacy();
                            privacy::set_viewer_hook_state(false, "", "");
                            voice_engine.is_session_active.store(false, Ordering::SeqCst);
                            voice_engine.is_mic_muted.store(true, Ordering::SeqCst);
                            if let Some(ref w_weak) = win_handle {
                                let _ = w_weak.upgrade_in_event_loop(|win| {
                                    win.set_is_relay_online(false);
                                    win.set_is_mic_muted(true);
                                    win.set_privacy_mode(0);
                                    win.set_block_remote_input(false);
                                    win.set_show_session_options(false);
                                    win.set_status_text("Terputus".into());
                                });
                            }
                            disconnected = true;
                            break;
                        }
                    }
                }

                if disconnected {
                    thread::sleep(Duration::from_millis(1500));
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    });
}
