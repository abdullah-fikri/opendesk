//! Persistence of settings and history under `%APPDATA%\OpenDesk` (Windows) or `~/.config/opendesk`.
//!
//! Both files are small JSON documents written atomically enough for a single process (the CLI and the
//! running app may both write `config.json`; the app polls it and picks up CLI changes).

use std::env;
use std::fs;
use std::path::PathBuf;

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

fn write_json<T: Serialize + ?Sized>(name: &str, value: &T) {
    let result = serde_json::to_string_pretty(value)
        .map_err(|e| e.to_string())
        .and_then(|data| fs::write(app_dir().join(name), data).map_err(|e| e.to_string()));
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
