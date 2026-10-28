//! Persistence of settings and history under `%APPDATA%\OpenDesk` (Windows) or `~/.config/opendesk`.
//!
//! Both files are small JSON documents written atomically enough for a single process (the CLI and the
//! running app may both write `config.json`; the app polls it and picks up CLI changes).

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::domain::history::HistoryRecord;
use crate::domain::settings::Settings;
use crate::infra::{device, logger};

const SETTINGS_FILE: &str = "config.json";
const HISTORY_FILE: &str = "history.json";

/// `%APPDATA%\OpenDesk`, created on first use (falls back to the working directory).
#[cfg(windows)]
pub fn app_dir() -> PathBuf {
    match env::var("APPDATA") {
        Ok(appdata) => {
            let dir = PathBuf::from(appdata).join("OpenDesk");
            let _ = fs::create_dir_all(&dir);
            dir
        }
        Err(_) => PathBuf::from("."),
    }
}

/// `$XDG_CONFIG_HOME/opendesk` (normally `~/.config/opendesk`), created on first use (falls back to the
/// working directory).
#[cfg(not(windows))]
pub fn app_dir() -> PathBuf {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    match base {
        Some(base) => {
            let dir = base.join("opendesk");
            let _ = fs::create_dir_all(&dir);
            dir
        }
        None => PathBuf::from("."),
    }
}

fn read_json<T: DeserializeOwned>(name: &str) -> Option<T> {
    let data = fs::read_to_string(app_dir().join(name)).ok()?;
    match serde_json::from_str(&data) {
        Ok(value) => Some(value),
        Err(e) => {
            logger::warn("storage", &format!("Ignoring unreadable {name}: {e}"));
            None
        }
    }
}

/// Writes `data` to `<path>.tmp`, flushes it to disk and renames it over `path`, so a crash or a concurrent
/// reader never sees a half-written file. The temporary file is removed on failure.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    write_atomic_mode(path, data, false)
}

/// Like [`write_atomic`], for secrets: on Linux the file is created readable by its owner only (0600). On
/// Windows it inherits the ACL of the app directory in the user's profile (the user, SYSTEM, Administrators).
pub(crate) fn write_atomic_private(path: &Path, data: &[u8]) -> io::Result<()> {
    write_atomic_mode(path, data, true)
}

fn write_atomic_mode(path: &Path, data: &[u8], private: bool) -> io::Result<()> {
    // Unique per process and call: the CLI, the app and several threads may save the same file at once.
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}-{seq}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let result = options
        .open(&tmp)
        .and_then(|mut f| f.write_all(data).and_then(|_| f.sync_all()))
        .and_then(|_| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_json<T: Serialize + ?Sized>(name: &str, value: &T) {
    let result = serde_json::to_string_pretty(value)
        .map_err(|e| e.to_string())
        .and_then(|data| write_atomic(&app_dir().join(name), data.as_bytes()).map_err(|e| e.to_string()));
    if let Err(e) = result {
        logger::error("storage", &format!("Could not write {name}: {e}"));
    }
}

/// Loads settings, creating them (with a fresh desk ID) on first run and repairing invalid values.
pub fn load_settings() -> Settings {
    match read_json::<Settings>(SETTINGS_FILE) {
        Some(mut settings) => {
            if settings.repair(device::new_desk_id) {
                save_settings(&settings);
            }
            settings
        }
        None => {
            let settings = Settings::new(device::new_desk_id());
            save_settings(&settings);
            settings
        }
    }
}

/// Writes settings to disk.
pub fn save_settings(settings: &Settings) {
    write_json(SETTINGS_FILE, settings);
}

/// Loads the connection history (empty when missing or unreadable).
pub fn load_history() -> Vec<HistoryRecord> {
    read_json(HISTORY_FILE).unwrap_or_default()
}

/// Writes the connection history to disk.
pub fn save_history(list: &[HistoryRecord]) {
    write_json(HISTORY_FILE, list);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_tmp() {
        let dir = std::env::temp_dir().join(format!("opendesk-storage-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("config.json");

        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        let leftovers =
            fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().path().extension() == Some("tmp".as_ref()));
        assert_eq!(leftovers.count(), 0);

        let _ = fs::remove_dir_all(&dir);
    }
}
