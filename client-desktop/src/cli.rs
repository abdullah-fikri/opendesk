//! Command-line entrypoint: `opendesk status|server|trust|stop|uninstall|version|help` (Windows and Linux), and
//! the update commands `update|rollback|pin|unpin` (Windows, handed to the service). The older spellings
//! (`--update-now`, `--rollback`, `--pin-version`, `--unpin`, `id`, `--status`, `kill`) keep working.
//!
//! The same executable is the GUI app; when the first argument is a known command it is handled here and the
//! process exits before any window, tray or network thread starts. The running app watches `config.json`, so
//! `opendesk server <url>` reconnects it within about a second. Text is built in `cli_ui` and written through
//! `infra::console`.

use std::env;

use crate::cli_ui::{self, Style};
use crate::domain::edition::Edition;
use crate::domain::{desk_id, settings};
use crate::infra::autostart::{self, StopError};
use crate::infra::{console, device, identity, key_pins, logger, storage, user_clipboard, window};

const HELP: &str = "Usage: opendesk <command>\n\n\
Commands:\n\
\x20 status                 Show this device's name, ID, key and relay\n\
\x20 server [url]           Show or set the relay address (e.g. wss://relay.example.com)\n\
\x20 trust <id>             Accept a host's changed device key\n\
\x20 stop                   Stop OpenDesk and its watchdog\n\
\x20 uninstall              Stop OpenDesk and remove autostart entries\n\
\x20 version                Show the installed version\n\
\x20 help                   Show this help\n\n\
Updates (Windows, run as Administrator):\n\
\x20 update [latest]        Install the newest release and follow new releases; shows the progress\n\
\x20 update <version>       Install that release, newer or older, and stay on it\n\
\x20 update status          Show the installed, pinned and kept versions and the last update\n\
\x20 rollback [version]     Go back to the previous (or a kept) version\n\
\x20 pin <version>          Install only that version from now on\n\
\x20 unpin                  Follow new releases again\n\
\x20 Add --force to update or rollback to go below the release's minimum version.\n\
\x20 Add --no-wait to update or rollback to return at once instead of showing the progress.";

/// Handles a CLI command and exits, or returns when the arguments are for the GUI app.
pub fn run_if_requested() {
    let args: Vec<String> = env::args().skip(1).collect();
    let first = args.first().map(|a| a.to_ascii_lowercase()).unwrap_or_default();
    let rest = args.get(1..).unwrap_or_default();

    let code = match first.as_str() {
        // A bare `opendesk` from a terminal shows the banner and starts the app as usual.
        "" => {
            let out = Out::open();
            if out.console.is_terminal() {
                out.banner(true);
                out.write("Starting OpenDesk. Commands: opendesk help\n\n");
            }
            #[cfg(windows)]
            console::detach();
            return;
        }
        "server" => set_server(rest.first().map(|a| a.trim_start_matches('-').to_string())),
        f if f.starts_with("--server=") => set_server(Some(args[0]["--server=".len()..].to_string())),
        "trust" => trust(rest.first()),
        "stop" | "kill" => stop(),
        "uninstall" => uninstall(),
        "status" | "id" | "--status" => status(),
        "version" | "--version" | "-v" => {
            let out = Out::open();
            out.banner(true);
            out.write(&format!("OpenDesk {}\n", env!("CARGO_PKG_VERSION")));
            0
        }
        "help" | "--help" | "-h" | "/?" => help(),
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
        f if is_command_word(f) => {
            let out = Out::open();
            out.write(&format!("Unknown command: {}\n\n", args[0]));
            out.write(&format!("{}\n", out.help_text()));
            2
        }
        _ => return,
    };
    std::process::exit(code);
}

