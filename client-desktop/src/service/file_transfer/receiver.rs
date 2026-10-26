//! Receiving files and folders. Bytes are staged in a `.part` file, verified (size + SHA-256) and only then
//! linked under their final name in `Downloads\OpenDesk`; a received file never overwrites an existing one.

use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use core_engine::codec::from_base64;
use sha2::{Digest, Sha256};

use super::tree::{create_dir_unique, link_unique};
use super::{CHUNK_SIZE, LAST_RECEIVED_MS, MAX_CHUNK_B64, hex, now_ms, remember_received};
use crate::domain::transfer::{
    MAX_FILE_BYTES, MAX_TREE_BYTES, MAX_TREE_ENTRIES, is_safe_id, percent, safe_rel_path, sanitize_file_name,
};
use crate::infra::{logger, storage, virtual_clipboard, window};
use crate::protocol::{FileAction, FilePayload};
use crate::service::context::AppContext;
use crate::service::ui_port::{TransferState, TransferUpdate, UiPort};

/// Incoming transfers with no chunk for this long are abandoned and their staging file deleted.
const RECEIVE_IDLE: Duration = Duration::from_secs(60);
/// Completed transfer IDs are remembered this long to reject replays.
const COMPLETED_TTL: Duration = Duration::from_secs(600);
/// Most files received at once.
const MAX_ACTIVE_FILES: usize = 8;
/// Most folder transfers received at once.
const MAX_ACTIVE_GROUPS: usize = 4;
/// Longest plain file name accepted.
const MAX_NAME_LEN: usize = 200;
/// Win32 virtual-key code of V.
const VK_V: u8 = 0x56;
/// Wait between replacing the clipboard and the auto-paste Ctrl+V.
const PASTE_SETTLE: Duration = Duration::from_millis(150);

/// What to tell the user and the sender about one payload: `(status line, ack)`.
/// `ack` is `Some(ok)` when the sender must be told the final result.
pub type Outcome = (Option<String>, Option<bool>);

struct Incoming {
    file: File,
    path: PathBuf,
    name: String,
    received: u64,
    total: u64,
    hash: Sha256,
    last_activity: Instant,
    /// Percentage last shown in the transfer list.
    shown_percent: u64,
    /// Files inside a folder transfer: (target folder, file name, group key). The clipboard is only updated
    /// once the whole group has arrived.
    dest: Option<(PathBuf, String, String)>,
}

/// A folder transfer in progress (`group` on start messages).
struct FolderGroup {
    /// Top-level items as saved (folders renamed "name (n)" on conflict), in arrival order, for the clipboard.
    tops: Vec<PathBuf>,
    /// Sender's top-level folder name (lowercase) -> folder created for it.
    roots: HashMap<String, PathBuf>,
    entries: usize,
    bytes: u64,
    last_activity: Instant,
}

impl FolderGroup {
    fn new() -> Self {
        Self { tops: Vec::new(), roots: HashMap::new(), entries: 0, bytes: 0, last_activity: Instant::now() }
    }

    /// Maps validated components to a local folder under `base`: the folder itself for `dir`, else the file's
    /// parent.
    ///
    /// SECURITY: every path is built only from components that passed `safe_rel_path`, below a top folder
    /// created fresh for this transfer, so nothing can land outside `base` or merge into an existing folder.
    fn folder_for(&mut self, base: &Path, comps: &[&str], dir: bool) -> Option<PathBuf> {
        let parent_len = if dir { comps.len() } else { comps.len() - 1 };
        if parent_len == 0 {
            return Some(base.to_path_buf()); // loose top-level file: saved like a normal transfer
        }
        let key = comps[0].to_lowercase();
        let root = match self.roots.get(&key) {
            Some(r) => r.clone(),
            None => {
                let r = create_dir_unique(base, comps[0])?;
                self.tops.push(r.clone());
                self.roots.insert(key, r.clone());
                r
            }
        };
        let path = comps[1..parent_len].iter().fold(root, |p, c| p.join(c));
        fs::create_dir_all(&path).ok()?;
        Some(path)
    }
}

