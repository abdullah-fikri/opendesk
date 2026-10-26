//! What the logged-on user has on the clipboard. The service's SYSTEM agent sees the clipboard change (sequence
//! number) but Windows gives it none of the content, so it reads through `OpenDesk.exe --clipboard-read <out>`
//! started as the session user. Everywhere else this reads `core_engine::clipboard` directly. Writing works as
//! SYSTEM and stays in `core_engine::clipboard`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Text and file list on the clipboard; empty text is `None`.
#[derive(Default, Serialize, Deserialize)]
pub struct Contents {
    pub text: Option<String>,
    pub files: Vec<PathBuf>,
}

/// The clipboard as the logged-on user sees it.
pub fn read() -> Contents {
    #[cfg(windows)]
    if crate::infra::system_service::is_local_system() {
        return read_as_session_user();
    }
    read_here()
}

/// The clipboard as this process sees it (the `--clipboard-read` helper).
pub fn read_here() -> Contents {
    Contents { text: core_engine::clipboard::text().filter(|t| !t.is_empty()), files: core_engine::clipboard::files() }
}

/// Runs the helper as the session user. Its file lives in the user's own profile and only for the moment it
/// takes to read it (it may hold copied passwords).
#[cfg(windows)]
fn read_as_session_user() -> Contents {
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::infra::{logger, storage, system_service};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    // The clipboard watcher and Ctrl+V may read at the same time.
    let out = storage::app_dir().join(format!("clipboard-{}.json", NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_file(&out);
    let contents = system_service::run_as_session_user(&format!("--clipboard-read \"{}\"", out.display()))
        .then(|| std::fs::read(&out).ok())
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let _ = std::fs::remove_file(&out);
    contents.unwrap_or_else(|| {
        logger::warn("clipboard", "Could not read the clipboard as the logged-on user.");
        Contents::default()
    })
}
