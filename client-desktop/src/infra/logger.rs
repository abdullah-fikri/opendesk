//! Append-only trace log: `opendesk.trace` in the app directory (`%APPDATA%\OpenDesk`, `~/.config/opendesk`)
//! and stderr. Entries carry IDs, sizes and file names for diagnosis; callers must not log clipboard text, file
//! contents or credentials.

use std::any::Any;
use std::backtrace::Backtrace;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

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

/// One line `level timestamp component pid subsystem - msg`.
fn format_line(level: &str, component: &str, subsystem: &str, msg: &str) -> String {
    let ts = format_timestamp();
    let pid = process::id();
    format!("{:>7} {} {:>6} {:6} {:>32} - {}\n", level, ts, component, pid, subsystem, msg)
}

/// Writes `line` to stderr and the trace file; the caller holds `LOG_LOCK` (or could not get it).
fn write_line(line: &str) {
    let _ = std::io::stderr().write_all(line.as_bytes());
    let path = trace_path();
    rotate_if_large(&path, TRACE_LIMIT);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Writes one line `level timestamp component pid subsystem - msg` to stderr and the trace file.
pub fn log_trace(level: &str, component: &str, subsystem: &str, msg: &str) {
    let line = format_line(level, component, subsystem, msg);
    let _guard = LOG_LOCK.lock();
    write_line(&line);
}

/// `error` for the panic hook: never waits long for `LOG_LOCK` (the panicking thread may hold it) and still
/// writes when the lock stays busy.
fn error_unblocked(subsystem: &str, msg: &str) {
    let line = format_line("error", "front", subsystem, msg);
    for _ in 0..50 {
        if let Some(_guard) = LOG_LOCK.try_lock() {
            return write_line(&line);
        }
        thread::sleep(Duration::from_millis(2));
    }
    write_line(&line);
}

/// Text of a panic payload (`panic!` with a literal or a formatted message).
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    match payload.downcast_ref::<&str>() {
        Some(s) => s,
        None => payload.downcast_ref::<String>().map_or("<non-string payload>", String::as_str),
    }
}

/// The trace entry for a panic: thread, location, message and backtrace.
fn panic_report(thread: Option<&str>, location: &str, message: &str, backtrace: &Backtrace) -> String {
    format!("Thread '{}' panicked at {location}: {message}\n{backtrace}", thread.unwrap_or("<unnamed>"))
}

/// Logs every panic (thread name, location, message, backtrace) to the trace log, then runs the previous
/// hook. The panic still ends only its own thread.
pub fn install_panic_hook() {
    install_panic_hook_with(error_unblocked);
}

fn install_panic_hook_with(log: fn(&str, &str)) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        let location = info.location().map(|l| l.to_string()).unwrap_or_default();
        let current = thread::current();
        let backtrace = Backtrace::force_capture();
        log("panic", &panic_report(current.name(), &location, panic_message(info.payload()), &backtrace));
        previous(info);
    }));
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

    static PANIC_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn record(subsystem: &str, msg: &str) {
        PANIC_LOG.lock().push(format!("{subsystem}: {msg}"));
    }

    #[test]
    fn a_panic_in_a_thread_is_logged() {
        install_panic_hook_with(record);
        let result = crate::infra::thread::spawn_named("test-panicking", || panic!("deliberate {}", 42)).join();
        let _ = panic::take_hook(); // back to the default hook
        assert!(result.is_err());
        let log = PANIC_LOG.lock();
        let entry = log.iter().find(|e| e.contains("deliberate 42")).expect("panic logged");
        assert!(entry.starts_with("panic: Thread 'test-panicking' panicked at "), "{entry}");
        assert!(entry.contains("logger.rs"), "{entry}");
    }

    #[test]
    fn panic_payloads_and_unnamed_threads() {
        assert_eq!(panic_message(&"literal"), "literal");
        assert_eq!(panic_message(&String::from("formatted")), "formatted");
        assert_eq!(panic_message(&7u8), "<non-string payload>");
        let report = panic_report(None, "a.rs:1:2", "boom", &Backtrace::disabled());
        assert!(report.starts_with("Thread '<unnamed>' panicked at a.rs:1:2: boom"));
    }

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