/// Puts received paths on the clipboard (remembered as ours so the watcher never sends them back) and
/// optionally pastes them into the focused Explorer window.
fn publish(ctx: &AppContext, paths: &[PathBuf], paste: bool) {
    LAST_RECEIVED_MS.store(now_ms(), Ordering::SeqCst);
    let list: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
    remember_received(list.clone());
    let stored = core_engine::clipboard::set_files(&list);
    ctx.mark_clipboard_synced();
    if cfg!(windows) && !stored {
        // Pasting now would paste whatever was copied before (an older transfer): leave the files in Downloads.
        logger::warn("file_transfer", "Clipboard busy or refused; received files were not put on it.");
        ctx.ui.status(format!("Saved in {} (could not paste: clipboard busy)", download_dir().display()));
        return;
    }
    if paste {
        // Explorer learns about the new clipboard from a notification; pasting before it arrives pastes the old
        // content.
        thread::sleep(PASTE_SETTLE);
        logger::info("file_transfer", &format!("Pasting into foreground window: {}", window::foreground_class()));
        ctx.injector.send_key_combo(true, false, false, VK_V);
        crate::service::host::input::warn_if_refused();
        thread::sleep(Duration::from_millis(25));
        // Explorer touches the clipboard while pasting; mark that as synced too so it is never echoed back.
        ctx.mark_clipboard_synced();
        LAST_RECEIVED_MS.store(now_ms(), Ordering::SeqCst);
    }
}

/// Folder that receives files (`Downloads\OpenDesk` in the user's home, else `<app dir>/Downloads`).
pub fn download_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = tests::TEST_DOWNLOADS.lock().unwrap().clone() {
        return dir;
    }
    if let Ok(profile) = env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let dir = PathBuf::from(profile).join("Downloads").join("OpenDesk");
        if fs::create_dir_all(&dir).is_ok() {
            return dir;
        }
    }
    let fallback = storage::app_dir().join("Downloads");
    let _ = fs::create_dir_all(&fallback);
    fallback
}

/// Reports one incoming file (`key` = `sender:transfer_id`) to the transfer list.
fn progress(ui: &dyn UiPort, key: &str, name: &str, done: u64, total: u64, state: TransferState) {
    let id = format!("in:{key}");
    ui.transfer(TransferUpdate { id, name: name.to_string(), outgoing: false, done, total, state });
}

fn done_status(label: &str) -> String {
    if cfg!(windows) {
        format!("Done: {label} (ready to paste, saved in Downloads\\OpenDesk)")
    } else {
        format!("Done: {label} (saved in ~/Downloads/OpenDesk)")
    }
}

/// Incoming transfers of one relay connection.
pub struct FileReceiver {
    /// Keyed by `sender:transfer_id` so two senders cannot collide or hijack each other's transfer.
    active_files: HashMap<String, Incoming>,
    completed_ids: HashMap<String, Instant>,
    /// Keyed by `sender:group_id`.
    groups: HashMap<String, FolderGroup>,
}

impl FileReceiver {
    /// A receiver with no transfers in progress.
    pub fn new() -> Self {
        Self { active_files: HashMap::new(), completed_ids: HashMap::new(), groups: HashMap::new() }
    }

    /// Drops an incoming file and its staging bytes; the transfer list shows `reason`.
    fn discard(&mut self, ui: &dyn UiPort, key: &str, reason: &str) -> Option<String> {
        let entry = self.active_files.remove(key)?;
        drop(entry.file);
        let _ = fs::remove_file(&entry.path);
        progress(ui, key, &entry.name, entry.received, entry.total, TransferState::Failed(reason.into()));
        Some(entry.name)
    }

    /// Drops transfers that stopped receiving chunks and forgets old completed IDs.
    /// Returns a status line for the last abandoned transfer.
    pub fn expire_stale(&mut self, ui: &dyn UiPort, now: Instant) -> Option<String> {
        self.completed_ids.retain(|_, done| now.duration_since(*done) < COMPLETED_TTL);
        let stale: Vec<String> = self
            .active_files
            .iter()
            .filter(|(_, f)| now.duration_since(f.last_activity) >= RECEIVE_IDLE)
            .map(|(k, _)| k.clone())
            .collect();
        let mut status = None;
        for key in stale {
            if let Some(name) = self.discard(ui, &key, "The sender stopped") {
                logger::warn("file_transfer", &format!("Incoming transfer timed out: {name}"));
                status = Some(format!("Could not receive {name}: the sender stopped"));
            }
        }
        // A folder whose sender went quiet keeps what already arrived, just without the clipboard step.
        let active = &self.active_files;
        self.groups.retain(|gkey, g| {
            now.duration_since(g.last_activity) < RECEIVE_IDLE
                || active.values().any(|f| f.dest.as_ref().is_some_and(|(_, _, k)| k == gkey))
        });
        status
    }