/// A bare word such as `updte`: not an option and not a path.
fn is_command_word(arg: &str) -> bool {
    !arg.is_empty() && !arg.starts_with('-') && arg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The console of this command and how to draw on it.
struct Out {
    console: console::Console,
    style: Style,
}

impl Out {
    fn open() -> Out {
        let console = console::open();
        let style = Style::new(console.ansi() && !console::no_color(), console.unicode());
        Out { console, style }
    }

    fn write(&self, text: &str) {
        self.console.write(text);
    }

    /// The logo with version, edition and (`with_id`) this device's desk ID, followed by a blank line.
    fn banner(&self, with_id: bool) {
        let id = with_id.then(|| desk_id::format(&storage::load_settings().desk_id));
        let text = cli_ui::banner(&self.style, env!("CARGO_PKG_VERSION"), Edition::current(), id.as_deref());
        self.write(&format!("\n{text}\n\n"));
    }

    fn help_text(&self) -> String {
        HELP.lines()
            .map(
                |line| {
                    if line.ends_with(':') && !line.starts_with(' ') { self.style.bold(line) } else { line.to_string() }
                },
            )
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A message and the exit code, for the one-shot commands.
    fn done(&self, text: &str, code: i32) -> i32 {
        self.write(&format!("{text}\n"));
        code
    }
}

fn help() -> i32 {
    let out = Out::open();
    out.banner(true);
    out.write(&format!("{}\n", out.help_text()));
    0
}

fn set_server(url: Option<String>) -> i32 {
    let out = Out::open();
    let mut cfg = storage::load_settings();
    let Some(raw) = url.filter(|u| !u.is_empty()) else {
        return out.done(
            &format!(
                "[OpenDesk]\n  Device ID : {}\n  Relay     : {}\n  Change it : opendesk server wss://relay.example.com",
                desk_id::format(&cfg.desk_id),
                cfg.gateway_url
            ),
            0,
        );
    };
    cfg.gateway_url = settings::normalize_gateway_url(&raw);
    storage::save_settings(&cfg);
    logger::info("cli", &format!("Gateway URL updated via CLI to: {}", cfg.gateway_url));
    out.done(
        &format!(
            "[OpenDesk] Relay updated.\n  Relay     : {}\n  Device ID : {}\n  The running app reconnects automatically.",
            cfg.gateway_url,
            desk_id::format(&cfg.desk_id)
        ),
        0,
    )
}

/// Accepts a host's new key: forgets the pinned one, so the next session pins the key it presents.
fn trust(id: Option<&String>) -> i32 {
    let out = Out::open();
    let clean = id.map(|s| desk_id::clean(s)).unwrap_or_default();
    if !desk_id::is_valid(&clean) {
        return out.done("[OpenDesk] Usage: opendesk trust <device ID>", 2);
    }
    let text = if key_pins::forget(&clean) {
        format!(
            "[OpenDesk] Forgot the key of {}. The next connection trusts the key it presents.",
            desk_id::format(&clean)
        )
    } else {
        format!("[OpenDesk] No key of {} was pinned.", desk_id::format(&clean))
    };
    out.done(&text, 0)
}

fn stop() -> i32 {
    let out = Out::open();
    match autostart::stop_running() {
        Ok(()) => out.done(
            if cfg!(windows) {
                "[OpenDesk] OpenDesk and its watchdog were stopped."
            } else {
                "[OpenDesk] OpenDesk was stopped."
            },
            0,
        ),
        Err(StopError::AccessDenied) => out.done(
            if cfg!(windows) {
                "[OpenDesk] Failed: OpenDesk runs as Administrator.\n  Run this terminal as Administrator, or use the tray menu \"Stop & Exit\"."
            } else {
                "[OpenDesk] Failed: the running OpenDesk belongs to another user.\n  Run this command as that user, or use the tray menu \"Stop & Exit\"."
            },
            1,
        ),
    }
}

fn uninstall() -> i32 {
    let out = Out::open();
    autostart::remove_autostart();
    out.done(
        if cfg!(windows) {
            "[OpenDesk] Processes stopped and autostart entries removed.\nRun the uninstaller or delete the installation folder."
        } else {
            "[OpenDesk] OpenDesk stopped and its start-at-login entry removed.\nRemove the package (sudo apt remove opendesk) or delete the binary."
        },
        0,
    )
}

/// `update [latest|<version>|status] [--force] [--no-wait]`.
fn update(rest: &[String]) -> i32 {
    use crate::service::update::store::Request;

    let out = Out::open();
    let force = rest.iter().any(|a| a == "--force");
    let wait = !rest.iter().any(|a| a == "--no-wait");
    match rest.iter().find(|a| !a.starts_with("--")).map(|a| a.to_ascii_lowercase()).as_deref() {
        None | Some("latest") | Some("now") => {
            out.banner(true);
            let job = cli_ui::Job::Update { target: None };
            run_update_job(&out, &[Request::Unpin, Request::UpdateNow], job, wait)
        }
        Some("status") => update_status(&out),
        Some(v) => match parse_version(&out, v) {
            Some(version) => {
                out.banner(true);
                let job = cli_ui::Job::Update { target: Some(version.clone()) };
                run_update_job(&out, &[Request::Install { version, force }], job, wait)
            }
            None => 2,
        },
    }
}

/// `rollback [version] [--force] [--no-wait]`, `pin <version>`, `unpin`: queued for the service, which carries
/// them out (see `service::update`). Only administrators can write the update directory.
fn update_request(command: &str, rest: &[String]) -> i32 {
    use crate::service::update::store::Request;

    let out = Out::open();
    let version = match rest.iter().find(|a| !a.starts_with("--")) {
        Some(raw) => match parse_version(&out, raw) {
            Some(v) => Some(v),
            None => return 2,
        },
        None => None,
    };
    let wait = !rest.iter().any(|a| a == "--no-wait");
    let (request, job) = match command {
        "rollback" => {
            let force = rest.iter().any(|a| a == "--force");
            (Request::Rollback { version: version.clone(), force }, cli_ui::Job::Rollback { target: version })
        }
        "pin" | "pin-version" => match version {
            Some(version) => (Request::Pin { version: version.clone() }, cli_ui::Job::Pin { version }),
            None => return out.done("[OpenDesk] Usage: opendesk pin 1.2.3", 2),
        },
        _ => (Request::Unpin, cli_ui::Job::Unpin),
    };
    out.banner(true);
    run_update_job(&out, &[request], job, wait)
}

/// A version argument (`1.2.3` or `v1.2.3`), normalized; prints the expected form when it is not one.
fn parse_version(out: &Out, raw: &str) -> Option<String> {
    use crate::service::update::manifest::Version;

    let parsed = Version::parse(raw).map(|v| v.to_string());
    if parsed.is_none() {
        out.write(&format!("[OpenDesk] \"{raw}\" is not a version; versions look like 1.2.3.\n"));
    }
    parsed
}

/// The update directory, or None (printed) when the OpenDesk service does not manage updates here.
#[cfg(windows)]
fn update_store(out: &Out) -> Option<crate::service::update::store::Store> {
    use crate::service::update::{secure_dir, store::Store};

    let root = secure_dir::root();
    // Not found (as opposed to access denied): the service never prepared it.
    let missing = std::fs::metadata(&root).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    if !crate::infra::system_service::installed() || missing {
        out.write(
            "[OpenDesk] Updates are managed by the OpenDesk service, which is not installed here.\n  Install OpenDesk with its setup program; the portable .exe does not update itself.\n",
        );
        return None;
    }
    Some(Store::new(root))
}

/// Queues `requests` and, unless `--no-wait`, watches the service carry out `job` and draws its steps.
#[cfg(windows)]
fn run_update_job(out: &Out, requests: &[crate::service::update::store::Request], job: cli_ui::Job, wait: bool) -> i32 {
    use crate::service::update::now_unix_ms;

    let Some(store) = update_store(out) else { return 1 };
    let queued_ms = now_unix_ms();
    let base = u128::from(queued_ms) * 1_000_000;
    // Consecutive names keep the order: the service handles requests oldest first.
    for (i, request) in requests.iter().enumerate() {
        match store.push_request(request, base + i as u128) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return out.done("[OpenDesk] Failed: run this terminal as Administrator.", 1);
            }
            Err(e) => return out.done(&format!("[OpenDesk] Failed: {e}"), 1),
        }
    }
    if !wait {
        return out.done(
            "Request queued. The OpenDesk service acts within a few seconds and installs once no session is active.\nProgress: opendesk update status",
            0,
        );
    }
    let last_request = store.request_path(base + (requests.len() - 1) as u128);
    watch(out, &store, &job, &last_request, queued_ms)
}

