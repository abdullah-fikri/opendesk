#![windows_subsystem = "windows"]

mod audio;
mod config;
mod daemon;
mod dragdrop;
mod file_transfer;
mod logger;
mod network;
mod privacy;
mod protocol;
mod session;
mod tray;

use std::env;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use audio::VoiceEngine;
use config::{
    clean_desk_id, format_desk_id, load_config, load_history, normalize_gateway_url, save_config,
    save_history, HistoryRecord,
};
use core_engine::capture::{enable_per_monitor_dpi_awareness, GdiScreenCapturer};
use core_engine::input::InputInjector;
use network::spawn_network_worker;
use protocol::OutgoingMsg;
use serde_json::json;
use slint::{ComponentHandle, ModelRc, VecModel};
use tray::{bring_main_window_to_front, SystemTray, TrayAction};

slint::include_modules!();

fn to_slint_history_items(list: &[HistoryRecord]) -> Vec<HistoryItem> {
    list.iter()
        .map(|r| HistoryItem {
            desk_id: r.desk_id.clone().into(),
            alias: r.alias.clone().into(),
            last_connected: r.last_connected.clone().into(),
        })
        .collect()
}

/// Maps Slint key event string to Win32 Virtual-Key code when it's a control/navigation key.
fn map_special_key_to_vk(text: &str) -> Option<u16> {
    let ch = text.chars().next()?;
    match ch {
        '\u{0008}' => Some(0x08), // Backspace
        '\u{0009}' => Some(0x09), // Tab
        '\r' | '\n' => Some(0x0D), // Enter
        '\u{001b}' => Some(0x1B), // Escape
        '\u{007f}' => Some(0x2E), // Delete
        '\u{f700}' => Some(0x26), // UpArrow
        '\u{f701}' => Some(0x28), // DownArrow
        '\u{f702}' => Some(0x25), // LeftArrow
        '\u{f703}' => Some(0x27), // RightArrow
        '\u{f729}' => Some(0x24), // Home
        '\u{f72b}' => Some(0x23), // End
        '\u{f72c}' => Some(0x21), // PageUp
        '\u{f72d}' => Some(0x22), // PageDown
        '\u{f727}' => Some(0x2D), // Insert
        '\u{f704}' => Some(0x70), // F1
        '\u{f705}' => Some(0x71), // F2
        '\u{f706}' => Some(0x72), // F3
        '\u{f707}' => Some(0x73), // F4
        '\u{f708}' => Some(0x74), // F5
        '\u{f709}' => Some(0x75), // F6
        '\u{f70a}' => Some(0x76), // F7
        '\u{f70b}' => Some(0x77), // F8
        '\u{f70c}' => Some(0x78), // F9
        '\u{f70d}' => Some(0x79), // F10
        '\u{f70e}' => Some(0x7A), // F11
        '\u{f70f}' => Some(0x7B), // F12
        _ => None,
    }
}

