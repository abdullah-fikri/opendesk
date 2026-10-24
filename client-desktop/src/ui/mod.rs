//! Presentation layer: the Slint window (`ui/*.slint`), its callbacks, the presenter that applies service
//! events to it, the tray icon and drag-and-drop. Nothing here decides behavior; callbacks call services.

mod callbacks;
mod presenter;
mod session_model;
mod view_model;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use core_engine::capture::HostSupport;
use slint::ComponentHandle;

use crate::domain::desk_id;
use crate::domain::edition::Edition;
use crate::infra::tray::{SystemTray, TrayAction, bring_main_window_to_front};
use crate::infra::{autostart, logger, window};
use crate::service::context::Ctx;
use crate::service::file_transfer::sender;
use crate::service::history::History;
use crate::service::sessions;

pub use presenter::SlintUi;

slint::include_modules!();

/// Fills the window with its initial state and binds every callback.
pub fn bind(win: &MainWindow, ctx: &Ctx, history: &Arc<History>, gateway_url: &str) {
    win.set_local_desk_id(desk_id::format(&ctx.local_id).into());
    win.set_gateway_url(gateway_url.into());
    win.set_is_admin(ctx.edition == Edition::Admin);
    win.set_active_dashboard_tab(if ctx.edition == Edition::Admin { 1 } else { 0 });
    win.set_history_items(view_model::history_items(&history.records()));
    if let HostSupport::Unsupported(reason) = core_engine::capture::host_support() {
        win.set_host_notice(reason.into());
    }
    callbacks::bind(win, ctx, history);

    // Closing the window keeps OpenDesk running in the tray, so this device stays reachable.
    let title = ctx.window_title.clone();
    win.window().on_close_requested(move || {
        logger::info("ui", "Window closed; OpenDesk keeps running in the tray.");
        if window::NATIVE_HIDE {
            // Windows: hide the native window but keep Slint's event loop alive (it quits when its last window hides).
            window::hide_window_by_title(&title);
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            // Elsewhere `app::run` keeps the event loop alive without a visible window.
            slint::CloseRequestResponse::HideWindow
        }
    });
}

/// Creates the tray icon. Keep the returned value alive for the lifetime of the app.
pub fn install_tray(win: &MainWindow, ctx: &Ctx) -> SystemTray {
    let weak = win.as_weak();
    let ctx = Arc::clone(ctx);
    let title = ctx.window_title.clone();
    SystemTray::new(&ctx.window_title.clone(), move |action| match action {
        TrayAction::ShowWindow => {
            bring_main_window_to_front(&title);
            let _ = weak.upgrade_in_event_loop(|w| {
                let _ = w.show();
            });
        }
        TrayAction::DisconnectSession => {
            logger::info("ui", "Disconnecting every session from the tray.");
            sessions::disconnect_all(&ctx);
        }
        TrayAction::ReconnectRelay => ctx.relay.reconnect.store(true, Ordering::SeqCst),
        TrayAction::RestartApp => {
            logger::info("ui", "Restarting from the tray.");
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).args(std::env::args().skip(1)).spawn();
            }
            std::process::exit(0);
        }
        TrayAction::ExitApp => {
            logger::info("ui", "Exiting from the tray.");
            autostart::mark_intentional_stop();
            if sessions::disconnect_all(&ctx) > 0 {
                // Let the relay connection send the disconnects before the process ends.
                thread::sleep(Duration::from_millis(80));
            }
            std::process::exit(0);
        }
    })
}

/// Accepts files dropped on the window and sends them to the current partner. OLE drop registration must
/// happen on the UI thread once the native window exists, so it is retried for a few seconds after start.
/// `hide_after` hides the window once registered (background start).
pub fn enable_file_drop(win: &MainWindow, ctx: &Ctx, hide_after: bool) {
    if !window::FILE_DROP_SUPPORTED {
        // Files are sent with the toolbar's file picker instead.
        return;
    }
    let weak = win.as_weak();
    let ctx = Arc::clone(ctx);
    thread::spawn(move || {
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(250));
            let registered = Arc::new(AtomicBool::new(false));
            let (flag, drop_ctx, title) = (Arc::clone(&registered), Arc::clone(&ctx), ctx.window_title.clone());
            let _ = weak.upgrade_in_event_loop(move |_| {
                let on_drop = move |paths| {
                    let target = drop_ctx.partner();
                    if !target.is_empty() {
                        // Auto-paste into the Explorer folder open on the other side.
                        sender::send_files(&drop_ctx, paths, target, true);
                    }
                };
                if window::register_window_file_drop(&title, on_drop) {
                    logger::info("ui", "File drop target registered.");
                    if hide_after {
                        window::hide_window_by_title(&title);
                    }
                    flag.store(true, Ordering::SeqCst);
                }
            });
            thread::sleep(Duration::from_millis(80));
            if registered.load(Ordering::SeqCst) {
                return;
            }
        }
    });
}
