//! What the logged-on user has on the clipboard. The service's SYSTEM agent sees the clipboard change (sequence
//! number) but Windows gives it none of the content, so it reads through `OpenDesk.exe --clipboard-read <out>`
//! started as the session user. Everywhere else this reads `core_engine::clipboard` directly. Writing works as
//! SYSTEM and stays in `core_engine::clipboard`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Text, file list and image on the clipboard; empty text is `None`.
#[derive(Default, Serialize, Deserialize)]
pub struct Contents {
    pub text: Option<String>,
    pub files: Vec<PathBuf>,
    /// A copied image as PNG (`domain::clip_image`), read only when there is neither text nor files. Base64 in
    /// the helper's file.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "png_b64")]
    pub image: Option<Vec<u8>>,
}

mod png_b64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(png: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match png {
            Some(bytes) => s.serialize_str(&core_engine::codec::to_base64(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        Ok(Option::<String>::deserialize(d)?.and_then(|b| core_engine::codec::from_base64(&b)))
    }
}

/// The clipboard as the logged-on user sees it; with `image`, a copied image too (it costs a PNG encode).
pub fn read(image: bool) -> Contents {
    #[cfg(windows)]
    if crate::infra::system_service::is_local_system() {
        return read_as_session_user(image);
    }
    read_here(image)
}

/// The clipboard as this process sees it (the `--clipboard-read` helper).
pub fn read_here(image: bool) -> Contents {
    let text = core_engine::clipboard::text().filter(|t| !t.is_empty());
    let files = core_engine::clipboard::files();
    let image = if image && text.is_none() && files.is_empty() {
        core_engine::clipboard::image().and_then(crate::domain::clip_image::to_png)
    } else {
        None
    };
    Contents { text, files, image }
}

/// Runs the helper as the session user. Its file lives in the user's own profile and only for the moment it
/// takes to read it (it may hold copied passwords).
#[cfg(windows)]
fn read_as_session_user(image: bool) -> Contents {
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::infra::{logger, storage, system_service};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    // The clipboard watcher and Ctrl+V may read at the same time.
    let out = storage::app_dir().join(format!("clipboard-{}.json", NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_file(&out);
    let contents = system_service::run_as_session_user(&format!(
        "--clipboard-read \"{}\"{}",
        out.display(),
        if image { " image" } else { "" }
    ))
    .then(|| std::fs::read(&out).ok())
    .flatten()
    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let _ = std::fs::remove_file(&out);
    contents.unwrap_or_else(|| {
        logger::warn("clipboard", "Could not read the clipboard as the logged-on user.");
        Contents::default()
    })
}