    /// Deletes every partial file; called when the relay connection drops because grants die with it.
    pub fn abort_all(&mut self, ui: &dyn UiPort) -> Option<String> {
        let keys: Vec<String> = self.active_files.keys().cloned().collect();
        let mut status = None;
        for key in keys {
            if let Some(name) = self.discard(ui, &key, "Relay connection lost") {
                status = Some(format!("Could not receive {name}: relay connection lost"));
            }
        }
        self.groups.clear();
        status
    }

    /// Handles one payload from `from`. `allow_auto_paste` is false outside a screen session.
    pub fn handle(&mut self, ctx: &AppContext, payload: &FilePayload, from: &str, allow_auto_paste: bool) -> Outcome {
        let id = payload.id.as_str();
        if id.is_empty() {
            return (None, None);
        }
        // Bytes for a paste stream we requested go straight to the pasting application, never to Downloads.
        if let Some(result) = virtual_clipboard::on_transfer(from, payload.action, id, payload) {
            return result;
        }
        let key = format!("{from}:{id}");
        if self.completed_ids.contains_key(&key) {
            return (None, None);
        }
        match payload.action {
            FileAction::Start => self.start(ctx, payload, from, key, allow_auto_paste),
            FileAction::Chunk => self.chunk(ctx, payload, &key),
            FileAction::End => self.end(ctx, payload, key, allow_auto_paste),
            _ => (None, None),
        }
    }

    fn start(
        &mut self,
        ctx: &AppContext,
        payload: &FilePayload,
        from: &str,
        key: String,
        allow_auto_paste: bool,
    ) -> Outcome {
        if self.active_files.contains_key(&key) {
            return (None, None);
        }
        if self.active_files.len() >= MAX_ACTIVE_FILES {
            return (Some("Could not receive files: too many active transfers".into()), Some(false));
        }
        let raw_name = payload.name.as_deref().unwrap_or("opendesk_file.bin");
        let Some(total) = payload.size else { return (None, None) };
        if !is_safe_id(&payload.id)
            || total > MAX_FILE_BYTES
            || raw_name.is_empty()
            || raw_name == "."
            || raw_name == ".."
            || raw_name.len() > MAX_NAME_LEN
            || raw_name.chars().any(|c| c.is_control())
        {
            return (None, Some(false));
        }
        // Folder transfers (only sent to `file-folder` receivers) carry the relative path in `rel`.
        let dest = if payload.group.is_some() {
            match self.start_in_group(ctx, payload, from, &key, total, allow_auto_paste) {
                Ok(dest) => Some(dest),
                Err(answer) => return answer,
            }
        } else {
            None
        };
        let name = match payload.rel.as_deref() {
            Some(rel) if dest.is_some() => rel.replace('/', "\\"),
            _ => sanitize_file_name(raw_name),
        };
        // Keep incomplete bytes away from the destination, and never truncate an existing file.
        let staging = download_dir().join(format!(".opendesk-{}-{}.part", payload.id, now_ms()));
        match OpenOptions::new().write(true).create_new(true).open(&staging) {
            Ok(file) => {
                logger::info("file_transfer", &format!("Receiving id={} name={name} size={total}", payload.id));
                let status = format!("Receiving {name}...");
                progress(ctx.ui.as_ref(), &key, &name, 0, total, TransferState::Active);
                self.active_files.insert(
                    key,
                    Incoming {
                        file,
                        path: staging,
                        name,
                        received: 0,
                        total,
                        hash: Sha256::new(),
                        last_activity: Instant::now(),
                        shown_percent: 0,
                        dest,
                    },
                );
                (Some(status), None)
            }
            Err(_) => (Some(format!("Could not receive {name}: the file could not be created")), Some(false)),
        }
    }

