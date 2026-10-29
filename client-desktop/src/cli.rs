//! Command-line entrypoint: `opendesk status|server|trust|stop|uninstall|version|help` (Windows and Linux), and
//! the update commands `update|rollback|pin|unpin` (Windows, handed to the service). The older spellings
//! (`--update-now`, `--rollback`, `--pin-version`, `--unpin`, `id`, `--status`, `kill`) keep working.
//!
//! The same executable is the GUI app; when the first argument is a known command it is handled here and the
//! process exits before any window, tray or network thread starts. The running app watches `config.json`, so
//! `opendesk server <url>` reconnects it within about a second.

use std::env;

use crate::domain::{desk_id, settings};
use crate::infra::autostart::{self, StopError};
use crate::infra::{device, identity, key_pins, logger, storage, user_clipboard, window};

const HELP: &str = "Usage: opendesk <command>\r\n\r\n\
Commands:\r\n\
\x20 status                 Show this device's name, ID, key and relay\r\n\
\x20 server [url]           Show or set the relay address (e.g. wss://relay.example.com)\r\n\
\x20 trust <id>             Accept a host's changed device key\r\n\
\x20 stop                   Stop OpenDesk and its watchdog\r\n\
\x20 uninstall              Stop OpenDesk and remove autostart entries\r\n\
\x20 version                Show the installed version\r\n\
\x20 help                   Show this help\r\n\r\n\
Updates (Windows, run as Administrator):\r\n\
\x20 update [latest]        Install the newest release now and follow new releases\r\n\
\x20 update <version>       Install that release, newer or older, and stay on it\r\n\
\x20 update status          Show the installed, pinned and kept versions\r\n\
\x20 rollback [version]     Go back to the previous (or a kept) version\r\n\
\x20 pin <version>          Install only that version from now on\r\n\
\x20 unpin                  Follow new releases again\r\n\
\x20 Add --force to update or rollback to go below the release's minimum version.";

/// Handles a CLI command and exits, or returns when the arguments are for the GUI app.
pub fn run_if_requested() {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(first) = args.first().map(|a| a.to_ascii_lowercase()) else { return };
    let rest = &args[1..];

    let code = match first.as_str() {
        "server" => set_server(rest.first().map(|a| a.trim_start_matches('-').to_string())),
        f if f.starts_with("--server=") => set_server(Some(args[0]["--server=".len()..].to_string())),
        "trust" => trust(rest.first()),
        "stop" | "kill" => stop(),
        "uninstall" => uninstall(),
        "status" | "id" | "--status" => status(),
        "version" | "--version" | "-v" => print(&format!("OpenDesk {}", env!("CARGO_PKG_VERSION")), 0),
        "help" | "--help" | "-h" | "/?" => print(HELP, 0),
        "update" => update(rest),
        "rollback" | "pin" | "unpin" => update_request(&first, rest),
        "--update-now" => update(&[]),
        "--rollback" | "--pin-version" | "--unpin" => update_request(first.trim_start_matches("--"), rest),
        // Internal: the Windows service entry point, and the file picker and clipboard reader the SYSTEM agent
        // runs as the user.
        #[cfg(windows)]
        "--service" => crate::infra::system_service::run(),
        "--pick-files" => pick_files(rest.first()),
        "--clipboard-read" => clipboard_read(rest.first()),
        // A mistyped command must not silently start the app; options (`--background`, `--id=`) and paths do.
        f if is_command_word(f) => print(&format!("Unknown command: {}\r\n\r\n{HELP}", args[0]), 2),
        _ => return,
    };
    std::process::exit(code);
}

