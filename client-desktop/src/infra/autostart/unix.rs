//! Process lifecycle outside Windows: one instance per user (a lock file, plus a Unix socket through which a
//! second launch wakes the window and `opendesk stop` ends the app) and starting at login through an XDG
//! autostart entry. There is no watchdog: the desktop session starts OpenDesk at login.

use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::infra::logger;
use crate::infra::storage::app_dir;

/// Installed by the Linux package for every user; a per-user entry would start a second copy.
const SYSTEM_AUTOSTART: &str = "/etc/xdg/autostart/opendesk.desktop";
const USER_AUTOSTART: &str = "opendesk.desktop";

/// Writes `msg` to standard output (the CLI shares its messages with Windows, which uses CRLF).
pub fn print_to_console(msg: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "\n{}\n", msg.replace("\r\n", "\n"));
    let _ = out.flush();
}

/// Why stopping the running instance failed.
#[derive(Debug, PartialEq, Eq)]
pub enum StopError {
    /// The running instance belongs to another user.
    AccessDenied,
}

fn socket_path() -> PathBuf {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let dir = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(env::temp_dir);
    dir.join(format!("opendesk-{uid}.sock"))
}

/// Sends one command to the running instance; false when none is listening.
fn send_command(cmd: &str) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket_path())?;
    stream.write_all(cmd.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    // Wait until the instance handled it (it closes the stream, or exits).
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let _ = stream.read(&mut [0u8; 1]);
    Ok(())
}

/// Stops the running OpenDesk of this user (`opendesk stop`).
pub fn stop_running() -> Result<(), StopError> {
    match send_command("quit") {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(StopError::AccessDenied),
        // Not running (no socket, or a stale one) counts as stopped.
        _ => Ok(()),
    }
}

fn user_autostart_file() -> Option<PathBuf> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("autostart").join(USER_AUTOSTART))
}

/// Stops OpenDesk and removes this user's autostart entry (`opendesk uninstall`).
pub fn remove_autostart() {
    let _ = stop_running();
    if let Some(file) = user_autostart_file() {
        let _ = fs::remove_file(file);
    }
}

/// Ensures only one OpenDesk runs per user. A second launch asks the first to show its window (unless it is a
/// background start) and exits.
pub fn ensure_single_instance(start_in_background: bool) {
    let lock_path = app_dir().join("instance.lock");
    let Ok(lock) = File::options().create(true).truncate(false).write(true).open(&lock_path) else { return };
    // SAFETY: flock on a descriptor we own; the lock lives as long as the process keeps the file open.
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        if !start_in_background && !cfg!(feature = "headless") {
            let _ = send_command("show");
        }
        std::process::exit(0);
    }
    // Held until the process exits.
    std::mem::forget(lock);

    let path = socket_path();
    let _ = fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(listener) => {
            thread::spawn(move || {
                for mut stream in listener.incoming().flatten() {
                    let mut cmd = String::new();
                    let _ = (&mut stream).take(16).read_to_string(&mut cmd);
                    match cmd.trim() {
                        "show" => crate::infra::tray::request_show(),
                        "quit" => {
                            logger::info("app.service", "Stop requested from the command line.");
                            mark_intentional_stop();
                            let _ = fs::remove_file(socket_path());
                            std::process::exit(0);
                        }
                        _ => {}
                    }
                }
            });
        }
        Err(e) => logger::warn("app.service", &format!("Single-instance socket unavailable: {e}")),
    }
}

/// Marks an intentional user stop ("Stop & Exit"), mirroring Windows where it pauses the watchdog.
pub fn mark_intentional_stop() {
    let _ = fs::write(app_dir().join("user_exit.flag"), "stopped");
}

/// Windows only: the SYSTEM service restarts its agent. Nothing supervises the app here.
pub fn supervised_by_service() -> bool {
    false
}

/// Quotes a path for a desktop entry's `Exec` key.
fn exec_quote(path: &Path) -> String {
    let mut out = String::from("\"");
    for ch in path.to_string_lossy().chars() {
        if matches!(ch, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Registers start at login (an XDG autostart entry that starts OpenDesk in the tray), unless the package
/// installed one for every user or `OPENDESK_NO_AUTOSTART` is set. Development builds never register.
pub fn init_background_guardian() {
    let _ = fs::remove_file(app_dir().join("user_exit.flag"));
    if Path::new(SYSTEM_AUTOSTART).exists() || env::var_os("OPENDESK_NO_AUTOSTART").is_some() {
        return;
    }
    // An AppImage runs from a temporary mount: register the AppImage file itself.
    let Some(exe) = env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| env::current_exe().ok()) else { return };
    if exe.components().any(|c| c.as_os_str() == "target") {
        return;
    }
    let Some(file) = user_autostart_file() else { return };
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=OpenDesk\nComment=Self-hosted remote desktop (starts in the tray)\n\
         Exec={} --background\nIcon=opendesk\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
        exec_quote(&exe)
    );
    if fs::read_to_string(&file).ok().as_deref() == Some(entry.as_str()) {
        return;
    }
    let written = file.parent().is_some_and(|d| fs::create_dir_all(d).is_ok()) && fs::write(&file, entry).is_ok();
    if written {
        logger::info("app.service", &format!("Start at login registered: {}", file.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_paths_are_quoted() {
        assert_eq!(exec_quote(Path::new("/opt/Open Desk/opendesk")), "\"/opt/Open Desk/opendesk\"");
        assert_eq!(exec_quote(Path::new("/a/$x\"y")), "\"/a/\\$x\\\"y\"");
    }
}
