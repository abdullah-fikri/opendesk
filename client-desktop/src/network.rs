use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
use crate::file_transfer::{
    abort_pending_sends, deliver_ack, is_received_clipboard, offer_files, send_files_async, serve_pull, FileReceiver, IN_FLIGHT_FILE_CHUNKS, LAST_FILE_RECV_MS, PEER_FILE_ACK, PEER_FILE_OFFER,
};
use crate::logger;
use crate::p2p::{self, P2p, P2P_ACTIVE};
use crate::privacy;
use crate::protocol::OutgoingMsg;
use crate::session::{decode_b64_to_pixel_buffer, spawn_host_capture_loop, VideoInbox, ViewerDecoder, ViewerOutput, CAP_VIDEO_DELTA};
use crate::{HostItem, MainWindow};

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

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
    host_resolution_mode: Arc<AtomicUsize>,
    voice_engine: Arc<VoiceEngine>,
    remote_resolution: Arc<Mutex<(i32, i32)>>,
    last_clip_seq: Arc<AtomicU32>,
    rx_out: Receiver<OutgoingMsg>,
    tx_out_for_capture: Sender<OutgoingMsg>,
) {
    let latest_b64_frame: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let latest_bin_frame: Arc<VideoInbox> = Arc::new(VideoInbox::default());

    let decode_b64_slot = Arc::clone(&latest_b64_frame);
    let decode_bin_slot = Arc::clone(&latest_bin_frame);
    let win_decode = win_handle.clone();
    let res_decode = Arc::clone(&remote_resolution);
    let key_partner = Arc::clone(&active_partner);
    let key_tx = tx_out_for_capture.clone();

    let initial_monitors = enumerate_monitors();
    let host_monitor_bounds = Arc::new(Mutex::new(initial_monitors[0]));
    let can_send_frame = Arc::new(AtomicBool::new(true));

    // Automatic Background Clipboard Watcher + Live CLI Config Reload Watcher:
    let clip_inj = Arc::clone(&injector);
    let clip_partner = Arc::clone(&active_partner);
    let clip_seq_tracker = Arc::clone(&last_clip_seq);
    let clip_tx = tx_out_for_capture.clone();
    let clip_win = win_handle.clone();
    let clip_is_host = Arc::clone(&is_host_capturing);
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

                // Copy on the host (remote Ctrl+C) sends the files to the viewer's clipboard. The viewer side
                // sends its files explicitly on Ctrl+V, so it never auto-uploads here (that would send twice).
                // Files we received ourselves are skipped by provenance, which stops the echo loop.
                // AnyDesk-style: copied files (either side) are only offered; the partner pulls them on paste.
                if PEER_FILE_OFFER.load(Ordering::SeqCst) {
                    let files = clip_inj.get_clipboard_files();
                    if !files.is_empty() {
                        // Sets its own status; folders are walked off this thread.
                        if !is_received_clipboard(&files) {
                            offer_files(files, partner, clip_tx.clone(), clip_win.clone());
                        }
                        continue;
                    }
                } else if clip_is_host.load(Ordering::SeqCst) {
                    let files = clip_inj.get_clipboard_files();
                    if !files.is_empty() {
                        if !is_received_clipboard(&files) {
                            logger::info(
                                "app.clip_comp",
                                &format!("Host file copy detected ({} files); sending to viewer clipboard.", files.len()),
                            );
                            send_files_async(files, partner, clip_tx.clone(), clip_win.clone(), false);
                        }
                        continue;
                    }
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
            let mut viewer = ViewerDecoder::new();
            let mut last_key_request: Option<Instant> = None;

            loop {
                let batch = decode_bin_slot.drain();
                let b64_opt = if batch.is_empty() {
                    decode_b64_slot.lock().unwrap().take()
                } else {
                    None
                };

                let decoded = if !batch.is_empty() {
                    match viewer.process(batch) {
                        ViewerOutput::Frame(buf, w, h) => Some((buf, w, h)),
                        ViewerOutput::Unchanged => None,
                        ViewerOutput::NeedKey => {
                            // Lost or undecodable delta: ask the host for a key frame, at most every 300 ms.
                            if last_key_request.is_none_or(|t| t.elapsed() >= Duration::from_millis(300)) {
                                last_key_request = Some(Instant::now());
                                let partner = key_partner.lock().unwrap().clone();
                                if !partner.is_empty() {
                                    logger::info("desk_rt.video", "Delta chain broken; requesting key frame.");
                                    let _ = key_tx.send(OutgoingMsg::Json(json!({
                                        "type": "relay-input",
                                        "targetClean": partner,
                                        "event": { "action": "video-keyframe" }
                                    })));
                                }
                            }
                            None
                        }
                    }
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
                    let file_busy = IN_FLIGHT_FILE_CHUNKS.load(Ordering::Relaxed) > 0
                        || now_epoch_ms().saturating_sub(LAST_FILE_RECV_MS.load(Ordering::Relaxed)) < 2000;
                    let path = if P2P_ACTIVE.load(Ordering::Relaxed) { "P2P" } else { "Relay" };
                    let _ = win_dec_weak.upgrade_in_event_loop(move |win| {
                        win.set_remote_frame(Image::from_rgba8(buf));
                        win.set_has_remote_frame(true);
                        if !win.get_is_host() && win.get_is_connected() && !file_busy {
                            win.set_status_text(
                                format!("{}x{} • {} FPS • {}", w, h, fps_display, path).into(),
                            );
                        }
                    });
                }
            }
        });
    }

    thread::spawn(move || {
        let mut file_receiver = FileReceiver::new();
        let mut file_queue: VecDeque<(Value, Option<String>)> = VecDeque::new();
        // Input that arrived over P2P, already wrapped as `input-command` JSON by the p2p module.
        let (p2p_in_tx, p2p_in_rx) = mpsc::channel::<String>();

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
            let mut caps = vec!["file-ack", "file-offer", crate::file_transfer::CAP_FILE_FOLDER, "cursor-shape"];
            caps.extend(crate::session::video_caps());
            if p2p::enabled() {
                caps.push(p2p::CAP);
            }
            let reg_msg = json!({
                "type": "register",
                "deskId": local_id,
                "platform": platform_tag,
                "deviceName": get_device_display_name(),
                "deviceToken": std::env::var("OPENDESK_DEVICE_TOKEN").unwrap_or_else(|_| crate::config::load_config().device_token),
                "isAdmin": cfg!(feature = "admin"),
                "caps": caps
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
                    win.set_status_text("Mendaftarkan perangkat...".into());
                    win.set_is_relay_online(false);
                });
            }

            can_send_frame.store(true, Ordering::SeqCst);
            let mut disconnected = false;
            let mut video_in_flight = false;
            let mut last_expire = Instant::now();
            let mut server_p2p = false;
            let mut server_offer = false;
            let mut p2p_link: Option<Arc<P2p>> = None;

            while !reconnect_signal.load(Ordering::SeqCst) && !disconnected {
                // 1. Drain outgoing queue: coalesce high-frequency MouseMove and VideoFrames,
                // send Control JSON & Binary Audio immediately, and queue FileTransfer separately.
                let mut pending_video: Option<Vec<u8>> = None;
                let mut pending_mouse: Option<(f64, f64)> = None;
                let partner_now = active_partner.lock().unwrap().clone();

                // The P2P path lives exactly as long as the session it was opened for.
                if p2p_link.as_ref().is_some_and(|p| p.partner != partner_now) {
                    if let Some(p) = p2p_link.take() {
                        p.close();
                    }
                    while p2p_in_rx.try_recv().is_ok() {}
                }
                let direct = p2p_link.as_ref().filter(|p| p.is_active()).cloned();

                loop {
                    match rx_out.try_recv() {
                        Ok(OutgoingMsg::Json(val)) => {
                            let p2p_sent = direct.as_ref().is_some_and(|p| {
                                val.get("type").and_then(|t| t.as_str()) == Some("relay-input")
                                    && val.get("targetClean").and_then(|t| t.as_str()) == Some(p.partner.as_str())
                                    && val.get("event").is_some_and(|ev| p.send_input(ev))
                            });
                            if !p2p_sent {
                                let _ = socket.send(tungstenite::Message::Text(val.to_string().into()));
                            }
                        }
                        Ok(OutgoingMsg::MouseMove(nx, ny)) => {
                            pending_mouse = Some((nx, ny));
                        }
                        Ok(OutgoingMsg::BinaryFrame(jpeg_bytes)) => {
                            pending_video = Some(jpeg_bytes);
                        }
                        Ok(OutgoingMsg::AudioChunk(pcm_pkt)) => {
                            if !partner_now.is_empty() && !direct.as_ref().is_some_and(|p| p.send_audio(&pcm_pkt)) {
                                let _ = socket.send(tungstenite::Message::Binary(pcm_pkt.into()));
                            }
                        }
                        Ok(OutgoingMsg::FileTransfer { msg, status_text }) => {
                            file_queue.push_back((msg, status_text));
                        }
                        Err(_) => break,
                    }
                }

                if let Some((nx, ny)) = pending_mouse {
                    if !partner_now.is_empty() {
                        let event = json!({
                            "action": "mousemove",
                            "normX": nx,
                            "normY": ny
                        });
                        if !direct.as_ref().is_some_and(|p| p.send_input(&event)) {
                            let move_msg = json!({
                                "type": "relay-input",
                                "targetClean": partner_now,
                                "event": event
                            });
                            let _ = socket.send(tungstenite::Message::Text(move_msg.to_string().into()));
                        }
                    }
                }

                if let Some(jpeg_bytes) = pending_video {
                    // Over P2P the viewer's frame-ack releases can_send_frame instead of the socket flush.
                    if !direct.as_ref().is_some_and(|p| p.send_video(&jpeg_bytes)) {
                        let _ = socket.send(tungstenite::Message::Binary(jpeg_bytes.into()));
                        video_in_flight = true;
                    }
                }

                // Drain background file transfer queue:
                // - Up to 8 chunks/tick during active screen session (keeps video & audio latency low)
                // - Up to 64 chunks/tick when screen session is disconnected (full-speed background transfer!)
                let max_file_chunks = if partner_now.is_empty() { 64 } else { 8 };
                let mut latest_file_status: Option<String> = None;
                for _ in 0..max_file_chunks {
                    let Some((f_msg, f_status)) = file_queue.pop_front() else {
                        break;
                    };
                    // WouldBlock means tungstenite buffered the frame; any other error leaves it queued
                    // until the read loop notices the dead socket and fails the transfer.
                    match socket.send(tungstenite::Message::Text(f_msg.to_string().into())) {
                        Ok(()) => {}
                        Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => {
                            file_queue.push_front((f_msg, f_status));
                            break;
                        }
                    }
                    IN_FLIGHT_FILE_CHUNKS.fetch_sub(1, Ordering::Relaxed);
                    if f_status.is_some() {
                        latest_file_status = f_status;
                    }
                }
                if last_expire.elapsed() >= Duration::from_secs(1) {
                    last_expire = Instant::now();
                    if let Some(st) = file_receiver.expire_stale(last_expire) {
                        latest_file_status = Some(st);
                    }
                }
                if let Some(st) = latest_file_status {
                    if let Some(ref w_weak) = win_handle {
                        let _ = w_weak.upgrade_in_event_loop(move |win| {
                            win.set_status_text(st.into());
                        });
                    }
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
                    let incoming = match p2p_in_rx.try_recv() {
                        Ok(text) => Ok(tungstenite::Message::Text(text.into())),
                        Err(_) => socket.read(),
                    };
                    match incoming {
                        Ok(tungstenite::Message::Ping(payload)) => {
                            let _ = socket.send(tungstenite::Message::Pong(payload));
                            let _ = socket.flush();
                        }
                        Ok(tungstenite::Message::Binary(bin)) => {
                            if bin.len() > 1 && bin[0] == 0x02 {
                                voice_engine.play_binary_packet(&bin);
                            } else {
                                latest_bin_frame.push(bin.to_vec());
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
                                    server_p2p = val
                                        .get("caps")
                                        .and_then(|v| v.as_array())
                                        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some("p2p-signal")));
                                    // Older relays drop offer/pull, so clipboard offers need relay support too.
                                    server_offer = val
                                        .get("caps")
                                        .and_then(|v| v.as_array())
                                        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some("file-offer")));
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
                                    let peer_acks = val
                                        .get("peerCaps")
                                        .and_then(|v| v.as_array())
                                        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some("file-ack")));
                                    PEER_FILE_ACK.store(peer_acks, Ordering::SeqCst);
                                    let peer_cursor = val
                                        .get("peerCaps")
                                        .and_then(|v| v.as_array())
                                        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some("cursor-shape")));
                                    crate::session::PEER_CURSOR_SHAPE.store(peer_cursor, Ordering::SeqCst);
                                    let peer_delta = val
                                        .get("peerCaps")
                                        .and_then(|v| v.as_array())
                                        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some(CAP_VIDEO_DELTA)));
                                    crate::session::PEER_VIDEO_DELTA.store(peer_delta, Ordering::SeqCst);
                                    let peer_h264 = val
                                        .get("peerCaps")
                                        .and_then(|v| v.as_array())
                                        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some(crate::session::CAP_VIDEO_H264)));
                                    crate::session::PEER_VIDEO_H264.store(peer_h264, Ordering::SeqCst);
                                    let peer_offer = val
                                        .get("peerCaps")
                                        .and_then(|v| v.as_array())
                                        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some("file-offer")));
                                    crate::file_transfer::PEER_FILE_OFFER.store(peer_offer && server_offer, Ordering::SeqCst);
                                    // Folder entries ride on existing start/offer messages, so only the peer must understand them.
                                    let peer_folder = val
                                        .get("peerCaps")
                                        .and_then(|v| v.as_array())
                                        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some(crate::file_transfer::CAP_FILE_FOLDER)));
                                    crate::file_transfer::PEER_FILE_FOLDER.store(peer_folder, Ordering::SeqCst);

                                    if let Some(old) = p2p_link.take() {
                                        old.close();
                                    }
                                    let peer_p2p = val
                                        .get("peerCaps")
                                        .and_then(|v| v.as_array())
                                        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some(p2p::CAP)));
                                    if server_p2p && peer_p2p && p2p::enabled() {
                                        p2p_link = P2p::start(
                                            role == "host",
                                            &local_id,
                                            target_clean.clone(),
                                            Arc::clone(&can_send_frame),
                                            tx_out_for_capture.clone(),
                                            p2p::Sinks {
                                                video: Arc::clone(&latest_bin_frame),
                                                voice: Arc::clone(&voice_engine),
                                                input: p2p_in_tx.clone(),
                                            },
                                        );
                                    }

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
                                            Arc::clone(&host_resolution_mode),
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
                                        // Viewer informs host of preferred resolution & audio quality upon connection.
                                        // Automatic mode uses the last canvas size; the new canvas re-reports it if it differs.
                                        let res_mode = crate::session::VIEWER_RES_MODE.load(Ordering::SeqCst);
                                        let viewport = crate::session::VIEWPORT_PX.load(Ordering::SeqCst);
                                        let res_event = crate::session::resolution_event(res_mode, crate::session::unpack_box(viewport));
                                        let sent_box = if res_event.get("auto").is_some() { viewport } else { 0 };
                                        crate::session::LAST_SENT_BOX.store(sent_box, Ordering::SeqCst);
                                        let current_aq = voice_engine.audio_quality.load(Ordering::SeqCst);
                                        let init_res_msg = json!({
                                            "type": "relay-input",
                                            "targetClean": target_clean,
                                            "event": res_event
                                        });
                                        let _ = tx_out_for_capture.send(OutgoingMsg::Json(init_res_msg));
                                        let init_aq_msg = json!({
                                            "type": "relay-input",
                                            "targetClean": target_clean,
                                            "event": {
                                                "action": "set-audio-quality",
                                                "quality": current_aq
                                            }
                                        });
                                        let _ = tx_out_for_capture.send(OutgoingMsg::Json(init_aq_msg));

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

                                        // Host -> viewer only: the viewer shows its own cursor in the host's current shape.
                                        if action == "cursor-shape" {
                                            if !is_host_capturing.load(Ordering::SeqCst) {
                                                let shape = event.get("shape").and_then(|v| v.as_i64()).unwrap_or(1) as i32;
                                                if let Some(ref w_weak) = win_handle {
                                                    let _ = w_weak.upgrade_in_event_loop(move |win| win.set_remote_cursor(shape));
                                                }
                                            }
                                            continue;
                                        }

                                        // Viewer lost a delta frame; the capture loop answers with a key frame.
                                        if action == "video-keyframe" {
                                            if is_host_capturing.load(Ordering::SeqCst) {
                                                crate::session::KEYFRAME_REQUESTED.store(true, Ordering::SeqCst);
                                            }
                                            continue;
                                        }

                                        if action == "set-mic" {
                                            // A remote peer may silence this mic, but only the local user can enable it.
                                            if event.get("muted").and_then(|v| v.as_bool()) == Some(true) {
                                                voice_engine.is_mic_muted.store(true, Ordering::SeqCst);
                                                if let Some(ref w_weak) = win_handle {
                                                    let _ = w_weak.upgrade_in_event_loop(|win| {
                                                        win.set_is_mic_muted(true);
                                                    });
                                                }
                                            }
                                            continue;
                                        }

                                        if action == "switch-monitor" {
                                            active_monitor_idx.fetch_add(1, Ordering::SeqCst);
                                            continue;
                                        }

                                        if action == "set-resolution" {
                                            // `auto` + width/height: stream at the viewer's canvas size (newer viewers only).
                                            let mode = crate::session::mode_from_resolution_event(event);
                                            host_resolution_mode.store(mode, Ordering::SeqCst);
                                            logger::info(
                                                "app.frontend_session",
                                                &format!(
                                                    "Viewer requested resolution mode: {}",
                                                    if event.get("auto").is_some() { format!("auto {}x{}", event["width"], event["height"]) } else { mode.to_string() }
                                                ),
                                            );
                                            continue;
                                        }

                                        if action == "set-audio-quality" {
                                            let q = event.get("quality").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
                                            let safe_q = q.min(2);
                                            voice_engine.audio_quality.store(safe_q, Ordering::SeqCst);
                                            if let Some(ref w_weak) = win_handle {
                                                let _ = w_weak.upgrade_in_event_loop(move |win| {
                                                    win.set_audio_quality(safe_q as i32);
                                                });
                                            }
                                            logger::info(
                                                "app.frontend_session",
                                                &format!("Partner set voice audio quality: {}", safe_q),
                                            );
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
                                            // Tracked so a session end releases whatever the viewer still held.
                                            "keydown" => {
                                                if key_code > 0 {
                                                    crate::keymap::host_key_down(&injector, key_code);
                                                }
                                            }
                                            "keyup" => {
                                                if key_code > 0 {
                                                    crate::keymap::host_key_up(&injector, key_code);
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                "p2p-signal" => {
                                    let from = val.get("fromClean").and_then(|v| v.as_str()).unwrap_or("");
                                    if let (Some(p), Some(cand)) = (p2p_link.as_ref(), val.get("candidate").and_then(|v| v.as_str())) {
                                        if p.partner == from {
                                            p.on_signal(cand);
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
                                    // fromClean is stamped by the relay, never taken from the payload.
                                    // Relays built before file grants do not stamp it; attribute the transfer to the session partner
                                    // (outside a session `from` stays empty and the payload is dropped).
                                    let from = val
                                        .get("fromClean")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string)
                                        .unwrap_or_else(|| active_partner.lock().unwrap().clone());
                                    if let (Some(payload), false) = (val.get("payload"), from.is_empty()) {
                                        let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
                                        let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        if action == "ack" {
                                            deliver_ack(&id, payload.get("ok").and_then(|v| v.as_bool()) == Some(true));
                                            continue;
                                        }
                                        if action == "offer" || action == "pull" {
                                            let status = if action == "offer" {
                                                crate::virtual_clip::set_offer(&from, payload, tx_out_for_capture.clone())
                                            } else {
                                                serve_pull(&from, payload, &tx_out_for_capture, win_handle.clone());
                                                None
                                            };
                                            if let (Some(st), Some(w_weak)) = (status, win_handle.as_ref()) {
                                                let _ = w_weak.upgrade_in_event_loop(move |win| win.set_status_text(st.into()));
                                            }
                                            continue;
                                        }
                                        let allow_auto_paste = !active_partner.lock().unwrap().is_empty();
                                        let (status_msg, ack) = file_receiver.handle_payload(
                                            payload,
                                            &from,
                                            &injector,
                                            &last_clip_seq,
                                            allow_auto_paste,
                                        );
                                        if let Some(ok) = ack {
                                            let ack_msg = json!({
                                                "type": "relay-file",
                                                "targetClean": from,
                                                "payload": { "action": "ack", "id": id, "ok": ok }
                                            });
                                            let _ = socket.send(tungstenite::Message::Text(ack_msg.to_string().into()));
                                        }
                                        if let (Some(status_msg), Some(w_weak)) = (status_msg, win_handle.as_ref()) {
                                            let _ = w_weak.upgrade_in_event_loop(move |win| {
                                                win.set_status_text(status_msg.into());
                                            });
                                        }
                                    }
                                }
                                "session-closed" => {
                                    crate::virtual_clip::clear_offer(&active_partner.lock().unwrap());
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
                            // Relay grants die with this connection, so every transfer on it has failed.
                            abort_pending_sends();
                            crate::virtual_clip::abort_pulls();
                            crate::virtual_clip::clear_offer("");
                            for _ in file_queue.drain(..) {
                                IN_FLIGHT_FILE_CHUNKS.fetch_sub(1, Ordering::Relaxed);
                            }
                            let recv_status = file_receiver.abort_all();
                            *active_partner.lock().unwrap() = String::new();
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
                                    win.set_is_connected(false);
                                    win.set_is_host(false);
                                    win.set_has_remote_frame(false);
                                    win.set_status_text(recv_status.unwrap_or_else(|| "Terputus".into()).into());
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
            // The relay owns session authorization, so a lost relay connection also ends the direct path.
            if let Some(p) = p2p_link.take() {
                p.close();
            }
        }
    });
}