/// Redraws the step list every 250 ms from `progress.json` until the job ends, stalls or Ctrl+C is pressed.
/// The display is rewritten in place on an ANSI terminal and appended line by line elsewhere.
#[cfg(windows)]
fn watch(
    out: &Out,
    store: &crate::service::update::store::Store,
    job: &cli_ui::Job,
    last_request: &std::path::Path,
    queued_ms: u64,
) -> i32 {
    use std::time::{Duration, Instant};

    use crate::service::update::now_unix_ms;
    use cli_ui::{Outcome, RateMeter, Seen};

    const POLL: Duration = Duration::from_millis(250);

    console::watch_ctrl_c();
    let started = Instant::now();
    let mut picked_up_at: Option<Instant> = None;
    let mut rate = RateMeter::default();
    let mut drawn: Vec<String> = Vec::new();
    let mut last_active = 0;
    let mut frame = 0;
    let mut last_plain: Vec<String> = Vec::new();
    let redraw_in_place = out.style_in_place();

    loop {
        if picked_up_at.is_none() && !last_request.exists() {
            picked_up_at = Some(Instant::now());
        }
        // Only a file written after this command queued its request speaks about it.
        let progress = store.progress().filter(|p| p.updated_at >= queued_ms);
        if let Some(p) = &progress
            && p.phase == crate::service::update::store::Phase::Downloading
        {
            rate.push(now_unix_ms(), p.bytes);
        }
        let seen = Seen {
            picked_up: picked_up_at.is_some(),
            progress: progress.as_ref(),
            rate: rate.rate(),
            current: env!("CARGO_PKG_VERSION"),
            last_active,
        };
        let view = cli_ui::view(job, &seen, &out.style);
        last_active = view.active;
        let lines = cli_ui::render_steps(&view.steps, &out.style, frame, console::width());
        frame += 1;

        if redraw_in_place {
            // Move up over what was drawn and clear each line before rewriting it.
            let mut text = String::new();
            if !drawn.is_empty() {
                text.push_str(&format!("\x1b[{}A", drawn.len()));
            }
            for line in &lines {
                text.push_str(&format!("\x1b[2K{line}\n"));
            }
            out.write(&text);
            drawn = lines;
        } else {
            // No cursor control: print only the lines whose state changed (the spinner frame does not count).
            let plain: Vec<String> =
                view.steps.iter().map(|s| format!("{:?} {} {}", s.mark, s.label, s.detail)).collect();
            for (i, line) in lines.iter().enumerate() {
                if last_plain.get(i) != plain.get(i) && view.steps[i].mark != cli_ui::Mark::Pending {
                    out.write(&format!("{line}\n"));
                }
            }
            last_plain = plain;
        }

        match view.outcome {
            Outcome::Finished { ok, message } => {
                out.write(&format!("\n{message}\n"));
                return i32::from(!ok);
            }
            Outcome::Watching => {}
        }
        if console::ctrl_c_pressed() {
            out.write(
                "\nStopped watching; the update continues in the OpenDesk service.\nProgress: opendesk update status\n",
            );
            return 0;
        }
        let since_pickup = picked_up_at.map(|t| t.elapsed());
        if let Some(hint) = cli_ui::stalled(job, started.elapsed(), since_pickup, progress.is_some()) {
            out.write(&format!("\n{hint}\n"));
            return 1;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(windows)]
impl Out {
    /// Whether the step list can be rewritten in place (cursor movement on an interactive terminal).
    fn style_in_place(&self) -> bool {
        self.console.ansi() && self.console.is_terminal()
    }
}

/// `update status`: what the service knows about versions on this machine, the last update phase and the
/// check schedule.
#[cfg(windows)]
fn update_status(out: &Out) -> i32 {
    use crate::service::update::{now_unix, now_unix_ms};

    let Some(store) = update_store(out) else { return 1 };
    out.banner(true);
    let state = store.state();
    let kept: Vec<String> = store.installed_versions().iter().map(|v| v.to_string()).collect();
    let queued = std::fs::read_dir(store.root().join("requests")).is_ok_and(|mut d| d.next().is_some());
    let pending = match store.pending() {
        Some(p) => format!("{p:?}"),
        None if queued => "request queued".into(),
        None => "none".into(),
    };
    let progress = store.progress().unwrap_or_default();
    let now = now_unix() as i64;
    let when = |at: Option<u64>| match at {
        Some(t) => cli_ui::relative(t as i64 - now),
        None => "never (the service has not started a check yet)".to_string(),
    };
    let or_none = |list: &[String]| if list.is_empty() { "none".to_string() } else { list.join(", ") };
    out.done(
        &format!(
            "Updates\n  Installed   : {}\n  Pinned to   : {}\n  Kept        : {}\n  Marked bad  : {}\n  Min version : {}\n  Last update : {}\n  Last check  : {}\n  Next check  : {}\n  In progress : {pending}\n  Releases    : https://github.com/abdullah-fikri/opendesk/releases",
            env!("CARGO_PKG_VERSION"),
            state.pinned.as_deref().unwrap_or("none (follows new releases)"),
            or_none(&kept),
            or_none(&state.bad),
            state.min_version.as_deref().unwrap_or("none"),
            cli_ui::phase_summary(&progress, now_unix_ms()),
            when(progress.checked_at),
            when(progress.next_check_at),
        ),
        0,
    )
}

#[cfg(not(windows))]
fn run_update_job(
    out: &Out,
    _requests: &[crate::service::update::store::Request],
    _job: cli_ui::Job,
    _wait: bool,
) -> i32 {
    update_status(out)
}

#[cfg(not(windows))]
fn update_status(out: &Out) -> i32 {
    out.done("[OpenDesk] Auto-update is Windows only; update the package with your package manager.", 1)
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
    let out = Out::open();
    let cfg = storage::load_settings();
    out.banner(false);
    out.done(
        &format!(
            "  Device name : {}\n  Device ID   : {}\n  Device key  : {}\n  Relay       : {}\n\nMore commands: opendesk help",
            device::display_name(),
            desk_id::format(&cfg.desk_id),
            identity::local().map(|id| identity::fingerprint(&id.public)).unwrap_or_else(|| "unavailable".into()),
            cfg.gateway_url,
        ),
        0,
    )
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