fn main() -> Result<(), slint::PlatformError> {
    // 1. Handle terminal CLI commands first (`opendesk server --wss://...`, `opendesk status`, `opendesk id`)
    daemon::handle_cli_commands();

    enable_per_monitor_dpi_awareness();

    let mut cfg = load_config();
    let mut start_in_background = false;

    for arg in env::args().skip(1) {
        if arg.starts_with("--id=") {
            cfg.desk_id = arg.replace("--id=", "");
        } else if arg == "--background" || arg == "--minimized" {
            start_in_background = true;
        }
    }

    // Enforce Single-Instance Lock: if already running in tray, wake existing window & exit duplicate process
    daemon::ensure_single_instance(start_in_background || cfg!(feature = "headless"));

    logger::info("main", "* OpenDesk Windows Startup *");
    logger::info(
        "main",
        &format!(
            "* Version 2.5.0 (role: {})",
            if cfg!(feature = "headless") {
                "headless-host"
            } else if cfg!(feature = "admin") {
                "admin"
            } else {
                "standard-user"
            }
        ),
    );

    // Register boot autostart + CLI PATH shim + 10-second Watchdog Guardian (auto-restarts if killed in Task Manager)
    daemon::init_background_guardian();

    let init_capturer = GdiScreenCapturer::new();
    let local_w = init_capturer.screen_width;
    let local_h = init_capturer.screen_height;
    drop(init_capturer);
    logger::info(
        "desk_rt.d3d_view",
        &format!("Current display mode: {}x{}, 60 Hz", local_w, local_h),
    );

    let current_gateway = Arc::new(Mutex::new(cfg.gateway_url.clone()));
    let reconnect_signal = Arc::new(AtomicBool::new(false));
    let injector = Arc::new(InputInjector::new());
    let active_partner = Arc::new(Mutex::new(String::new()));
    let is_host_capturing = Arc::new(AtomicBool::new(false));
    let active_monitor_idx = Arc::new(AtomicUsize::new(0));
    let remote_resolution = Arc::new(Mutex::new((local_w, local_h)));
    let last_clip_seq = Arc::new(AtomicU32::new(injector.get_clipboard_sequence_number()));

    let (tx_out, rx_out) = mpsc::channel::<OutgoingMsg>();
    let tx_out_clone = tx_out.clone();
    let voice_engine = VoiceEngine::new(tx_out.clone());
    privacy::init_privacy_and_hooks(tx_out.clone());

    // -------------------------------------------------------------------------
    // HEADLESS / ZERO-GUI HOST MODE (`make build-windows-host`)
    // Strictly incoming-only background daemon: no window, no taskbar, no tray.
    // -------------------------------------------------------------------------
    if cfg!(feature = "headless") {
        logger::info(
            "main.headless",
            &format!(
                "Running in Zero-GUI Headless Host mode. Desk ID: {} | Gateway: {}",
                format_desk_id(&cfg.desk_id),
                cfg.gateway_url
            ),
        );
        spawn_network_worker(
            None,
            cfg.desk_id.clone(),
            Arc::clone(&current_gateway),
            Arc::clone(&reconnect_signal),
            Arc::clone(&injector),
            Arc::clone(&active_partner),
            Arc::clone(&is_host_capturing),
            Arc::clone(&active_monitor_idx),
            Arc::clone(&voice_engine),
            Arc::clone(&remote_resolution),
            Arc::clone(&last_clip_seq),
            rx_out,
            tx_out.clone(),
        );

        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    // -------------------------------------------------------------------------
    // GUI MODE (Standard User `make build-windows` or Admin `make build-windows-admin`)
    // -------------------------------------------------------------------------
    let main_window = MainWindow::new()?;

    let formatted_local = format_desk_id(&cfg.desk_id);
    let win_title = format!("OpenDesk - {}", formatted_local);
    main_window.set_local_desk_id(formatted_local.clone().into());
    main_window.set_gateway_url(cfg.gateway_url.clone().into());
    main_window.set_status_text("Menghubungkan...".into());
    main_window.set_is_relay_online(false);
    main_window.set_is_admin(cfg!(feature = "admin"));
    main_window.set_active_dashboard_tab(if cfg!(feature = "admin") { 1 } else { 0 });

    // Intercept window close (X) -> keep running in System Tray so device is always connectable
    let close_title = win_title.clone();
    main_window.window().on_close_requested(move || {
        logger::info(
            "win_app.tray",
            "Window close intercepted: hiding to system tray (background daemon stays online).",
        );
        dragdrop::hide_window_by_title(&close_title);
        slint::CloseRequestResponse::KeepWindowShown
    });

    let history_records = Arc::new(Mutex::new(load_history()));
    {
        let list = history_records.lock().unwrap();
        let slint_items = to_slint_history_items(&list);
        main_window.set_history_items(ModelRc::from(Rc::new(VecModel::from(slint_items))));
    }

    // System Tray with full right-click context menu (Show, Disconnect, Reconnect, Restart, Exit)
    let tray_win = main_window.as_weak();
    let tray_title = win_title.clone();
    let tray_partner = Arc::clone(&active_partner);
    let tray_capturing = Arc::clone(&is_host_capturing);
    let tray_voice = Arc::clone(&voice_engine);
    let tray_reconnect = Arc::clone(&reconnect_signal);
    let tray_tx = tx_out.clone();
    let _tray = SystemTray::new(&win_title, move |action| match action {
        TrayAction::ShowWindow => {
            logger::info("win_app.tray", "Restoring main window from system tray.");
            bring_main_window_to_front(&tray_title);
            let _ = tray_win.upgrade_in_event_loop(|win| {
                let _ = win.show();
            });
        }
        TrayAction::DisconnectSession => {
            logger::info("win_app.tray", "Disconnecting active session from system tray.");
            tray_capturing.store(false, Ordering::SeqCst);
            privacy::reset_host_privacy();
            privacy::set_viewer_hook_state(false, "", "");
            tray_voice.is_session_active.store(false, Ordering::SeqCst);
            tray_voice.is_mic_muted.store(true, Ordering::SeqCst);
            let clean = tray_partner.lock().unwrap().clone();
            if !clean.is_empty() {
                let disc = json!({
                    "type": "session-disconnect",
                    "targetClean": clean
                });
                let _ = tray_tx.send(OutgoingMsg::Json(disc));
            }
            let _ = tray_win.upgrade_in_event_loop(|win| {
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
        TrayAction::ReconnectRelay => {
            logger::info("win_app.tray", "Triggering relay reconnect from system tray.");
            tray_reconnect.store(true, Ordering::SeqCst);
        }
        TrayAction::RestartApp => {
            logger::info("win_app.tray", "Restarting OpenDesk application from system tray.");
            tray_capturing.store(false, Ordering::SeqCst);
            if let Ok(exe) = env::current_exe() {
                let args: Vec<String> = env::args().skip(1).collect();
                let _ = std::process::Command::new(exe).args(args).spawn();
            }
            std::process::exit(0);
        }
        TrayAction::ExitApp => {
            logger::info("win_app.tray", "Stopping and exiting OpenDesk from system tray.");
            daemon::mark_intentional_stop();
            privacy::reset_host_privacy();
            privacy::set_viewer_hook_state(false, "", "");
            tray_capturing.store(false, Ordering::SeqCst);
            let clean = tray_partner.lock().unwrap().clone();
            if !clean.is_empty() {
                let disc = json!({
                    "type": "session-disconnect",
                    "targetClean": clean
                });
                let _ = tray_tx.send(OutgoingMsg::Json(disc));
                thread::sleep(Duration::from_millis(80));
            }
            std::process::exit(0);
        }
    });

    spawn_network_worker(
        Some(main_window.as_weak()),
        cfg.desk_id.clone(),
        Arc::clone(&current_gateway),
        Arc::clone(&reconnect_signal),
        Arc::clone(&injector),
        Arc::clone(&active_partner),
        Arc::clone(&is_host_capturing),
        Arc::clone(&active_monitor_idx),
        Arc::clone(&voice_engine),
        Arc::clone(&remote_resolution),
        Arc::clone(&last_clip_seq),
        rx_out,
        tx_out.clone(),
    );

    // Admin callback: Refresh Server Host List on demand
    let tx_refresh = tx_out_clone.clone();
    main_window.on_refresh_hosts(move || {
        let req = json!({
            "type": "get-hosts"
        });
        let _ = tx_refresh.send(OutgoingMsg::Json(req));
    });

    // Register native Win32 Drag & Drop ON THE UI THREAD so OLE RevokeDragDrop succeeds!
    let partner_drop = Arc::clone(&active_partner);
    let tx_drop = tx_out.clone();
    let win_drop = main_window.as_weak();
    let title_for_drop = win_title.clone();
    thread::spawn(move || {
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(250));
            let partner_cb = Arc::clone(&partner_drop);
            let tx_cb = tx_drop.clone();
            let win_cb = win_drop.clone();
            let title_cb = title_for_drop.clone();
            let registered = Arc::new(AtomicBool::new(false));
            let reg_flag = Arc::clone(&registered);

            let _ = win_drop.upgrade_in_event_loop(move |_win| {
                let win_progress = win_cb.clone();
                if dragdrop::register_window_file_drop(&title_cb, move |paths| {
                    let target = partner_cb.lock().unwrap().clone();
                    if !target.is_empty() {
                        file_transfer::send_files_async(
                            paths,
                            target,
                            tx_cb.clone(),
                            Some(win_progress.clone()),
                            true, // Auto-paste directly into open remote Explorer folder!
                        );
                    }
                }) {
                    logger::info(
                        "app.tunnel_ft_comp",
                        "Win32 DragAcceptFiles registered on UI thread (OLE RevokeDragDrop active).",
                    );
                    if start_in_background {
                        dragdrop::hide_window_by_title(&title_cb);
                    }
                    reg_flag.store(true, Ordering::SeqCst);
                }
            });

            thread::sleep(Duration::from_millis(80));
            if registered.load(Ordering::SeqCst) {
                break;
            }
        }
    });

    // Copy Local ID Callback
    let inj_copy = Arc::clone(&injector);
    let seq_copy = Arc::clone(&last_clip_seq);
    let id_for_copy = formatted_local.clone();
    let win_copy = main_window.as_weak();
    main_window.on_copy_local_id(move || {
        inj_copy.set_clipboard_text(&id_for_copy);
        seq_copy.store(inj_copy.get_clipboard_sequence_number(), Ordering::SeqCst);
        if let Some(w) = win_copy.upgrade() {
            w.set_status_text("ID disalin".into());
        }
    });

    // Save Gateway URL Callback
    let gw_update = Arc::clone(&current_gateway);
    let sig_reconnect = Arc::clone(&reconnect_signal);
    let win_gw = main_window.as_weak();
    let mut cfg_save = cfg.clone();
    main_window.on_save_gateway_url(move |new_url| {
        let clean_url = normalize_gateway_url(&new_url);
        *gw_update.lock().unwrap() = clean_url.clone();
        cfg_save.gateway_url = clean_url.clone();
        save_config(&cfg_save);
        if let Some(w) = win_gw.upgrade() {
            w.set_gateway_url(clean_url.into());
        }
        sig_reconnect.store(true, Ordering::SeqCst);
    });

    // Request Connect Callback (Unattended Direct Connect)
    let tx_req = tx_out_clone.clone();
    let win_req = main_window.as_weak();
    let history_req = Arc::clone(&history_records);
    main_window.on_request_connect(move |target| {
        let clean = clean_desk_id(&target);
        let formatted = format_desk_id(&clean);

        {
            let mut list = history_req.lock().unwrap();
            let mut found = false;
            for item in list.iter_mut() {
                if item.desk_id == formatted {
                    item.last_connected = "Hari ini".to_string();
                    found = true;
                    break;
                }
            }
            if !found && !clean.is_empty() {
                list.insert(
                    0,
                    HistoryRecord {
                        desk_id: formatted.clone(),
                        alias: String::new(),
                        last_connected: "Hari ini".to_string(),
                    },
                );
            }
            save_history(&list);

            if let Some(w) = win_req.upgrade() {
                let slint_items = to_slint_history_items(&list);
                w.set_history_items(ModelRc::from(Rc::new(VecModel::from(slint_items))));
            }
        }

        logger::info(
            "win_app.frontend",
            &format!("Sending direct connection request for address {}", clean),
        );

        let req = json!({
            "type": "connect-request",
            "targetId": clean
        });
        let _ = tx_req.send(OutgoingMsg::Json(req));
        if let Some(w) = win_req.upgrade() {
            w.set_status_text(format!("Menghubungkan {}...", formatted).into());
        }
    });

    // Rename History Callbacks
    let win_rename = main_window.as_weak();
    main_window.on_open_rename(move |id, current_alias| {
        if let Some(w) = win_rename.upgrade() {
            w.set_rename_target_id(id);
            w.set_rename_input_val(current_alias);
            w.set_show_rename_modal(true);
        }
    });

    let win_save_rename = main_window.as_weak();
    let history_rename = Arc::clone(&history_records);
    main_window.on_save_rename(move |id, new_alias| {
        let mut list = history_rename.lock().unwrap();
        for item in list.iter_mut() {
            if item.desk_id == id.as_str() {
                item.alias = new_alias.to_string();
                break;
            }
        }
        save_history(&list);

        if let Some(w) = win_save_rename.upgrade() {
            let slint_items = to_slint_history_items(&list);
            w.set_history_items(ModelRc::from(Rc::new(VecModel::from(slint_items))));
        }
    });

    // Delete History Callback
    let win_del = main_window.as_weak();
    let history_del = Arc::clone(&history_records);
    main_window.on_delete_history(move |id| {
        let mut list = history_del.lock().unwrap();
        list.retain(|item| item.desk_id != id.as_str());
        save_history(&list);

        if let Some(w) = win_del.upgrade() {
            let slint_items = to_slint_history_items(&list);
            w.set_history_items(ModelRc::from(Rc::new(VecModel::from(slint_items))));
        }
    });

    let tx_accept = tx_out_clone.clone();
    let partner_accept = Arc::clone(&active_partner);
    main_window.on_accept_incoming(move || {
        let clean = partner_accept.lock().unwrap().clone();
        let resp = json!({
            "type": "approval-response",
            "fromClean": clean,
            "accept": true
        });
        let _ = tx_accept.send(OutgoingMsg::Json(resp));
    });

    let tx_reject = tx_out_clone.clone();
    let partner_reject = Arc::clone(&active_partner);
    main_window.on_reject_incoming(move || {
        let clean = partner_reject.lock().unwrap().clone();
        let resp = json!({
            "type": "approval-response",
            "fromClean": clean,
            "accept": false
        });
        let _ = tx_reject.send(OutgoingMsg::Json(resp));
    });

    // Toggle Voice Call Microphone Callback (syncs both local & remote partner mic!)
    let voice_toggle = Arc::clone(&voice_engine);
    let partner_mic = Arc::clone(&active_partner);
    let tx_mic = tx_out_clone.clone();
    let win_mic = main_window.as_weak();
    main_window.on_toggle_mic(move || {
        let prev = voice_toggle.is_mic_muted.load(Ordering::SeqCst);
        let next = !prev;
        voice_toggle.is_mic_muted.store(next, Ordering::SeqCst);
        if let Some(w) = win_mic.upgrade() {
            w.set_is_mic_muted(next);
        }
        let clean = partner_mic.lock().unwrap().clone();
        if !clean.is_empty() {
            let msg = json!({
                "type": "relay-input",
                "targetClean": clean,
                "event": {
                    "action": "set-mic",
                    "muted": next
                }
            });
            let _ = tx_mic.send(OutgoingMsg::Json(msg));
        }
    });

    // Switch Remote Monitor Callback (Viewer -> Host)
    let tx_mon = tx_out_clone.clone();
    let partner_mon = Arc::clone(&active_partner);
    let win_mon = main_window.as_weak();
    main_window.on_switch_monitor(move || {
        let clean = partner_mon.lock().unwrap().clone();
        if !clean.is_empty() {
            let msg = json!({
                "type": "relay-input",
                "targetClean": clean,
                "event": {
                    "action": "switch-monitor"
                }
            });
            let _ = tx_mon.send(OutgoingMsg::Json(msg));
            if let Some(w) = win_mon.upgrade() {
                w.set_status_text("Mengganti layar monitor...".into());
            }
        }
    });

    // Open File Picker Dialog Callback (Send Files button -> Auto-Paste into active remote folder)
    let tx_pick = tx_out_clone.clone();
    let partner_pick = Arc::clone(&active_partner);
    let win_pick = main_window.as_weak();
    main_window.on_open_file_picker(move || {
        let clean = partner_pick.lock().unwrap().clone();
        if clean.is_empty() {
            return;
        }
        let tx_f = tx_pick.clone();
        let win_f = win_pick.clone();
        thread::spawn(move || {
            let paths = dragdrop::pick_files_dialog();
            if !paths.is_empty() {
                file_transfer::send_files_async(paths, clean, tx_f, Some(win_f), true);
            }
        });
    });

    // Privacy Mode Callback (0=Normal, 1=Freeze Screen, 2=Maintenance Screen, 3=Black Screen)
    let tx_priv = tx_out_clone.clone();
    let partner_priv = Arc::clone(&active_partner);
    let win_priv = main_window.as_weak();
    main_window.on_set_privacy_mode(move |mode| {
        if let Some(w) = win_priv.upgrade() {
            w.set_privacy_mode(mode);
        }
        let clean = partner_priv.lock().unwrap().clone();
        if !clean.is_empty() {
            let msg = json!({
                "type": "relay-input",
                "targetClean": clean,
                "event": {
                    "action": "set-privacy",
                    "mode": mode
                }
            });
            let _ = tx_priv.send(OutgoingMsg::Json(msg));
        }
    });

    // Toggle Block Remote Physical Input Callback
    let tx_blk = tx_out_clone.clone();
    let partner_blk = Arc::clone(&active_partner);
    let win_blk = main_window.as_weak();
    main_window.on_toggle_block_input(move || {
        let mut next = false;
        if let Some(w) = win_blk.upgrade() {
            next = !w.get_block_remote_input();
            w.set_block_remote_input(next);
        }
        let clean = partner_blk.lock().unwrap().clone();
        if !clean.is_empty() {
            let msg = json!({
                "type": "relay-input",
                "targetClean": clean,
                "event": {
                    "action": "set-block-input",
                    "blocked": next
                }
            });
            let _ = tx_blk.send(OutgoingMsg::Json(msg));
        }
    });

    // System Shortcut Callback (Task Manager, Start Menu, Explorer, Desktop)
    let tx_sc = tx_out_clone.clone();
    let partner_sc = Arc::clone(&active_partner);
    main_window.on_send_system_shortcut(move |sc_name| {
        let clean = partner_sc.lock().unwrap().clone();
        if !clean.is_empty() {
            let msg = json!({
                "type": "relay-input",
                "targetClean": clean,
                "event": {
                    "action": "shortcut",
                    "name": sc_name.as_str()
                }
            });
            let _ = tx_sc.send(OutgoingMsg::Json(msg));
        }
    });

    let tx_disc = tx_out_clone.clone();
    let partner_disc = Arc::clone(&active_partner);
    let host_cap_disc = Arc::clone(&is_host_capturing);
    let voice_disc = Arc::clone(&voice_engine);
    let win_disc = main_window.as_weak();
    main_window.on_disconnect_session(move || {
        host_cap_disc.store(false, Ordering::SeqCst);
        privacy::reset_host_privacy();
        privacy::set_viewer_hook_state(false, "", "");
        voice_disc.is_session_active.store(false, Ordering::SeqCst);
        voice_disc.is_mic_muted.store(true, Ordering::SeqCst);
        let clean = partner_disc.lock().unwrap().clone();
        *partner_disc.lock().unwrap() = String::new();
        let disc = json!({
            "type": "session-disconnect",
            "targetClean": clean
        });
        let _ = tx_disc.send(OutgoingMsg::Json(disc));
        if let Some(w) = win_disc.upgrade() {
            w.set_is_connected(false);
            w.set_is_host(false);
            w.set_is_mic_muted(true);
            w.set_privacy_mode(0);
            w.set_block_remote_input(false);
            w.set_show_session_options(false);
            w.set_has_remote_frame(false);
            w.set_status_text("Online".into());
        }
    });

    // Coalesced Normalized Mouse Move Forwarding (Zero WebSocket Queue Flooding)
    let tx_move = tx_out_clone.clone();
    main_window.on_send_mouse_move(move |nx_10000, ny_10000| {
        let norm_x = (nx_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let norm_y = (ny_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let _ = tx_move.send(OutgoingMsg::MouseMove(norm_x, norm_y));
    });

    let tx_down = tx_out_clone.clone();
    let partner_down = Arc::clone(&active_partner);
    main_window.on_send_mouse_down(move |nx_10000, ny_10000, btn| {
        let clean = partner_down.lock().unwrap().clone();
        let norm_x = (nx_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let norm_y = (ny_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let input_msg = json!({
            "type": "relay-input",
            "targetClean": clean,
            "event": {
                "action": "mousedown",
                "normX": norm_x,
                "normY": norm_y,
                "button": btn
            }
        });
        let _ = tx_down.send(OutgoingMsg::Json(input_msg));
    });

    let tx_up = tx_out_clone.clone();
    let partner_up = Arc::clone(&active_partner);
    main_window.on_send_mouse_up(move |nx_10000, ny_10000, btn| {
        let clean = partner_up.lock().unwrap().clone();
        let norm_x = (nx_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let norm_y = (ny_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let input_msg = json!({
            "type": "relay-input",
            "targetClean": clean,
            "event": {
                "action": "mouseup",
                "normX": norm_x,
                "normY": norm_y,
                "button": btn
            }
        });
        let _ = tx_up.send(OutgoingMsg::Json(input_msg));
    });

    let tx_wheel = tx_out_clone.clone();
    let partner_wheel = Arc::clone(&active_partner);
    main_window.on_send_mouse_wheel(move |nx_10000, ny_10000, delta_y| {
        let clean = partner_wheel.lock().unwrap().clone();
        if clean.is_empty() {
            return;
        }
        let norm_x = (nx_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let norm_y = (ny_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let input_msg = json!({
            "type": "relay-input",
            "targetClean": clean,
            "event": {
                "action": "wheel",
                "normX": norm_x,
                "normY": norm_y,
                "deltaY": delta_y
            }
        });
        let _ = tx_wheel.send(OutgoingMsg::Json(input_msg));
    });

    // Full Keyboard + Clipboard Paste (Ctrl+V File & Text) Forwarding
    let tx_key = tx_out_clone.clone();
    let partner_key = Arc::clone(&active_partner);
    let inj_key = Arc::clone(&injector);
    let win_key = main_window.as_weak();
    main_window.on_send_key_event(move |text_shared, ctrl, alt, shift| {
        let clean = partner_key.lock().unwrap().clone();
        if clean.is_empty() {
            return;
        }
        let text = text_shared.as_str();
        if text.is_empty() {
            return;
        }

        // 1. Check if it's a special navigation/control key (Backspace, Enter, Arrows, Tab, Esc, F1-F12)
        if let Some(vk) = map_special_key_to_vk(text) {
            let msg = if ctrl || alt || shift {
                json!({
                    "type": "relay-input",
                    "targetClean": clean,
                    "event": {
                        "action": "keycombo",
                        "ctrl": ctrl,
                        "alt": alt,
                        "shift": shift,
                        "vk": vk
                    }
                })
            } else {
                json!({
                    "type": "relay-input",
                    "targetClean": clean,
                    "event": {
                        "action": "keypress",
                        "keyCode": vk
                    }
                })
            };
            let _ = tx_key.send(OutgoingMsg::Json(msg));
            return;
        }

        // 2. Handle Ctrl / Alt shortcuts (Ctrl+C, Ctrl+V, Ctrl+A, Ctrl+X, Ctrl+Z, etc.)
        if ctrl || alt {
            if let Some(ch) = text.chars().next() {
                let upper = ch.to_ascii_uppercase();
                // On Ctrl+V: If local clipboard has files, transfer & auto-paste into the open remote folder!
                if ctrl && !alt && (upper == 'V' || ch == '\u{0016}') {
                    let clip_files = inj_key.get_clipboard_files();
                    if !clip_files.is_empty() {
                        file_transfer::send_files_async(
                            clip_files,
                            clean.clone(),
                            tx_key.clone(),
                            Some(win_key.clone()),
                            true, // Auto-paste (Ctrl+V) on remote PC as soon as file lands!
                        );
                        return;
                    }
                    if let Some(clip_text) = inj_key.get_clipboard_text() {
                        if !clip_text.is_empty() {
                            let sync_msg = json!({
                                "type": "relay-input",
                                "targetClean": clean,
                                "event": {
                                    "action": "clipboard",
                                    "text": clip_text
                                }
                            });
                            let _ = tx_key.send(OutgoingMsg::Json(sync_msg));
                        }
                    }
                    let paste_msg = json!({
                        "type": "relay-input",
                        "targetClean": clean,
                        "event": {
                            "action": "keycombo",
                            "ctrl": true,
                            "alt": false,
                            "shift": shift,
                            "vk": 0x56
                        }
                    });
                    let _ = tx_key.send(OutgoingMsg::Json(paste_msg));
                    return;
                }

                let vk: u16 = if (ch as u32) >= 1 && (ch as u32) <= 26 {
                    (b'A' + (ch as u8 - 1)) as u16
                } else if upper.is_ascii_alphanumeric() {
                    upper as u16
                } else {
                    0
                };

                if vk > 0 {
                    let combo_msg = json!({
                        "type": "relay-input",
                        "targetClean": clean,
                        "event": {
                            "action": "keycombo",
                            "ctrl": ctrl,
                            "alt": alt,
                            "shift": shift,
                            "vk": vk
                        }
                    });
                    let _ = tx_key.send(OutgoingMsg::Json(combo_msg));
                    return;
                }
            }
        }

        // 3. Printable Unicode text (letters, numbers, symbols, spaces)
        let text_msg = json!({
            "type": "relay-input",
            "targetClean": clean,
            "event": {
                "action": "text",
                "text": text
            }
        });
        let _ = tx_key.send(OutgoingMsg::Json(text_msg));
    });

    main_window.run()
}
