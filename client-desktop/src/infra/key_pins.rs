//! Host keys this viewer trusts, pinned on first use (`known_hosts.json` in the app directory): host clean
//! desk ID -> base64 X25519 public key. See `docs/features/e2e.md`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use core_engine::codec::{from_base64, to_base64};

use crate::infra::{logger, storage};

const PINS_FILE: &str = "known_hosts.json";

/// Result of comparing a host's key with the pinned one.
#[derive(Debug, PartialEq, Eq)]
pub enum Pin {
    /// First contact: the key is now pinned.
    New,
    /// The pinned key.
    Match,
    /// A different key than the pinned one (`pinned` is the old key).
    Mismatch { pinned: Vec<u8> },
}

/// Serializes read-modify-write cycles of the file within this process (several sessions may connect at once).
static LOCK: Mutex<()> = Mutex::new(());

fn path() -> PathBuf {
    storage::app_dir().join(PINS_FILE)
}

fn load(path: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(path: &Path, pins: &BTreeMap<String, String>) {
    let result = serde_json::to_string_pretty(pins)
        .map_err(|e| e.to_string())
        .and_then(|t| storage::write_atomic(path, t.as_bytes()).map_err(|e| e.to_string()));
    if let Err(e) = result {
        logger::error("e2e", &format!("Could not store pinned host keys: {e}"));
    }
}

/// Checks `key` for host `id`, pinning it when the host has none yet.
pub fn check(id: &str, key: &[u8]) -> Pin {
    check_at(&path(), id, key)
}

/// Pinned key of host `id`, if any.
pub fn pinned(id: &str) -> Option<Vec<u8>> {
    load(&path()).get(id).and_then(|k| from_base64(k))
}

/// Forgets the key of host `id`: the next session pins whatever key it presents. True if one was pinned.
pub fn forget(id: &str) -> bool {
    forget_at(&path(), id)
}

fn check_at(path: &Path, id: &str, key: &[u8]) -> Pin {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut pins = load(path);
    match pins.get(id).and_then(|k| from_base64(k)) {
        Some(pinned) if pinned == key => Pin::Match,
        Some(pinned) => Pin::Mismatch { pinned },
        None => {
            pins.insert(id.to_string(), to_base64(key));
            save(path, &pins);
            Pin::New
        }
    }
}

fn forget_at(path: &Path, id: &str) -> bool {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut pins = load(path);
    let had = pins.remove(id).is_some();
    if had {
        save(path, &pins);
    }
    had
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_key_is_pinned_and_a_different_one_is_a_mismatch() {
        let dir = std::env::temp_dir().join(format!("opendesk-pins-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(PINS_FILE);
        let _ = fs::remove_file(&path);
        assert_eq!(check_at(&path, "111111111", &[1; 32]), Pin::New);
        assert_eq!(check_at(&path, "111111111", &[1; 32]), Pin::Match);
        assert_eq!(check_at(&path, "111111111", &[2; 32]), Pin::Mismatch { pinned: vec![1; 32] });
        assert_eq!(check_at(&path, "222222222", &[2; 32]), Pin::New, "pins are per host");
        // Accepting the new key: forget, then the next check pins it.
        assert!(forget_at(&path, "111111111") && !forget_at(&path, "111111111"));
        assert_eq!(check_at(&path, "111111111", &[2; 32]), Pin::New);
        assert_eq!(check_at(&path, "111111111", &[2; 32]), Pin::Match);
        let _ = fs::remove_dir_all(&dir);
    }
}