    /// Folder part of a `start` carrying `group`. `Err` answers right away (folder entries, rejections);
    /// `Ok(dest)` is a file whose bytes follow.
    fn start_in_group(
        &mut self,
        ctx: &AppContext,
        payload: &FilePayload,
        from: &str,
        key: &str,
        size: u64,
        allow_auto_paste: bool,
    ) -> Result<(PathBuf, String, String), Outcome> {
        let gid = payload.group.as_deref().unwrap_or("");
        let rel = payload.rel.as_deref().unwrap_or("");
        let dir = payload.dir == Some(true);
        let Some(comps) = safe_rel_path(rel).filter(|_| is_safe_id(gid)) else {
            logger::warn("file_transfer", &format!("Rejected folder entry with unsafe path: {rel:?}"));
            return Err((Some("Folder rejected: invalid name".into()), Some(false)));
        };
        let gkey = format!("{from}:{gid}");
        if !self.groups.contains_key(&gkey) && self.groups.len() >= MAX_ACTIVE_GROUPS {
            return Err((Some("Folder rejected: too many active transfers".into()), Some(false)));
        }
        let base = download_dir();
        let group = self.groups.entry(gkey.clone()).or_insert_with(FolderGroup::new);
        group.entries += 1;
        group.bytes = group.bytes.saturating_add(size);
        group.last_activity = Instant::now();
        // +1: the sender repeats the first folder entry as the end marker.
        if group.entries > MAX_TREE_ENTRIES + 1 || group.bytes > MAX_TREE_BYTES || (dir && size != 0) {
            self.groups.remove(&gkey);
            return Err((Some("Folder rejected: over the item or size limit".into()), Some(false)));
        }
        let Some(folder) = group.folder_for(&base, &comps, dir) else {
            return Err((Some(format!("Could not receive {rel}: the folder could not be created")), Some(false)));
        };
        if !dir {
            return Ok((folder, comps[comps.len() - 1].to_string(), gkey));
        }
        self.completed_ids.insert(key.to_string(), Instant::now());
        if payload.group_end != Some(true) {
            return Err((None, Some(true)));
        }
        // Whole folder arrived: hand the top-level items to the clipboard once.
        let group = self.groups.remove(&gkey).unwrap();
        let paste = payload.auto_paste.unwrap_or(true) && allow_auto_paste;
        publish(ctx, &group.tops, paste);
        let label = match group.tops.as_slice() {
            [one] => one.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            many => format!("{} items", many.len()),
        };
        logger::info("file_transfer", &format!("Folder transfer complete: {:?} (auto_paste={paste})", group.tops));
        Err((Some(done_status(&label)), Some(true)))
    }

    fn chunk(&mut self, ctx: &AppContext, payload: &FilePayload, key: &str) -> Outcome {
        let Some(entry) = self.active_files.get_mut(key) else { return (None, None) };
        let bytes = payload.chunk.as_deref().filter(|b64| b64.len() <= MAX_CHUNK_B64).and_then(from_base64);
        let ok = match bytes {
            Some(bytes)
                if bytes.len() <= CHUNK_SIZE
                    && entry.received.saturating_add(bytes.len() as u64) <= entry.total
                    && entry.file.write_all(&bytes).is_ok() =>
            {
                entry.hash.update(&bytes);
                entry.received += bytes.len() as u64;
                entry.last_activity = Instant::now();
                true
            }
            _ => false,
        };
        if !ok {
            let name = self.discard(ctx.ui.as_ref(), key, "Invalid data or disk full").unwrap_or_default();
            return (Some(format!("Could not receive {name}")), Some(false));
        }
        let shown = percent(entry.received, entry.total);
        if shown != entry.shown_percent {
            entry.shown_percent = shown;
            progress(ctx.ui.as_ref(), key, &entry.name, entry.received, entry.total, TransferState::Active);
        }
        let status = (entry.total > 0)
            .then(|| format!("Receiving {} ({}%)...", entry.name, percent(entry.received, entry.total)));
        (status, None)
    }

