//! Append-only trace log: `opendesk.trace` in the app directory (`%APPDATA%\OpenDesk`, `~/.config/opendesk`)
//! and stderr. Entries carry IDs, sizes and file names for diagnosis; callers must not log clipboard text, file
//! contents or credentials.

use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_LOCK: Mutex<()> = Mutex::new(());

/// Size past which the trace file is moved to `opendesk.trace.old` before the next write.
const TRACE_LIMIT: u64 = 10 * 1024 * 1024;

fn format_timestamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    // Compute UTC date/time components cleanly
    let days = secs / 86400;
    let rem = secs % 86400;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;

    // Civil date from days since 1970-01-01
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", year, m, d, hour, min, sec, millis)
}

#[cfg(not(windows))]
fn trace_path() -> PathBuf {
    crate::infra::storage::app_dir().join("opendesk.trace")
}

#[cfg(windows)]
fn trace_path() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let dir = PathBuf::from(appdata).join("OpenDesk");
        let _ = fs::create_dir_all(&dir);
        dir.join("opendesk.trace")
    } else {
        PathBuf::from("opendesk.trace")
    }
}

/// Moves `path` to `<path>.old` (replacing an older one) once it exceeds `limit` bytes. Errors are ignored:
/// the service and the user app share the file, and a rename blocked by the other process is retried on the
/// next write.
fn rotate_if_large(path: &Path, limit: u64) {
    if fs::metadata(path).map(|m| m.len() > limit).unwrap_or(false) {
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(path, &old);
    }
}

/// Writes one line `level timestamp component pid subsystem - msg` to stderr and the trace file.
pub fn log_trace(level: &str, component: &str, subsystem: &str, msg: &str) {
    let ts = format_timestamp();
    let pid = process::id();
    let line = format!("{:>7} {} {:>6} {:6} {:>32} - {}\n", level, ts, component, pid, subsystem, msg);

    if let Ok(_guard) = LOG_LOCK.lock() {
        let _ = std::io::stderr().write_all(line.as_bytes());
        let path = trace_path();
        rotate_if_large(&path, TRACE_LIMIT);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Logs an informational message.
pub fn info(subsystem: &str, msg: &str) {
    log_trace("info", "front", subsystem, msg);
}

/// Logs a recoverable problem.
pub fn warn(subsystem: &str, msg: &str) {
    log_trace("warning", "front", subsystem, msg);
}

/// Logs a failure.
pub fn error(subsystem: &str, msg: &str) {
    log_trace("error", "front", subsystem, msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_only_past_the_limit() {
        let dir = std::env::temp_dir().join(format!("opendesk-logger-test-{}", process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("opendesk.trace");
        let old = dir.join("opendesk.trace.old");
        let _ = fs::remove_file(&old);

        fs::write(&path, b"12345").unwrap();
        rotate_if_large(&path, 5);
        assert!(path.exists() && !old.exists());

        fs::write(&old, b"stale").unwrap();
        fs::write(&path, b"123456").unwrap();
        rotate_if_large(&path, 5);
        assert!(!path.exists());
        assert_eq!(fs::read(&old).unwrap(), b"123456");

        let _ = fs::remove_dir_all(&dir);
    }
}
