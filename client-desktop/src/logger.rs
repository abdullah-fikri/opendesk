use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_LOCK: Mutex<()> = Mutex::new(());

fn format_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
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

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        year, m, d, hour, min, sec, millis
    )
}

fn trace_path() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let dir = PathBuf::from(appdata).join("OpenDesk");
        let _ = fs::create_dir_all(&dir);
        dir.join("opendesk.trace")
    } else {
        PathBuf::from("opendesk.trace")
    }
}

pub fn log_trace(level: &str, component: &str, subsystem: &str, msg: &str) {
    let ts = format_timestamp();
    let pid = process::id();
    let line = format!(
        "{:>7} {} {:>6} {:6} {:>32} - {}\n",
        level, ts, component, pid, subsystem, msg
    );

    if let Ok(_guard) = LOG_LOCK.lock() {
        let _ = std::io::stderr().write_all(line.as_bytes());
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(trace_path())
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

pub fn info(subsystem: &str, msg: &str) {
    log_trace("info", "front", subsystem, msg);
}

pub fn warn(subsystem: &str, msg: &str) {
    log_trace("warning", "front", subsystem, msg);
}

pub fn error(subsystem: &str, msg: &str) {
    log_trace("error", "front", subsystem, msg);
}