    fn end(&mut self, ctx: &AppContext, payload: &FilePayload, key: String, allow_auto_paste: bool) -> Outcome {
        let Some(Incoming { mut file, path: staging, name, received, total, hash, dest, .. }) =
            self.active_files.remove(&key)
        else {
            // Unknown transfer (expired or rejected start): NACK so the sender does not wait for a timeout.
            return (None, Some(false));
        };
        let checksum_ok = payload.sha256.as_deref() == Some(hex(&hash.finalize()).as_str());
        let written = received == total && checksum_ok && file.flush().is_ok() && file.sync_all().is_ok();
        drop(file);
        let fail = |reason: &str| {
            progress(ctx.ui.as_ref(), &key, &name, received, total, TransferState::Failed(reason.into()))
        };
        if !written {
            let _ = fs::remove_file(staging);
            fail("Size or SHA-256 mismatch");
            return (Some(format!("Could not receive {name}: size or SHA-256 mismatch")), Some(false));
        }
        let base = download_dir();
        let (target_dir, target_name) = match &dest {
            Some((dir, file_name, _)) => (dir.clone(), file_name.as_str()),
            None => (base.clone(), name.as_str()),
        };
        let saved = link_unique(&staging, &target_dir, target_name);
        let _ = fs::remove_file(&staging);
        let Some(saved) = saved else {
            fail("Saving failed");
            return (Some(format!("Could not receive {name}: saving failed")), Some(false));
        };
        progress(ctx.ui.as_ref(), &key, &name, total, total, TransferState::Done);
        self.completed_ids.insert(key, Instant::now());

        if let Some((dir, _, gkey)) = dest {
            // Part of a folder: the clipboard is set once the group's end marker arrives.
            LAST_RECEIVED_MS.store(now_ms(), Ordering::SeqCst);
            if let Some(group) = self.groups.get_mut(&gkey) {
                group.last_activity = Instant::now();
                if dir == base {
                    group.tops.push(saved); // loose top-level file sent along with folders
                }
            }
            return (Some(format!("Received: {name}")), Some(true));
        }
        let paste = payload.auto_paste.unwrap_or(true) && allow_auto_paste;
        logger::info("file_transfer", &format!("File transfer complete: {} (auto_paste={paste})", saved.display()));
        publish(ctx, &[saved], paste);
        (Some(done_status(&name)), Some(true))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Receive folder for tests instead of the user's Downloads.
    pub static TEST_DOWNLOADS: Mutex<Option<PathBuf>> = Mutex::new(None);

    #[test]
    fn folder_group_creates_unique_top_folder_and_stays_inside() {
        let base = env::temp_dir().join(format!("opendesk-group-{}-{}", std::process::id(), now_ms()));
        fs::create_dir_all(base.join("Project")).unwrap();
        fs::write(base.join("Project/old.txt"), b"old").unwrap();
        let mut g = FolderGroup::new();
        // Existing "Project" is never merged into: the group gets "Project (1)".
        assert_eq!(g.folder_for(&base, &["Project"], true), Some(base.join("Project (1)")));
        assert_eq!(g.folder_for(&base, &["project", "src", "main.rs"], false), Some(base.join("Project (1)/src")));
        assert!(base.join("Project (1)/src").is_dir());
        assert_eq!(g.folder_for(&base, &["loose.txt"], false), Some(base.clone()));
        assert_eq!(g.tops, vec![base.join("Project (1)")]);
        assert_eq!(fs::read(base.join("Project/old.txt")).unwrap(), b"old");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn receiver_rejects_unsafe_folder_entries_before_touching_disk() {
        let ctx = crate::service::test_support::context();
        let mut rx = FileReceiver::new();
        let entry = |rel: &str, group: &str| FilePayload {
            size: Some(0),
            dir: Some(true),
            group: Some(group.into()),
            rel: Some(rel.into()),
            ..FilePayload::start("fd_1", "x", 0)
        };
        for rel in [r"..\evil.txt", r"C:\Windows\evil.txt", r"\\srv\s\x", r"Project\CON", ""] {
            assert_eq!(rx.handle(&ctx, &entry(rel, "fg_1"), "111", false).1, Some(false), "{rel:?}");
        }
        assert_eq!(rx.handle(&ctx, &entry("ok", "../g"), "111", false).1, Some(false));
        assert!(rx.groups.is_empty() && rx.active_files.is_empty());
    }
}