/// A bare word such as `updte`: not an option and not a path.
fn is_command_word(arg: &str) -> bool {
    !arg.is_empty() && !arg.starts_with('-') && arg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn print(text: &str, code: i32) -> i32 {
    autostart::print_to_console(text);
    code
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

/// Accepts a host's new key: forgets the pinned one, so the next session pins the key it presents.
fn trust(id: Option<&String>) -> i32 {
    let clean = id.map(|s| desk_id::clean(s)).unwrap_or_default();
    if !desk_id::is_valid(&clean) {
        autostart::print_to_console("[OpenDesk] Usage: opendesk trust <device ID>");
        return 2;
    }
    let text = if key_pins::forget(&clean) {
        format!(
            "[OpenDesk] Forgot the key of {}. The next connection trusts the key it presents.",
            desk_id::format(&clean)
        )
    } else {
        format!("[OpenDesk] No key of {} was pinned.", desk_id::format(&clean))
    };
    autostart::print_to_console(&text);
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

/// `update [latest|<version>|status] [--force]`.
fn update(rest: &[String]) -> i32 {
    use crate::service::update::store::Request;

    let force = rest.iter().any(|a| a == "--force");
    match rest.iter().find(|a| !a.starts_with("--")).map(|a| a.to_ascii_lowercase()).as_deref() {
        None | Some("latest") | Some("now") => {
            queue_update_requests(&[Request::Unpin, Request::UpdateNow], "Checking for the newest release")
        }
        Some("status") => update_status(),
        Some(v) => match parse_version(v) {
            Some(version) => {
                let what = format!(
                    "Installing {version}; OpenDesk then stays on it (opendesk update latest to follow releases)"
                );
                queue_update_requests(&[Request::Install { version, force }], &what)
            }
            None => 2,
        },
    }
}

/// `rollback [version] [--force]`, `pin <version>`, `unpin`: queued for the service, which carries them out (see
/// `service::update`). Only administrators can write the update directory.
fn update_request(command: &str, rest: &[String]) -> i32 {
    use crate::service::update::store::Request;

    let version = match rest.iter().find(|a| !a.starts_with("--")) {
        Some(raw) => match parse_version(raw) {
            Some(v) => Some(v),
            None => return 2,
        },
        None => None,
    };
    let (request, what) = match command {
        "rollback" => {
            let what = match &version {
                Some(v) => format!("Rolling back to {v}"),
                None => "Rolling back to the previous version".to_string(),
            };
            (Request::Rollback { version, force: rest.iter().any(|a| a == "--force") }, what)
        }
        "pin" | "pin-version" => match version {
            Some(version) => {
                let what = format!("Pinning to {version}");
                (Request::Pin { version }, what)
            }
            None => return print("[OpenDesk] Usage: opendesk pin 1.2.3", 2),
        },
        _ => (Request::Unpin, "Following new releases again".to_string()),
    };
    queue_update_requests(&[request], &what)
}

/// A version argument (`1.2.3` or `v1.2.3`), normalized; prints the expected form when it is not one.
fn parse_version(raw: &str) -> Option<String> {
    use crate::service::update::manifest::Version;

    let parsed = Version::parse(raw).map(|v| v.to_string());
    if parsed.is_none() {
        autostart::print_to_console(&format!("[OpenDesk] \"{raw}\" is not a version; versions look like 1.2.3."));
    }
    parsed
}

/// The update directory, or None (printed) when the OpenDesk service does not manage updates here.
#[cfg(windows)]
fn update_store() -> Option<crate::service::update::store::Store> {
    use crate::service::update::{secure_dir, store::Store};

    let root = secure_dir::root();
    // Not found (as opposed to access denied): the service never prepared it.
    let missing = std::fs::metadata(&root).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    if !crate::infra::system_service::installed() || missing {
        autostart::print_to_console(
            "[OpenDesk] Updates are managed by the OpenDesk service, which is not installed here.\r\n  Install OpenDesk with its setup program; the portable .exe does not update itself.",
        );
        return None;
    }
    Some(Store::new(root))
}

#[cfg(windows)]
fn queue_update_requests(requests: &[crate::service::update::store::Request], what: &str) -> i32 {
    let Some(store) = update_store() else { return 1 };
    let base = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    // Consecutive names keep the order: the service handles requests oldest first.
    for (i, request) in requests.iter().enumerate() {
        match store.push_request(request, base + i as u128) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return print("[OpenDesk] Failed: run this terminal as Administrator.", 1);
            }
            Err(e) => return print(&format!("[OpenDesk] Failed: {e}"), 1),
        }
    }
    print(
        &format!(
            "[OpenDesk] {what}.\r\n  The OpenDesk service acts within a few seconds and installs once no session is active.\r\n  Progress: opendesk update status"
        ),
        0,
    )
}

/// `update status`: what the service knows about versions on this machine.
#[cfg(windows)]
fn update_status() -> i32 {
    let Some(store) = update_store() else { return 1 };
    let state = store.state();
    let kept: Vec<String> = store.installed_versions().iter().map(|v| v.to_string()).collect();
    let queued = std::fs::read_dir(store.root().join("requests")).is_ok_and(|mut d| d.next().is_some());
    let pending = match store.pending() {
        Some(p) => format!("{p:?}"),
        None if queued => "request queued".into(),
        None => "none".into(),
    };
    let or_none = |list: &[String]| if list.is_empty() { "none".to_string() } else { list.join(", ") };
    print(
        &format!(
            "[OpenDesk] Updates\r\n  Installed   : {}\r\n  Pinned to   : {}\r\n  Kept        : {}\r\n  Marked bad  : {}\r\n  Min version : {}\r\n  In progress : {pending}\r\n  Releases    : https://github.com/abdullah-fikri/opendesk/releases",
            env!("CARGO_PKG_VERSION"),
            state.pinned.as_deref().unwrap_or("none (follows new releases)"),
            or_none(&kept),
            or_none(&state.bad),
            state.min_version.as_deref().unwrap_or("none"),
        ),
        0,
    )
}

#[cfg(not(windows))]
fn queue_update_requests(_requests: &[crate::service::update::store::Request], _what: &str) -> i32 {
    update_status()
}

#[cfg(not(windows))]
fn update_status() -> i32 {
    print("[OpenDesk] Auto-update is Windows only; update the package with your package manager.", 1)
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
        "[OpenDesk] {}\r\n  Device name : {}\r\n  Device ID   : {}\r\n  Device key  : {}\r\n  Relay       : {}\r\n\r\nMore commands: opendesk help",
        env!("CARGO_PKG_VERSION"),
        device::display_name(),
        desk_id::format(&cfg.desk_id),
        identity::local().map(|id| identity::fingerprint(&id.public)).unwrap_or_else(|| "unavailable".into()),
        cfg.gateway_url,
    ));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bare_words_count_as_mistyped_commands() {
        for word in ["updte", "statsu", "rollbak"] {
            assert!(is_command_word(word), "{word}");
        }
        for not_word in ["--background", "--id=123456789", r"C:\Users\x\file.txt", "./file", ""] {
            assert!(!is_command_word(not_word), "{not_word}");
        }
    }
}
