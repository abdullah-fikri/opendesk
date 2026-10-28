//! Command-line entrypoint: `opendesk server|status|id|stop|uninstall|help` (Windows and Linux).
//!
//! The same executable is the GUI app; when the first argument is a known command it is handled here and the
//! process exits before any window, tray or network thread starts. The running app watches `config.json`, so
//! `opendesk server <url>` reconnects it within about a second.

use std::env;

use crate::domain::{desk_id, settings};
use crate::infra::autostart::{self, StopError};
use crate::infra::{device, identity, logger, storage, user_clipboard, window};

const HELP: &str = "Commands:\r\n  opendesk server <url>   Set the relay address (e.g. wss://relay.example.com)\r\n  opendesk status         Show this device's ID and relay\r\n  opendesk stop           Stop OpenDesk and its watchdog\r\n  opendesk uninstall      Stop OpenDesk and remove autostart entries";

/// Handles a CLI command and exits, or returns when the arguments are for the GUI app.
pub fn run_if_requested() {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(first) = args.first().map(|a| a.to_ascii_lowercase()) else { return };

    let code = match first.as_str() {
        "server" => set_server(args.get(1).map(|a| a.trim_start_matches('-').to_string())),
        f if f.starts_with("--server=") => set_server(Some(args[0]["--server=".len()..].to_string())),
        "stop" | "kill" => stop(),
        "uninstall" => uninstall(),
        "status" | "id" | "--status" | "help" | "--help" => status(),
        // Internal: the Windows service entry point, and the file picker and clipboard reader the SYSTEM agent
        // runs as the user.
        #[cfg(windows)]
        "--service" => crate::infra::system_service::run(),
        "--pick-files" => pick_files(args.get(1)),
        "--clipboard-read" => clipboard_read(args.get(1)),
        _ => return,
    };
    std::process::exit(code);
}

fn set_server(url: Option<String>) -> i32 {
    let mut cfg = storage::load_settings();
    let Some(raw) = url.filter(|u| !u.is_empty()) else {
        autostart::print_to_console(&format!(
            "[OpenDesk]\r\n  Device ID : {}\r\n  Relay     : {}\r\n  Change it : opendesk server wss://relay.example.com",
            desk_id::format(&cfg.desk_id),
            cfg.gateway_url
        ));
        return 0;
    };
    cfg.gateway_url = settings::normalize_gateway_url(&raw);
    storage::save_settings(&cfg);
    logger::info("cli", &format!("Gateway URL updated via CLI to: {}", cfg.gateway_url));
    autostart::print_to_console(&format!(
        "[OpenDesk] Relay updated.\r\n  Relay     : {}\r\n  Device ID : {}\r\n  The running app reconnects automatically.",
        cfg.gateway_url,
        desk_id::format(&cfg.desk_id)
    ));
    0
}

fn stop() -> i32 {
    match autostart::stop_running() {
        Ok(()) => {
            autostart::print_to_console(if cfg!(windows) {
                "[OpenDesk] OpenDesk and its watchdog were stopped."
            } else {
                "[OpenDesk] OpenDesk was stopped."
            });
            0
        }
        Err(StopError::AccessDenied) => {
            autostart::print_to_console(if cfg!(windows) {
                "[OpenDesk] Failed: OpenDesk runs as Administrator.\r\n  Run this terminal as Administrator, or use the tray menu \"Stop & Exit\"."
            } else {
                "[OpenDesk] Failed: the running OpenDesk belongs to another user.\r\n  Run this command as that user, or use the tray menu \"Stop & Exit\"."
            });
            1
        }
    }
}

fn uninstall() -> i32 {
    autostart::remove_autostart();
    autostart::print_to_console(if cfg!(windows) {
        "[OpenDesk] Processes stopped and autostart entries removed.\r\nRun the uninstaller or delete the installation folder."
    } else {
        "[OpenDesk] OpenDesk stopped and its start-at-login entry removed.\r\nRemove the package (sudo apt remove opendesk) or delete the binary."
    });
    0
}

/// Writes the picked paths, one per line, to `out`; exit code 1 when cancelled.
fn pick_files(out: Option<&String>) -> i32 {
    let Some(out) = out else { return 2 };
    let paths = window::pick_files_dialog();
    let list: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    match std::fs::write(out, list.join("\n")) {
        Ok(()) if !list.is_empty() => 0,
        _ => 1,
    }
}

/// Writes the clipboard's text and files as JSON to `out`.
fn clipboard_read(out: Option<&String>) -> i32 {
    let Some(out) = out else { return 2 };
    let json = serde_json::to_vec(&user_clipboard::read_here()).unwrap_or_default();
    match std::fs::write(out, json) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn status() -> i32 {
    let cfg = storage::load_settings();
    autostart::print_to_console(&format!(
        "[OpenDesk]\r\n  Device name : {}\r\n  Device ID   : {}\r\n  Device key  : {}\r\n  Relay       : {}\r\n\r\n{}",
        device::display_name(),
        desk_id::format(&cfg.desk_id),
        identity::local().map(|id| identity::fingerprint(&id.public)).unwrap_or_else(|| "unavailable".into()),
        cfg.gateway_url,
        HELP
    ));
    0
}
