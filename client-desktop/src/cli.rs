//! Command-line entrypoint: `opendesk server|status|id|stop|uninstall|help` (Windows and Linux), and the update
//! commands `--update-now|--rollback|--pin-version|--unpin` (Windows, handed to the service).
//!
//! The same executable is the GUI app; when the first argument is a known command it is handled here and the
//! process exits before any window, tray or network thread starts. The running app watches `config.json`, so
//! `opendesk server <url>` reconnects it within about a second.

use std::env;

use crate::domain::{desk_id, settings};
use crate::infra::autostart::{self, StopError};
use crate::infra::{device, logger, storage, user_clipboard, window};

const HELP: &str = "Commands:\r\n  opendesk server <url>   Set the relay address (e.g. wss://relay.example.com)\r\n  opendesk status         Show this device's ID and relay\r\n  opendesk stop           Stop OpenDesk and its watchdog\r\n  opendesk uninstall      Stop OpenDesk and remove autostart entries\r\n\r\nUpdates (Windows, as Administrator):\r\n  opendesk --update-now           Check and install the latest version\r\n  opendesk --rollback [version]   Go back to the previous (or a kept) version; --force below min_version\r\n  opendesk --pin-version <v>      Stay on version <v>; opendesk --unpin to follow releases again";

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
        "--update-now" | "--rollback" | "--pin-version" | "--unpin" => update_request(&first, &args[1..]),
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

/// `--update-now`, `--rollback [version] [--force]`, `--pin-version <v>`, `--unpin`: queued for the service,
/// which carries them out (see `service::update`). Only administrators can write the update directory.
fn update_request(command: &str, rest: &[String]) -> i32 {
    use crate::service::update::manifest::Version;
    use crate::service::update::store::Request;

    let version = rest.iter().find(|a| !a.starts_with("--")).cloned();
    if version.as_deref().is_some_and(|v| Version::parse(v).is_none()) {
        autostart::print_to_console("[OpenDesk] Versions look like 1.2.3.");
        return 2;
    }
    let request = match command {
        "--update-now" => Request::UpdateNow,
        "--rollback" => Request::Rollback { version, force: rest.iter().any(|a| a == "--force") },
        "--pin-version" => match version {
            Some(version) => Request::Pin { version },
            None => {
                autostart::print_to_console("[OpenDesk] Usage: opendesk --pin-version 1.2.3");
                return 2;
            }
        },
        _ => Request::Unpin,
    };
    queue_update_request(&request)
}

#[cfg(windows)]
fn queue_update_request(request: &crate::service::update::store::Request) -> i32 {
    use crate::service::update::{secure_dir, store::Store};

    let root = secure_dir::root();
    // Not found (as opposed to access denied): the service never prepared it.
    let missing = std::fs::metadata(&root).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    if !crate::infra::system_service::installed() || missing {
        autostart::print_to_console(
            "[OpenDesk] Updates are managed by the OpenDesk service, which is not running here.",
        );
        return 1;
    }
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    match Store::new(root).push_request(request, unique) {
        Ok(()) => {
            autostart::print_to_console(
                "[OpenDesk] Request sent to the OpenDesk service (it acts within a few seconds; see opendesk.trace).",
            );
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            autostart::print_to_console("[OpenDesk] Failed: run this terminal as Administrator.");
            1
        }
        Err(e) => {
            autostart::print_to_console(&format!("[OpenDesk] Failed: {e}"));
            1
        }
    }
}

#[cfg(not(windows))]
fn queue_update_request(_request: &crate::service::update::store::Request) -> i32 {
    autostart::print_to_console(
        "[OpenDesk] Auto-update is Windows only; update the package with your package manager.",
    );
    1
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
        "[OpenDesk]\r\n  Device name : {}\r\n  Device ID   : {}\r\n  Relay       : {}\r\n\r\n{}",
        device::display_name(),
        desk_id::format(&cfg.desk_id),
        cfg.gateway_url,
        HELP
    ));
    0
}
