#![windows_subsystem = "windows"]

mod audio;
mod config;
mod daemon;
mod dragdrop;
mod file_transfer;
mod keymap;
mod logger;
mod network;
mod p2p;
mod privacy;
mod protocol;
mod session;
mod tray;
mod viewer;
mod viewer_ui;
mod virtual_clip;

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

/// Target of the old single-session settings callbacks: the focused viewer tab (even while it connects),
/// else the partner of the host role.
fn session_target(ctx: &viewer_ui::Ctx) -> String {
    let focused = viewer::VIEWERS.focused_id();
    if focused.is_empty() { ctx.active_partner.lock().unwrap().clone() } else { focused }
}

/// Every session to end from the tray: all viewer tabs plus the host-role partner.
fn session_targets(active_partner: &Mutex<String>) -> Vec<String> {
    let mut targets: Vec<String> = viewer::VIEWERS.all().iter().map(|s| s.id.clone()).collect();
    let clean = active_partner.lock().unwrap().clone();
    if !clean.is_empty() && !targets.contains(&clean) {
        targets.push(clean);
    }
    targets
}

/// A connected viewer session.
fn is_live(id: &str) -> bool {
    viewer::VIEWERS.get(id).is_some_and(|s| s.is_connected())
}

/// Registers a UI callback that runs with the window and the shared viewer context.
macro_rules! on_ui {
    ($win:expr, $ctx:expr, $setter:ident, |$w:ident, $c:ident $(, $arg:ident)*| $body:block) => {{
        let weak = $win.as_weak();
        let ctx_owned = $ctx.clone();
        $win.$setter(move |$($arg),*| {
            if let Some($w) = weak.upgrade() {
                let $c = &ctx_owned;
                $body
            }
        });
    }};
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
    let host_resolution_mode = Arc::new(AtomicUsize::new(1080));
    let remote_resolution = Arc::new(Mutex::new((local_w, local_h)));
    let last_clip_seq = Arc::new(AtomicU32::new(injector.get_clipboard_sequence_number()));

    let (tx_out, rx_out) = mpsc::channel::<OutgoingMsg>();
    let tx_out_clone = tx_out.clone();
    let voice_engine = VoiceEngine::new(tx_out.clone());
    privacy::init_privacy_and_hooks(tx_out.clone());
    let ui_ctx = viewer_ui::Ctx {
        tx: tx_out.clone(),
        active_partner: Arc::clone(&active_partner),
        is_host: Arc::clone(&is_host_capturing),
        voice: Arc::clone(&voice_engine),
        injector: Arc::clone(&injector),
        last_clip_seq: Arc::clone(&last_clip_seq),
        win_title: format!("OpenDesk - {}", format_desk_id(&cfg.desk_id)),
    };

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
            Arc::clone(&host_resolution_mode),
            Arc::clone(&voice_engine),
            Arc::clone(&remote_resolution),
            Arc::clone(&last_clip_seq),
            rx_out,
            tx_out.clone(),
            ui_ctx.clone(),
        );

        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    // -------------------------------------------------------------------------
    // GUI MODE (Standard User `make build-windows` or Admin `make build-windows-admin`)
    // -------------------------------------------------------------------------
    let main_window = MainWindow::new()?;
    viewer_ui::init(&main_window);

    let formatted_local = format_desk_id(&cfg.desk_id);
    let win_title = format!("OpenDesk - {}", formatted_local);
    main_window.set_local_desk_id(formatted_local.clone().into());
    main_window.set_gateway_url(cfg.gateway_url.clone().into());
    main_window.set_status_text("Menghubungkan...".into());
    main_window.set_is_relay_online(false);
    main_window.set_is_admin(cfg!(feature = "admin"));
    main_window.set_active_dashboard_tab(if cfg!(feature = "admin") { 1 } else { 0 });
    main_window.set_resolution_mode(session::RES_AUTO as i32);
    main_window.set_audio_quality(2);

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
            for clean in session_targets(&tray_partner) {
                let disc = json!({
                    "type": "session-disconnect",
                    "targetClean": clean
                });
                let _ = tray_tx.send(OutgoingMsg::Json(disc));
            }
            viewer_ui::end_all_core();
            tray_partner.lock().unwrap().clear();
            let _ = tray_win.upgrade_in_event_loop(|win| {
                viewer_ui::clear_all_ui();
                win.set_focused_session("".into());
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
            let targets = session_targets(&tray_partner);
            for clean in &targets {
                let disc = json!({
                    "type": "session-disconnect",
                    "targetClean": clean
                });
                let _ = tray_tx.send(OutgoingMsg::Json(disc));
            }
            if !targets.is_empty() {
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
        Arc::clone(&host_resolution_mode),
        Arc::clone(&voice_engine),
        Arc::clone(&remote_resolution),
        Arc::clone(&last_clip_seq),
        rx_out,
        tx_out.clone(),
        ui_ctx.clone(),
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
    let req_ctx = ui_ctx.clone();
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

        let alias = history_req
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.desk_id == formatted)
            .map(|r| r.alias.clone())
            .unwrap_or_default();
        if let Some(w) = win_req.upgrade() {
            if clean.is_empty() {
                // Unchanged: the relay answers with its "not found" error.
                let _ = tx_req.send(OutgoingMsg::Json(json!({ "type": "connect-request", "targetId": clean })));
                return;
            }
            let (title, subtitle) = if alias.is_empty() { (formatted.clone(), String::new()) } else { (alias, formatted.clone()) };
            viewer_ui::request_connect(&w, &req_ctx, &clean, title, subtitle);
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

    // ---- Session actions. The old single-session callbacks act on the focused viewer session (or, in the
    // host role, on the viewer this machine hosts); the multi-session callbacks name their session. ----

    // Toggle Voice Call Microphone Callback (syncs both local & remote partner mic!)
    on_ui!(main_window, ui_ctx, on_toggle_mic, |w, c| {
        viewer_ui::toggle_mic(&w, c, &session_target(c));
    });

    // Switch Remote Monitor Callback (Viewer -> Host): cycles, understood by every host version.
    let tx_mon = tx_out_clone.clone();
    on_ui!(main_window, ui_ctx, on_switch_monitor, |w, c| {
        let clean = c.active_partner.lock().unwrap().clone();
        if !clean.is_empty() {
            let msg = json!({
                "type": "relay-input",
                "targetClean": clean,
                "event": {
                    "action": "switch-monitor"
                }
            });
            let _ = tx_mon.send(OutgoingMsg::Json(msg));
            w.set_status_text("Mengganti layar monitor...".into());
        }
    });

    // Open File Picker Dialog Callback (Send Files button -> Auto-Paste into active remote folder)
    on_ui!(main_window, ui_ctx, on_open_file_picker, |w, c| {
        let clean = c.active_partner.lock().unwrap().clone();
        viewer_ui::send_files(&w, c, &clean);
    });

    // Privacy Mode Callback (0=Normal, 1=Freeze Screen, 2=Maintenance Screen, 3=Black Screen)
    on_ui!(main_window, ui_ctx, on_set_privacy_mode, |w, c, mode| {
        viewer_ui::set_privacy(&w, c, &session_target(c), mode);
    });

    // Toggle Block Remote Physical Input Callback
    on_ui!(main_window, ui_ctx, on_toggle_block_input, |w, c| {
        viewer_ui::toggle_block_input(&w, c, &session_target(c));
    });

    // System Shortcut Callback (Task Manager, Start Menu, Explorer, Desktop)
    on_ui!(main_window, ui_ctx, on_send_system_shortcut, |w, c, sc_name| {
        viewer_ui::shortcut(&w, c, &session_target(c), sc_name.as_str());
    });

    // Remote Resolution Mode Selection Callback (Otomatis, 1:1 Native, 1080p, 720p, 480p)
    let host_res = Arc::clone(&host_resolution_mode);
    on_ui!(main_window, ui_ctx, on_set_resolution_mode, |w, c, mode| {
        host_res.store(viewer_ui::sanitize_res_mode(mode), Ordering::SeqCst);
        viewer_ui::set_resolution(&w, c, &session_target(c), mode);
    });

    // Canvas size (physical px) for automatic resolution, sent 300 ms after resizing stops.
    on_ui!(main_window, ui_ctx, on_viewport_resized, |_w, c, width, height| {
        viewer_ui::viewport_resized(c, &viewer::VIEWERS.focused_id(), width, height);
    });

    // Voice Call Microphone Audio Quality Selection Callback (16kHz, 24kHz, 48kHz HD)
    on_ui!(main_window, ui_ctx, on_set_audio_quality, |w, c, quality| {
        viewer_ui::set_audio_quality(&w, c, &session_target(c), quality);
    });

    let tx_disc = tx_out_clone.clone();
    let partner_disc = Arc::clone(&active_partner);
    let host_cap_disc = Arc::clone(&is_host_capturing);
    let voice_disc = Arc::clone(&voice_engine);
    on_ui!(main_window, ui_ctx, on_disconnect_session, |w, c| {
        // While this PC is controlled (the "Dikontrol / Akhiri" chip) it ends only the host-side session.
        // Otherwise (old viewer toolbar) it ends the focused viewer tab; the other tabs stay open.
        let focused = viewer::VIEWERS.focused_id();
        let hosting = host_cap_disc.load(Ordering::SeqCst);
        if !hosting && !focused.is_empty() {
            viewer_ui::close(&w, c, &focused, None);
            return;
        }
        let viewing = !viewer::VIEWERS.is_empty();
        let clean = partner_disc.lock().unwrap().clone();
        virtual_clip::clear_offer(if viewing { &clean } else { "" });
        host_cap_disc.store(false, Ordering::SeqCst);
        privacy::reset_host_privacy();
        if viewing {
            // Never touches viewer sessions: input target, hook and voice go back to the focused tab.
            let disc = json!({ "type": "session-disconnect", "targetClean": clean });
            let _ = tx_disc.send(OutgoingMsg::Json(disc));
            w.set_is_host(false);
            viewer_ui::sync_focus(&w, c, &viewer::VIEWERS.focused_id());
            return;
        }
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
        w.set_is_connected(false);
        w.set_is_host(false);
        w.set_is_mic_muted(true);
        w.set_privacy_mode(0);
        w.set_block_remote_input(false);
        w.set_show_session_options(false);
        w.set_has_remote_frame(false);
        w.set_status_text("Online".into());
    });

    // Coalesced Normalized Mouse Move Forwarding (Zero WebSocket Queue Flooding)
    let tx_move = tx_out_clone.clone();
    let partner_move = Arc::clone(&active_partner);
    main_window.on_send_mouse_move(move |nx_10000, ny_10000| {
        let norm_x = (nx_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let norm_y = (ny_10000 as f64 / 10000.0).clamp(0.0, 1.0);
        let clean = partner_move.lock().unwrap().clone();
        let _ = tx_move.send(OutgoingMsg::MouseMove(clean, norm_x, norm_y));
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

    // Full Keyboard Forwarding: physical key down/up (modifiers included) so the host sees real key
    // state (Ctrl+click, Ctrl+scroll, held Shift...), unicode text for typing, Ctrl+V clipboard/files.
    on_ui!(main_window, ui_ctx, on_send_key_event, |w, c, text, pressed, repeat, ctrl, alt, shift| {
        let clean = c.active_partner.lock().unwrap().clone();
        viewer_ui::key_event(&w, c, &clean, text.as_str(), pressed, repeat, keymap::Mods { ctrl, alt, shift });
    });

    // Focus lost / window deactivated: lift every key still held on the host.
    on_ui!(main_window, ui_ctx, on_release_remote_keys, |_w, c| {
        let clean = c.active_partner.lock().unwrap().clone();
        viewer_ui::release_keys(c, &clean);
    });

    // ---- Multi-session viewer API (docs/ui-contract.md) ----

    on_ui!(main_window, ui_ctx, on_focus_session, |w, c, id| {
        viewer_ui::focus(&w, c, &id);
        w.set_show_new_session(false);
    });
    on_ui!(main_window, ui_ctx, on_close_session, |w, c, id| {
        viewer_ui::close(&w, c, &id, None);
    });
    on_ui!(main_window, ui_ctx, on_set_view_layout, |w, _c, layout| {
        viewer_ui::set_layout(&w, layout);
    });
    on_ui!(main_window, ui_ctx, on_new_session_page, |w, _c| {
        w.set_show_new_session(true);
    });
    on_ui!(main_window, ui_ctx, on_session_mouse_move, |w, c, id, x, y| {
        viewer_ui::mouse(&w, c, &id, viewer_ui::Mouse::Move, x, y);
    });
    on_ui!(main_window, ui_ctx, on_session_mouse_down, |w, c, id, x, y, btn| {
        viewer_ui::mouse(&w, c, &id, viewer_ui::Mouse::Down(btn), x, y);
    });
    on_ui!(main_window, ui_ctx, on_session_mouse_up, |w, c, id, x, y, btn| {
        viewer_ui::mouse(&w, c, &id, viewer_ui::Mouse::Up(btn), x, y);
    });
    on_ui!(main_window, ui_ctx, on_session_mouse_wheel, |w, c, id, x, y, dy| {
        viewer_ui::mouse(&w, c, &id, viewer_ui::Mouse::Wheel(dy), x, y);
    });
    on_ui!(main_window, ui_ctx, on_session_key_event, |w, c, id, text, pressed, repeat, ctrl, alt, shift| {
        // Only the focused session gets keyboard input.
        if viewer::VIEWERS.is_focused(&id) && is_live(&id) {
            viewer_ui::key_event(&w, c, &id, text.as_str(), pressed, repeat, keymap::Mods { ctrl, alt, shift });
        }
    });
    on_ui!(main_window, ui_ctx, on_session_release_keys, |_w, c, id| {
        viewer_ui::release_keys(c, &id);
    });
    on_ui!(main_window, ui_ctx, on_session_viewport_resized, |_w, c, id, width, height| {
        viewer_ui::viewport_resized(c, &id, width, height);
    });
    on_ui!(main_window, ui_ctx, on_select_monitor, |w, c, id, index| {
        if is_live(&id) {
            viewer_ui::select_monitor(&w, c, &id, index);
        }
    });
    on_ui!(main_window, ui_ctx, on_session_send_files, |w, c, id| {
        if is_live(&id) {
            viewer_ui::send_files(&w, c, &id);
        }
    });
    on_ui!(main_window, ui_ctx, on_session_toggle_mic, |w, c, id| {
        if is_live(&id) {
            viewer_ui::toggle_mic(&w, c, &id);
        }
    });
    on_ui!(main_window, ui_ctx, on_session_set_privacy, |w, c, id, mode| {
        if is_live(&id) {
            viewer_ui::set_privacy(&w, c, &id, mode);
        }
    });
    on_ui!(main_window, ui_ctx, on_session_toggle_block_input, |w, c, id| {
        if is_live(&id) {
            viewer_ui::toggle_block_input(&w, c, &id);
        }
    });
    on_ui!(main_window, ui_ctx, on_session_shortcut, |w, c, id, name| {
        if is_live(&id) {
            viewer_ui::shortcut(&w, c, &id, name.as_str());
        }
    });
    on_ui!(main_window, ui_ctx, on_session_set_resolution, |w, c, id, mode| {
        if viewer::VIEWERS.get(&id).is_some() {
            viewer_ui::set_resolution(&w, c, &id, mode);
        }
    });
    on_ui!(main_window, ui_ctx, on_session_set_audio_quality, |w, c, id, quality| {
        if viewer::VIEWERS.get(&id).is_some() {
            viewer_ui::set_audio_quality(&w, c, &id, quality);
        }
    });

    main_window.run()
}
