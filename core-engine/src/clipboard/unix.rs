//! Clipboard text through `arboard` (X11 selections, or Wayland data-control where the compositor offers it).
//! File lists are Windows-only (Explorer formats): [`files`] is always empty and [`set_files`] does nothing,
//! so copied files travel through the file-transfer path instead.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Mutex;

/// One clipboard handle for the process: on X11 the selection is served by its background thread, so it must
/// outlive `set_text`.
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

fn with_clipboard<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Option<T>) -> Option<T> {
    let mut slot = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        *slot = arboard::Clipboard::new().ok();
    }
    f(slot.as_mut()?)
}

/// Changes whenever the clipboard text changes. There is no system counter on X11/Wayland, so this is a hash
/// of the current text (0 when empty or unavailable).
pub fn sequence_number() -> u32 {
    text().map_or(0, |t| {
        let mut h = DefaultHasher::new();
        t.hash(&mut h);
        (h.finish() as u32).max(1)
    })
}

/// Current clipboard text (up to 64K characters), if any.
pub fn text() -> Option<String> {
    with_clipboard(|c| c.get_text().ok()).map(|mut t| {
        if let Some((i, _)) = t.char_indices().nth(65536) {
            t.truncate(i);
        }
        t
    })
}

/// Files on the clipboard: not supported outside Windows.
pub fn files() -> Vec<PathBuf> {
    Vec::new()
}

/// Replaces the clipboard with `text`.
pub fn set_text(text: &str) {
    with_clipboard(|c| c.set_text(text.to_owned()).ok());
}

/// Putting a file list on the clipboard is Windows-only; received files stay in the download folder.
pub fn set_files(_file_paths: &[String]) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an X server (e.g. `xvfb-run cargo test -- --ignored`).
    #[test]
    #[ignore]
    fn text_round_trips_live() {
        set_text("opendesk clipboard ✓");
        assert_eq!(text().as_deref(), Some("opendesk clipboard ✓"));
        assert_ne!(sequence_number(), 0);
        assert!(files().is_empty());
    }
}
