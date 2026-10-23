use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use core_engine::codec::{from_base64, to_base64};
use core_engine::input::InputInjector;
use sha2::{Digest, Sha256};
use serde_json::{json, Value};
use slint::Weak;

use crate::logger;
use crate::protocol::OutgoingMsg;
use crate::MainWindow;

const CHUNK_SIZE: usize = 32768; // 32 KB per chunk for smooth non-blocking WebSocket flow
/// Incoming transfers with no chunk for this long are abandoned and their staging file deleted.
const RECEIVE_IDLE: Duration = Duration::from_secs(60);
/// Completed transfer IDs are remembered this long to reject replays.
const COMPLETED_TTL: Duration = Duration::from_secs(600);
/// Sender waits this long for the receiver's final ACK after queueing the last message.
const ACK_TIMEOUT: Duration = Duration::from_secs(120);

/// Tracks in-flight file transfer chunks in `tx_out` so large files apply backpressure
/// and UI progress reflects actual network transmission rather than in-memory queueing.
pub static IN_FLIGHT_FILE_CHUNKS: AtomicUsize = AtomicUsize::new(0);

/// Timestamp (epoch ms) of the last completed incoming file paste so Clipboard Watcher never echoes it back.
pub static LAST_FILE_RECV_MS: AtomicU64 = AtomicU64::new(0);

/// Paths this app itself put on the clipboard after receiving them, so copy-paste never sends them back.
static RECEIVED_CLIPBOARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// True if the clipboard holds exactly the files we just received (provenance, not timing).
pub fn is_received_clipboard(paths: &[PathBuf]) -> bool {
    let ours = RECEIVED_CLIPBOARD.lock().unwrap();
    !paths.is_empty()
        && paths.len() == ours.len()
        && paths.iter().all(|p| ours.iter().any(|o| o.eq_ignore_ascii_case(&p.to_string_lossy())))
}

/// Bumped when the relay connection drops; senders abort transfers started on an older connection.
pub static TRANSFER_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Whether the current session partner advertised `file-ack` (old clients never send ACKs).
pub static PEER_FILE_ACK: AtomicBool = AtomicBool::new(false);

/// Whether the current partner advertised `file-offer`: copied files are offered and pulled on paste (AnyDesk style).
pub static PEER_FILE_OFFER: AtomicBool = AtomicBool::new(false);

/// Capability for folder transfer (relative paths in offers and `group`/`rel` transfers).
/// Old receivers would flatten or mangle relative names, so folders are only sent to partners advertising it.
pub const CAP_FILE_FOLDER: &str = "file-folder";

/// Whether the current partner advertised `file-folder`.
pub static PEER_FILE_FOLDER: AtomicBool = AtomicBool::new(false);

/// Files we offered, as (offer id, partner, paths); `None` marks a folder entry, which is never pulled.
/// A pull may only ask for these. Newest last, a few kept.
static OUR_OFFERS: Mutex<Vec<(String, String, Vec<Option<PathBuf>>)>> = Mutex::new(Vec::new());
const MAX_OFFER_FILES: usize = 100;
/// Bumped per clipboard copy so a slow folder walk never publishes over a newer copy.
static OFFER_GEN: AtomicU64 = AtomicU64::new(0);

pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Limits for one folder transfer (both directions): entries (files + folders) and total bytes.
pub const MAX_TREE_ENTRIES: usize = 10_000;
pub const MAX_TREE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// FILEDESCRIPTORW.cFileName holds 260 UTF-16 units including the terminator.
const MAX_REL_UTF16: usize = 259;
const SKIPPED_FOLDERS: &str = "Folder dilewati: perangkat lain perlu OpenDesk versi terbaru untuk menerima folder";

/// Validates a relative path that came from the partner and returns its components.
/// Rejects absolute/UNC paths, drive letters and streams (`:`), `.`/`..`, empty components, reserved
/// device names, characters Windows cannot store, trailing dots/spaces (Windows would silently alias them)
/// and over-long paths. Both `\` and `/` separate components.
pub fn safe_rel_path(name: &str) -> Option<Vec<&str>> {
    if name.is_empty() || name.encode_utf16().count() > MAX_REL_UTF16 {
        return None;
    }
    let comps: Vec<&str> = name.split(['\\', '/']).collect();
    let ok = comps.iter().all(|c| {
        !c.is_empty()
            && *c != "."
            && *c != ".."
            && c.encode_utf16().count() <= 255
            && !c.ends_with(['.', ' '])
            && !c.chars().any(|ch| ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
            && !is_reserved_name(c)
    });
    ok.then_some(comps)
}

fn is_reserved_name(comp: &str) -> bool {
    // "nul.txt" and "CON .log" still open the device on Windows, so check the part before the first dot.
    let stem = comp.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.chars().count() == 4
            && stem.chars().last().is_some_and(|d| matches!(d, '1'..='9' | '¹' | '²' | '³')))
}

/// One entry of a walked selection. `rel` uses `\` separators; folders have `dir` set and size 0.
#[derive(Debug)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub rel: String,
    pub size: u64,
    pub dir: bool,
}

/// Expands the selected paths into files and folders (folders before their contents) without reading
/// any file contents. Symlinks and junctions inside folders are skipped (no loops, nothing outside the
/// selection). Errors carry an Indonesian status line.
pub fn walk_tree(roots: &[PathBuf], max_entries: usize, max_bytes: u64) -> Result<Vec<TreeEntry>, String> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut push = |out: &mut Vec<TreeEntry>, e: TreeEntry| -> Result<(), String> {
        if safe_rel_path(&e.rel).is_none() {
            return Err(format!("Gagal: nama atau path tidak didukung: {}", e.rel));
        }
        if e.size > MAX_FILE_BYTES {
            return Err(format!("Gagal: {} melebihi 2GB", e.rel));
        }
        total += e.size;
        if out.len() >= max_entries {
            return Err(format!("Gagal: folder terlalu banyak isi (maks {} item)", max_entries));
        }
        if total > max_bytes {
            return Err(format!("Gagal: total ukuran melebihi {} GB", max_bytes >> 30));
        }
        out.push(e);
        Ok(())
    };
    for root in roots {
        let (Ok(meta), Some(name)) = (fs::metadata(root), root.file_name().and_then(|n| n.to_str())) else {
            continue;
        };
        if meta.is_file() {
            push(&mut out, TreeEntry { path: root.clone(), rel: name.to_string(), size: meta.len(), dir: false })?;
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        push(&mut out, TreeEntry { path: root.clone(), rel: name.to_string(), size: 0, dir: true })?;
        let mut stack = vec![(root.clone(), name.to_string())];
        while let Some((dir, rel)) = stack.pop() {
            let rd = match fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) => {
                    // Unreadable folder (access denied): keep it as an empty folder rather than failing everything.
                    logger::warn("app.tunnel_ft_comp", &format!("Skipping unreadable folder {:?}: {}", dir, e));
                    continue;
                }
            };
            let mut children: Vec<_> = rd.filter_map(Result::ok).collect();
            children.sort_by_key(|c| c.file_name());
            for child in children {
                let Ok(ft) = child.file_type() else { continue };
                if ft.is_symlink() {
                    continue;
                }
                let Some(cname) = child.file_name().to_str().map(str::to_string) else {
                    return Err(format!("Gagal: nama tidak didukung di {}", rel));
                };
                let crel = format!("{}\\{}", rel, cname);
                if ft.is_dir() {
                    push(&mut out, TreeEntry { path: child.path(), rel: crel.clone(), size: 0, dir: true })?;
                    stack.push((child.path(), crel));
                } else if ft.is_file() {
                    let size = child.metadata().map(|m| m.len()).unwrap_or(0);
                    push(&mut out, TreeEntry { path: child.path(), rel: crel, size, dir: false })?;
                }
            }
        }
    }
    Ok(out)
}

/// Senders waiting for a receiver ACK, keyed by transfer ID.
static PENDING_ACKS: Mutex<Vec<(String, Sender<bool>)>> = Mutex::new(Vec::new());

/// Delivers a receiver ACK to the waiting sender thread, if any.
pub fn deliver_ack(id: &str, ok: bool) {
    let mut pending = PENDING_ACKS.lock().unwrap();
    if let Some(pos) = pending.iter().position(|(pid, _)| pid == id) {
        let (_, tx) = pending.swap_remove(pos);
        let _ = tx.send(ok);
    }
}

/// Fails every in-progress send after the relay connection is lost (grants die with the connection).
pub fn abort_pending_sends() {
    TRANSFER_EPOCH.fetch_add(1, Ordering::SeqCst);
    PENDING_ACKS.lock().unwrap().clear();
}

fn set_status(win: &Option<Weak<MainWindow>>, text: String) {
    if let Some(w) = win {
        let _ = w.upgrade_in_event_loop(move |win| win.set_status_text(text.into()));
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn matching_checksum(hash: Sha256, expected: Option<&str>) -> bool {
    let actual: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    expected == Some(actual.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_accepts_empty_file_and_rejects_corruption_or_missing_hash() {
        let empty = Sha256::new();
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(matching_checksum(empty.clone(), Some(digest)));
        assert!(!matching_checksum(empty.clone(), None));
        assert!(!matching_checksum(empty, Some(&"0".repeat(64))));
    }

    #[test]
    fn link_unique_never_overwrites_existing_file() {
        let dir = env::temp_dir().join(format!("opendesk-test-{}", now_epoch_ms()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), b"old").unwrap();
        let staging = dir.join(".part");
        fs::write(&staging, b"new").unwrap();
        let saved = link_unique(&staging, &dir, "a.txt").unwrap();
        assert_eq!(saved, dir.join("a (1).txt"));
        assert_eq!(fs::read(dir.join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(saved).unwrap(), b"new");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn safe_rel_path_accepts_relative_trees_only() {
        assert_eq!(safe_rel_path(r"Proyek\src\main.rs"), Some(vec!["Proyek", "src", "main.rs"]));
        assert_eq!(safe_rel_path("a/b.txt"), Some(vec!["a", "b.txt"]));
        for ok in ["COM10", "console.txt", "a.b.c", "Läporan 2024", "nulled.txt"] {
            assert!(safe_rel_path(ok).is_some(), "{ok}");
        }
        let long_comp = "x".repeat(256);
        let long_path = format!("{}\\{}", "a".repeat(200), "b".repeat(59)); // 260 units
        for bad in [
            "", r"\abs", "/abs", r"\\server\share", r"C:\x", "C:x", "a:stream", r"a\..\b", "..", ".", r"a\\b",
            r"a\", r"a\.\b", "CON", "nul.txt", r"x\com1", "LPT9.log", "AUX .txt", "COM¹", "a\u{1}b", "a<b", "a|b",
            "a?", "a*", "a\"b", "trail.", "trail ", &long_comp, &long_path,
        ] {
            assert!(safe_rel_path(bad).is_none(), "{bad:?}");
        }
    }

    fn temp_tree(tag: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("opendesk-walk-{}-{}-{}", tag, std::process::id(), now_epoch_ms()));
        fs::create_dir_all(root.join(r"Proyek\src\deep")).unwrap();
        fs::create_dir_all(root.join(r"Proyek\kosong")).unwrap();
        fs::write(root.join(r"Proyek\src\main.rs"), b"fn main() {}").unwrap();
        fs::write(root.join(r"Proyek\src\deep\x.txt"), b"x").unwrap();
        fs::write(root.join("loose.txt"), b"12345").unwrap();
        root
    }

    #[test]
    fn walk_tree_lists_folders_before_contents_with_relative_names() {
        let root = temp_tree("order");
        let entries = walk_tree(&[root.join("Proyek"), root.join("loose.txt")], 100, 1 << 20).unwrap();
        let rels: Vec<(&str, bool, u64)> = entries.iter().map(|e| (e.rel.as_str(), e.dir, e.size)).collect();
        assert_eq!(rels.len(), 7);
        for (rel, dir, size) in [
            ("Proyek", true, 0),
            (r"Proyek\kosong", true, 0),
            (r"Proyek\src", true, 0),
            (r"Proyek\src\deep", true, 0),
            (r"Proyek\src\main.rs", false, 12),
            (r"Proyek\src\deep\x.txt", false, 1),
            ("loose.txt", false, 5),
        ] {
            assert!(rels.contains(&(rel, dir, size)), "{rel}");
        }
        // Every entry comes after its parent folder, so the receiver can create folders first.
        for (i, e) in entries.iter().enumerate() {
            if let Some((parent, _)) = e.rel.rsplit_once('\\') {
                assert!(entries[..i].iter().any(|p| p.dir && p.rel == parent), "{} before its folder", e.rel);
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn walk_tree_enforces_entry_and_size_limits() {
        let root = temp_tree("limits");
        let sel = [root.join("Proyek")];
        assert!(walk_tree(&sel, 6, 1 << 20).is_ok());
        assert!(walk_tree(&sel, 5, 1 << 20).unwrap_err().contains("maks 5"));
        assert!(walk_tree(&sel, 100, 12).unwrap_err().contains("total ukuran"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn folder_group_creates_unique_top_folder_and_stays_inside() {
        let base = env::temp_dir().join(format!("opendesk-group-{}-{}", std::process::id(), now_epoch_ms()));
        fs::create_dir_all(base.join("Proyek")).unwrap();
        fs::write(base.join(r"Proyek\old.txt"), b"old").unwrap();
        let mut g = FolderGroup { tops: Vec::new(), roots: HashMap::new(), entries: 0, bytes: 0, last_activity: Instant::now() };
        // Existing "Proyek" is never merged into: the group gets "Proyek (1)".
        assert_eq!(g.folder_for(&base, &["Proyek"], true), Some(base.join("Proyek (1)")));
        assert_eq!(g.folder_for(&base, &["proyek", "src", "main.rs"], false), Some(base.join(r"Proyek (1)\src")));
        assert!(base.join(r"Proyek (1)\src").is_dir());
        assert_eq!(g.folder_for(&base, &["loose.txt"], false), Some(base.clone()));
        assert_eq!(g.tops, vec![base.join("Proyek (1)")]);
        assert_eq!(fs::read(base.join(r"Proyek\old.txt")).unwrap(), b"old");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn receiver_rejects_unsafe_folder_entries_before_touching_disk() {
        let mut rx = FileReceiver::new();
        let injector = InputInjector::new();
        let seq = Arc::new(AtomicU32::new(0));
        for rel in [r"..\evil.txt", r"C:\Windows\evil.txt", r"\\srv\s\x", r"Proyek\CON", ""] {
            let payload = json!({ "action": "start", "id": "fd_1", "name": "x", "size": 0, "dir": true, "group": "fg_1", "rel": rel });
            let (_, ack) = rx.handle_payload(&payload, "111", &injector, &seq, false);
            assert_eq!(ack, Some(false), "{rel:?}");
        }
        let bad_group = json!({ "action": "start", "id": "fd_2", "name": "x", "size": 0, "dir": true, "group": "../g", "rel": "ok" });
        assert_eq!(rx.handle_payload(&bad_group, "111", &injector, &seq, false).1, Some(false));
        assert!(rx.groups.is_empty() && rx.active_files.is_empty());
    }

    /// Receive folder for the loopback test instead of the user's Downloads.
    pub(super) static TEST_STAGING: Mutex<Option<PathBuf>> = Mutex::new(None);

    /// Drag-drop / send path end to end: sender messages fed straight into a receiver. Sets the clipboard,
    /// so run it by hand: `cargo test --release folder_send -- --ignored`
    #[test]
    #[ignore]
    fn folder_send_round_trip_recreates_tree() {
        let src = temp_tree("send");
        let dl = env::temp_dir().join(format!("opendesk-recv-{}-{}", std::process::id(), now_epoch_ms()));
        fs::create_dir_all(dl.join("Proyek")).unwrap(); // existing folder: the copy must become "Proyek (1)"
        *TEST_STAGING.lock().unwrap() = Some(dl.clone());
        PEER_FILE_FOLDER.store(true, Ordering::SeqCst);
        PEER_FILE_ACK.store(false, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        send_files_async(vec![src.join("Proyek"), src.join("loose.txt")], "111".into(), tx, None, false);

        let mut receiver = FileReceiver::new();
        let injector = InputInjector::new();
        let seq = Arc::new(AtomicU32::new(0));
        let mut finished = false;
        while let Ok(OutgoingMsg::FileTransfer { msg, .. }) = rx.recv_timeout(Duration::from_secs(10)) {
            IN_FLIGHT_FILE_CHUNKS.fetch_sub(1, Ordering::SeqCst);
            let payload = &msg["payload"];
            assert!(payload["name"].as_str().is_none_or(|n| !n.contains('\\')), "wire name must stay plain");
            let (status, ack) = receiver.handle_payload(payload, "111", &injector, &seq, false);
            assert_ne!(ack, Some(false), "rejected: {status:?}");
            if payload["groupEnd"] == true {
                finished = true;
                break;
            }
        }
        assert!(finished, "group end marker never arrived");
        let copy = dl.join("Proyek (1)");
        assert_eq!(fs::read(copy.join(r"src\main.rs")).unwrap(), b"fn main() {}");
        assert_eq!(fs::read(copy.join(r"src\deep\x.txt")).unwrap(), b"x");
        assert!(copy.join("kosong").is_dir());
        assert_eq!(fs::read(dl.join("loose.txt")).unwrap(), b"12345");
        assert_eq!(fs::read_dir(dl.join("Proyek")).unwrap().count(), 0, "existing folder untouched");
        let ours = RECEIVED_CLIPBOARD.lock().unwrap().clone();
        assert_eq!(ours, vec![copy.to_string_lossy().to_string(), dl.join("loose.txt").to_string_lossy().to_string()]);
        *TEST_STAGING.lock().unwrap() = None;
        let _ = fs::remove_dir_all(src);
        let _ = fs::remove_dir_all(dl);
    }
}

struct Incoming {
    file: File,
    path: PathBuf,
    name: String,
    received: u64,
    total: u64,
    hash: Sha256,
    last_activity: Instant,
    /// Set for files inside a folder transfer: (target folder, file name, group key). The clipboard is
    /// only updated once the whole group has arrived.
    dest: Option<(PathBuf, String, String)>,
}

/// A folder transfer in progress (`group` field on start messages).
struct FolderGroup {
    /// Top-level items as saved (folders renamed "name (n)" on conflict), in arrival order, for CF_HDROP.
    tops: Vec<PathBuf>,
    /// Sender's top-level folder name (lowercase) -> folder we created for it.
    roots: HashMap<String, PathBuf>,
    entries: usize,
    bytes: u64,
    last_activity: Instant,
}

/// Creates `base\name`, or `name (1)`, ... if taken; create_dir never reuses an existing folder.
fn create_dir_unique(base: &Path, name: &str) -> Option<PathBuf> {
    for n in 0..100 {
        let candidate = if n == 0 { base.join(name) } else { base.join(format!("{name} ({n})")) };
        match fs::create_dir(&candidate) {
            Ok(()) => return Some(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

impl FolderGroup {
    /// Maps validated components to a local folder under `base`: the folder itself for `dir`, else the file's
    /// parent. Every path is built only from components that passed `safe_rel_path`, below a top folder we
    /// created fresh, so nothing can land outside `base`.
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

pub struct FileReceiver {
    /// Keyed by `sender:transfer_id` so two senders cannot collide or hijack each other's transfer.
    active_files: HashMap<String, Incoming>,
    completed_ids: HashMap<String, Instant>,
    /// Keyed by `sender:group_id`.
    groups: HashMap<String, FolderGroup>,
}

/// Puts received paths on CF_HDROP (remembered as ours so the watcher never sends them back) and optionally
/// pastes them into the focused Explorer window.
fn publish_received(paths: &[PathBuf], injector: &InputInjector, last_clip_seq: &Arc<AtomicU32>, paste: bool) {
    LAST_FILE_RECV_MS.store(now_epoch_ms(), Ordering::SeqCst);
    let list: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
    *RECEIVED_CLIPBOARD.lock().unwrap() = list.clone();
    injector.set_clipboard_files(&list);
    last_clip_seq.store(injector.get_clipboard_sequence_number(), Ordering::SeqCst);
    if paste {
        thread::sleep(Duration::from_millis(35));
        injector.send_key_combo(true, false, false, 0x56); // Ctrl+V into open Explorer folder
        thread::sleep(Duration::from_millis(25));
        // Refresh clipboard sequence again after Explorer processes Ctrl+V so watcher never echoes back!
        last_clip_seq.store(injector.get_clipboard_sequence_number(), Ordering::SeqCst);
        LAST_FILE_RECV_MS.store(now_epoch_ms(), Ordering::SeqCst);
    }
}

impl FileReceiver {
    pub fn new() -> Self {
        Self {
            active_files: HashMap::new(),
            completed_ids: HashMap::new(),
            groups: HashMap::new(),
        }
    }

    /// Handles the folder part of a `start` carrying `group`. Returns Err((status, ack)) to answer right away
    /// (folder entries, rejections), or Ok(dest) for a file whose bytes follow.
    #[allow(clippy::too_many_arguments)]
    fn start_in_group(
        &mut self,
        payload: &Value,
        from: &str,
        key: &str,
        size: u64,
        injector: &InputInjector,
        last_clip_seq: &Arc<AtomicU32>,
        allow_auto_paste: bool,
    ) -> Result<(PathBuf, String, String), (Option<String>, Option<bool>)> {
        let gid = payload.get("group").and_then(|v| v.as_str()).unwrap_or("");
        let rel = payload.get("rel").and_then(|v| v.as_str()).unwrap_or("");
        let dir = payload.get("dir").and_then(|v| v.as_bool()) == Some(true);
        let comps = safe_rel_path(rel).filter(|_| {
            !gid.is_empty() && gid.len() <= 80 && gid.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        });
        let Some(comps) = comps else {
            logger::warn("app.tunnel_ft_comp", &format!("Rejected folder entry with unsafe path: {:?}", rel));
            return Err((Some("Gagal menerima folder: nama tidak valid".into()), Some(false)));
        };
        let gkey = format!("{}:{}", from, gid);
        if !self.groups.contains_key(&gkey) && self.groups.len() >= 4 {
            return Err((Some("Gagal menerima folder: terlalu banyak transfer aktif".into()), Some(false)));
        }
        let base = Self::get_staging_dir();
        let group = self.groups.entry(gkey.clone()).or_insert_with(|| FolderGroup {
            tops: Vec::new(),
            roots: HashMap::new(),
            entries: 0,
            bytes: 0,
            last_activity: Instant::now(),
        });
        group.entries += 1;
        group.bytes = group.bytes.saturating_add(size);
        group.last_activity = Instant::now();
        // +1: the sender repeats the first folder entry as the end marker.
        if group.entries > MAX_TREE_ENTRIES + 1 || group.bytes > MAX_TREE_BYTES || (dir && size != 0) {
            self.groups.remove(&gkey);
            return Err((Some("Gagal menerima folder: melebihi batas jumlah atau ukuran".into()), Some(false)));
        }
        let Some(folder) = group.folder_for(&base, &comps, dir) else {
            return Err((Some(format!("Gagal menerima {}: tidak bisa membuat folder", rel)), Some(false)));
        };
        if !dir {
            return Ok((folder, comps[comps.len() - 1].to_string(), gkey));
        }
        self.completed_ids.insert(key.to_string(), Instant::now());
        if payload.get("groupEnd").and_then(|v| v.as_bool()) != Some(true) {
            return Err((None, Some(true)));
        }
        // Whole folder arrived: hand the top-level items to the clipboard once.
        let group = self.groups.remove(&gkey).unwrap();
        let auto_paste = payload.get("autoPaste").and_then(|v| v.as_bool()).unwrap_or(true);
        publish_received(&group.tops, injector, last_clip_seq, auto_paste && allow_auto_paste);
        let label = match group.tops.as_slice() {
            [one] => one.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            many => format!("{} item", many.len()),
        };
        logger::info(
            "app.tunnel_ft_comp",
            &format!("Folder transfer complete: {:?} (CF_HDROP set, auto_paste={})", group.tops, auto_paste && allow_auto_paste),
        );
        Err((Some(format!("Selesai: {} (Siap Paste / Downloads\\OpenDesk)", label)), Some(true)))
    }

    pub fn get_staging_dir() -> PathBuf {
        #[cfg(test)]
        if let Some(dir) = tests::TEST_STAGING.lock().unwrap().clone() {
            return dir;
        }
        if let Ok(profile) = env::var("USERPROFILE") {
            let dl = PathBuf::from(profile).join("Downloads").join("OpenDesk");
            if fs::create_dir_all(&dl).is_ok() {
                return dl;
            }
        }
        let fallback = crate::config::get_app_dir().join("Downloads");
        let _ = fs::create_dir_all(&fallback);
        fallback
    }

    fn discard(&mut self, key: &str) -> Option<String> {
        let entry = self.active_files.remove(key)?;
        drop(entry.file);
        let _ = fs::remove_file(&entry.path);
        Some(entry.name)
    }

    /// Drops transfers that stopped receiving chunks and forgets old completed IDs.
    /// Returns a status line for the last abandoned transfer.
    pub fn expire_stale(&mut self, now: Instant) -> Option<String> {
        self.completed_ids
            .retain(|_, done| now.duration_since(*done) < COMPLETED_TTL);
        let stale: Vec<String> = self
            .active_files
            .iter()
            .filter(|(_, f)| now.duration_since(f.last_activity) >= RECEIVE_IDLE)
            .map(|(k, _)| k.clone())
            .collect();
        let mut status = None;
        for key in stale {
            if let Some(name) = self.discard(&key) {
                logger::warn("app.tunnel_ft_comp", &format!("Incoming transfer timed out: {}", name));
                status = Some(format!("Gagal menerima {}: pengirim berhenti mengirim", name));
            }
        }
        // A folder whose sender went quiet keeps what already arrived in Downloads, just without the clipboard step.
        let active = &self.active_files;
        self.groups.retain(|gkey, g| {
            now.duration_since(g.last_activity) < RECEIVE_IDLE
                || active.values().any(|f| f.dest.as_ref().is_some_and(|(_, _, k)| k == gkey))
        });
        status
    }

    /// Deletes every partial file; called when the relay connection drops because grants die with it.
    pub fn abort_all(&mut self) -> Option<String> {
        let keys: Vec<String> = self.active_files.keys().cloned().collect();
        let mut status = None;
        for key in keys {
            if let Some(name) = self.discard(&key) {
                status = Some(format!("Gagal menerima {}: koneksi relay terputus", name));
            }
        }
        self.groups.clear();
        status
    }

    /// Returns `(status_text, ack)`. `ack` is `Some(ok)` when the sender must be told the final result.
    pub fn handle_payload(
        &mut self,
        payload: &Value,
        from: &str,
        injector: &InputInjector,
        last_clip_seq: &Arc<AtomicU32>,
        allow_auto_paste: bool,
    ) -> (Option<String>, Option<bool>) {
        let (Some(action), Some(id)) = (
            payload.get("action").and_then(|v| v.as_str()),
            payload.get("id").and_then(|v| v.as_str()),
        ) else {
            return (None, None);
        };
        // Bytes for a paste stream we requested go straight to Explorer, never to Downloads.
        if let Some(result) = crate::virtual_clip::on_transfer(from, action, id, payload) {
            return result;
        }
        let key = format!("{}:{}", from, id);

        if self.completed_ids.contains_key(&key) {
            return (None, None);
        }

        match action {
            "start" => {
                if self.active_files.contains_key(&key) {
                    return (None, None);
                }
                if self.active_files.len() >= 8 {
                    return (Some("Gagal menerima berkas: terlalu banyak transfer aktif".into()), Some(false));
                }
                let raw_name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("opendesk_file.bin");
                let Some(total_size) = payload.get("size").and_then(|v| v.as_u64()) else {
                    return (None, None);
                };
                if id.len() > 80
                    || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                    || total_size > 2 * 1024 * 1024 * 1024
                    || raw_name.is_empty()
                    || raw_name == "."
                    || raw_name == ".."
                    || raw_name.len() > 200
                    || raw_name.chars().any(|c| c.is_control())
                {
                    return (None, Some(false));
                }
                let clean_name: String = raw_name
                    .chars()
                    .map(|c| match c {
                        '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                        _ => c,
                    })
                    .collect();
                // Folder transfers (only sent to `file-folder` receivers) carry the relative path in `rel`.
                let dest = if payload.get("group").is_some() {
                    match self.start_in_group(payload, from, &key, total_size, injector, last_clip_seq, allow_auto_paste) {
                        Ok(dest) => Some(dest),
                        Err(answer) => return answer,
                    }
                } else {
                    None
                };
                let clean_name = match payload.get("rel").and_then(|v| v.as_str()) {
                    Some(rel) if dest.is_some() => rel.replace('/', "\\"),
                    _ => clean_name,
                };

                let dir = Self::get_staging_dir();
                // Keep incomplete bytes away from the destination, and never truncate an existing file.
                let staging_path = dir.join(format!(".opendesk-{}-{}.part", id, now_epoch_ms()));
                return match OpenOptions::new().write(true).create_new(true).open(&staging_path) {
                    Ok(file) => {
                        logger::info(
                            "app.tunnel_ft_comp",
                            &format!(
                                "Receiving file transfer id={} name={} size={}",
                                id, clean_name, total_size
                            ),
                        );
                        self.active_files.insert(
                            key,
                            Incoming {
                                file,
                                path: staging_path,
                                name: clean_name.clone(),
                                received: 0,
                                total: total_size,
                                hash: Sha256::new(),
                                last_activity: Instant::now(),
                                dest,
                            },
                        );
                        (Some(format!("Menerima {}...", clean_name)), None)
                    }
                    Err(_) => (Some(format!("Gagal menerima {}: tidak bisa membuat file", clean_name)), Some(false)),
                };
            }
            "chunk" => {
                let Some(entry) = self.active_files.get_mut(&key) else {
                    return (None, None);
                };
                let bytes = payload
                    .get("chunk")
                    .and_then(|v| v.as_str())
                    .filter(|b64| b64.len() <= 48_000)
                    .and_then(from_base64);
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
                    let name = self.discard(&key).unwrap_or_default();
                    return (Some(format!("Gagal menerima {}", name)), Some(false));
                }
                if let Some(prog) = entry_progress(entry) {
                    return (Some(prog), None);
                }
            }
            "end" => {
                let auto_paste = payload
                    .get("autoPaste")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let Some(Incoming { mut file, path: staging_path, name: clean_name, received, total: total_size, hash, dest, .. }) =
                    self.active_files.remove(&key)
                else {
                    // Unknown transfer (expired or rejected start): NACK so the sender does not wait for a timeout.
                    return (None, Some(false));
                };
                let expected = payload.get("sha256").and_then(|v| v.as_str());
                let written = received == total_size && matching_checksum(hash, expected) && file.flush().is_ok() && file.sync_all().is_ok();
                drop(file);
                if !written {
                    let _ = fs::remove_file(staging_path);
                    return (Some(format!("Gagal menerima {}: ukuran atau SHA-256 tidak cocok", clean_name)), Some(false));
                }
                // Conflict policy: never overwrite; save as "name (n).ext" instead.
                let base = Self::get_staging_dir();
                let (target_dir, target_name) = match &dest {
                    Some((dir, name, _)) => (dir.clone(), name.as_str()),
                    None => (base.clone(), clean_name.as_str()),
                };
                let Some(target_path) = link_unique(&staging_path, &target_dir, target_name) else {
                    let _ = fs::remove_file(staging_path);
                    return (Some(format!("Gagal menerima {}: gagal menyimpan", clean_name)), Some(false));
                };
                let _ = fs::remove_file(staging_path);

                self.completed_ids.insert(key, Instant::now());
                if let Some((dir, _, gkey)) = dest {
                    // Part of a folder: the clipboard is set once the group's end marker arrives.
                    LAST_FILE_RECV_MS.store(now_epoch_ms(), Ordering::SeqCst);
                    if let Some(group) = self.groups.get_mut(&gkey) {
                        group.last_activity = Instant::now();
                        if dir == base {
                            group.tops.push(target_path); // loose top-level file sent along with folders
                        }
                    }
                    return (Some(format!("Diterima: {}", clean_name)), Some(true));
                }

                let abs_path = target_path.to_string_lossy().to_string();
                publish_received(&[target_path], injector, last_clip_seq, auto_paste && allow_auto_paste);

                logger::info(
                    "app.tunnel_ft_comp",
                    &format!(
                        "File transfer complete: {} (CF_HDROP set, auto_paste={})",
                        abs_path,
                        auto_paste && allow_auto_paste
                    ),
                );
                return (Some(format!("Selesai: {} (Siap Paste / Downloads\\OpenDesk)", clean_name)), Some(true));
            }
            _ => {}
        }
        (None, None)
    }
}

/// Hard-links `staging` to the first free `name`, `stem (1).ext`, ... in `dir`; hard_link never replaces an existing file.
fn link_unique(staging: &Path, dir: &Path, name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    for n in 0..100 {
        let candidate = if n == 0 { dir.join(name) } else { dir.join(format!("{stem} ({n}){ext}")) };
        match fs::hard_link(staging, &candidate) {
            Ok(()) => return Some(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

fn entry_progress(entry: &Incoming) -> Option<String> {
    (entry.total > 0).then(|| {
        let pct = ((entry.received * 100) / entry.total).min(99);
        format!("Menerima {} ({}%)...", entry.name, pct)
    })
}

/// Folder membership of one item of a folder transfer.
struct FolderTag {
    group: String,
    rel: String,
    dir: bool,
    /// Repeat of the first folder entry that tells the receiver the whole group was sent.
    end: bool,
}

struct SendItem {
    path: PathBuf,
    id: Option<String>,
    tag: Option<FolderTag>,
}

enum SendJob {
    /// Dropped / picked / copied paths: may contain folders.
    Paths(Vec<PathBuf>),
    /// One offered file pulled under the transfer ID chosen by the receiver, so it can route the bytes to its paste stream.
    Pull(PathBuf, String),
}

pub fn send_files_async(
    paths: Vec<PathBuf>,
    target_clean: String,
    tx_out: Sender<OutgoingMsg>,
    win_opt: Option<Weak<MainWindow>>,
    auto_paste: bool,
) {
    if !paths.is_empty() {
        send_job(SendJob::Paths(paths), target_clean, tx_out, win_opt, auto_paste);
    }
}

/// Turns selected paths into send items. Returns (items, folders_skipped) or an error status.
/// Without folders the list is exactly the selection, as before folder support.
fn expand_paths(paths: Vec<PathBuf>) -> Result<(Vec<SendItem>, bool), String> {
    let plain = |p: PathBuf| SendItem { path: p, id: None, tag: None };
    if !paths.iter().any(|p| p.is_dir()) {
        return Ok((paths.into_iter().map(plain).collect(), false));
    }
    if !PEER_FILE_FOLDER.load(Ordering::SeqCst) {
        logger::info("app.tunnel_ft_comp", "Partner lacks file-folder; skipping folders in selection.");
        return Ok((paths.into_iter().filter(|p| !p.is_dir()).map(plain).collect(), true));
    }
    let entries = walk_tree(&paths, MAX_TREE_ENTRIES, MAX_TREE_BYTES)?;
    let group = format!("fg_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros());
    let marker = entries.iter().find(|e| e.dir).map(|e| (e.path.clone(), e.rel.clone()));
    let mut items: Vec<SendItem> = entries
        .into_iter()
        .map(|e| SendItem {
            path: e.path,
            id: None,
            tag: Some(FolderTag { group: group.clone(), rel: e.rel, dir: e.dir, end: false }),
        })
        .collect();
    // Always close the group with a marker, so the receiver fills its clipboard even if some file failed here.
    if let Some((path, rel)) = marker {
        items.push(SendItem { path, id: None, tag: Some(FolderTag { group, rel, dir: true, end: true }) });
    }
    logger::info("app.tunnel_ft_comp", &format!("Sending folder selection: {} entries.", items.len() - 1));
    Ok((items, false))
}

/// Sends a folder entry (one `start` with `dir`, answered by an ACK; no bytes follow).
fn send_folder_entry(
    tag: &FolderTag,
    target_clean: &str,
    tx_out: &Sender<OutgoingMsg>,
    win_opt: &Option<Weak<MainWindow>>,
    auto_paste: bool,
) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros();
    let transfer_id = format!("fd_{}_{}", ts, SEQ.fetch_add(1, Ordering::Relaxed));
    let name = tag.rel.rsplit('\\').next().unwrap_or(&tag.rel);
    let expect_ack = PEER_FILE_ACK.load(Ordering::SeqCst);
    let (ack_tx, ack_rx) = mpsc::channel();
    if expect_ack {
        PENDING_ACKS.lock().unwrap().push((transfer_id.clone(), ack_tx));
    }
    let msg = json!({
        "type": "relay-file",
        "targetClean": target_clean,
        "payload": {
            "action": "start", "id": transfer_id, "name": name, "size": 0, "dir": true,
            "group": tag.group, "rel": tag.rel, "groupEnd": tag.end, "autoPaste": auto_paste
        }
    });
    IN_FLIGHT_FILE_CHUNKS.fetch_add(1, Ordering::SeqCst);
    let status = (!tag.end).then(|| format!("Mengirim folder {}...", tag.rel));
    let _ = tx_out.send(OutgoingMsg::FileTransfer { msg, status_text: status });
    if !expect_ack {
        return;
    }
    match ack_rx.recv_timeout(ACK_TIMEOUT) {
        Ok(true) if tag.end => set_status(win_opt, format!("Terkirim: {}", tag.rel)),
        Ok(true) => {}
        Ok(false) => set_status(win_opt, format!("Gagal mengirim folder {}", tag.rel)),
        Err(_) => {
            PENDING_ACKS.lock().unwrap().retain(|(pid, _)| pid != &transfer_id);
            set_status(win_opt, format!("Gagal mengirim folder {}: tidak ada konfirmasi penerima", tag.rel));
        }
    }
}

fn send_job(
    job: SendJob,
    target_clean: String,
    tx_out: Sender<OutgoingMsg>,
    win_opt: Option<Weak<MainWindow>>,
    auto_paste: bool,
) {
    if target_clean.is_empty() {
        return;
    }

    thread::spawn(move || {
        // Folders are walked here, on the worker thread, so a big tree never stalls the UI or the clipboard watcher.
        let (items, skipped_folders) = match job {
            SendJob::Pull(path, tid) => (vec![SendItem { path, id: Some(tid), tag: None }], false),
            SendJob::Paths(paths) => match expand_paths(paths) {
                Ok(expanded) => expanded,
                Err(status) => {
                    set_status(&win_opt, status);
                    return;
                }
            },
        };
        let epoch = TRANSFER_EPOCH.load(Ordering::SeqCst);
        for SendItem { path, id: forced_id, tag } in items {
            if TRANSFER_EPOCH.load(Ordering::SeqCst) != epoch {
                set_status(&win_opt, "Gagal mengirim berkas: koneksi terputus".into());
                return;
            }
            if let Some(folder) = tag.as_ref().filter(|t| t.dir) {
                send_folder_entry(folder, &target_clean, &tx_out, &win_opt, auto_paste);
                continue;
            }
            if !path.is_file() {
                continue;
            }

            let wire_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file.bin")
                .to_string();
            // Inside a folder the status shows the relative path; the wire `name` stays a plain file name.
            let file_name = tag.as_ref().map_or_else(|| wire_name.clone(), |t| t.rel.clone());

            let mut file = match File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    logger::error(
                        "app.tunnel_ft_comp",
                        &format!("Failed to open file {:?}: {}", path, e),
                    );
                    set_status(&win_opt, format!("Gagal membuka {}", file_name));
                    continue;
                }
            };

            let file_size = match file.metadata() {
                Ok(m) if m.len() <= 2 * 1024 * 1024 * 1024 => m.len(),
                _ => {
                    set_status(&win_opt, format!("Gagal mengirim {}: ukuran melebihi 2GB", file_name));
                    continue;
                }
            };
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros();
            let transfer_id = forced_id.unwrap_or_else(|| format!("ft_{}", ts));

            logger::info(
                "app.tunnel_ft_comp",
                &format!(
                    "Starting file transfer id={} name={} bytes={} auto_paste={}",
                    transfer_id, file_name, file_size, auto_paste
                ),
            );

            // Sampled per file: the session partner (and its caps) may change while a background transfer runs.
            let expect_ack = PEER_FILE_ACK.load(Ordering::SeqCst);
            let (ack_tx, ack_rx) = mpsc::channel();
            PENDING_ACKS.lock().unwrap().push((transfer_id.clone(), ack_tx));

            let mut start_msg = json!({
                "type": "relay-file",
                "targetClean": target_clean,
                "payload": {
                    "action": "start",
                    "id": transfer_id,
                    "name": wire_name,
                    "size": file_size
                }
            });
            if let Some(t) = &tag {
                start_msg["payload"]["group"] = json!(t.group);
                start_msg["payload"]["rel"] = json!(t.rel);
            }
            IN_FLIGHT_FILE_CHUNKS.fetch_add(1, Ordering::SeqCst);
            let _ = tx_out.send(OutgoingMsg::FileTransfer {
                msg: start_msg,
                status_text: Some(format!("Mengirim {} (0%)...", file_name)),
            });

            let mut buf = vec![0u8; CHUNK_SIZE];
            let mut hash = Sha256::new();
            let mut sent_bytes = 0u64;
            let mut chunk_idx = 0usize;
            let mut aborted = false;

            loop {
                // An early NACK (receiver rejected start) stops sending chunks the relay would drop anyway.
                if TRANSFER_EPOCH.load(Ordering::SeqCst) != epoch || ack_rx.try_recv() == Ok(false) {
                    aborted = true;
                    break;
                }
                // Bounded backpressure: wait if >48 chunks (~1.5 MB) are still waiting in network queue
                while IN_FLIGHT_FILE_CHUNKS.load(Ordering::Relaxed) > 48 {
                    if TRANSFER_EPOCH.load(Ordering::SeqCst) != epoch {
                        aborted = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(8));
                }
                if aborted {
                    break;
                }

                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        hash.update(&buf[..n]);
                        sent_bytes += n as u64;
                        chunk_idx += 1;
                        let b64 = to_base64(&buf[..n]);
                        let chunk_msg = json!({
                            "type": "relay-file",
                            "targetClean": target_clean,
                            "payload": {
                                "action": "chunk",
                                "id": transfer_id,
                                "chunk": b64
                            }
                        });

                        let status_update = if file_size > 0 && (chunk_idx % 4 == 0 || sent_bytes == file_size) {
                            let pct = ((sent_bytes * 100) / file_size).min(99);
                            Some(format!("Mengirim {} ({}%)...", file_name, pct))
                        } else {
                            None
                        };

                        IN_FLIGHT_FILE_CHUNKS.fetch_add(1, Ordering::SeqCst);
                        let _ = tx_out.send(OutgoingMsg::FileTransfer {
                            msg: chunk_msg,
                            status_text: status_update,
                        });

                        thread::sleep(Duration::from_millis(3));
                    }
                    Err(e) => {
                        logger::error("app.tunnel_ft_comp", &format!("Read failed for {:?}: {}", path, e));
                        aborted = true;
                        break;
                    }
                }
            }

            if aborted || sent_bytes != file_size {
                logger::error("app.tunnel_ft_comp", &format!("Transfer incomplete for {:?}", path));
                let mut pending = PENDING_ACKS.lock().unwrap();
                if let Some(pos) = pending.iter().position(|(pid, _)| pid == &transfer_id) {
                    pending.swap_remove(pos);
                }
                set_status(&win_opt, format!("Gagal mengirim {}", file_name));
                continue;
            }

            let checksum: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
            let end_msg = json!({
                "type": "relay-file",
                "targetClean": target_clean,
                "payload": {
                    "action": "end",
                    "id": transfer_id,
                    "name": wire_name,
                    "sha256": checksum,
                    "autoPaste": auto_paste
                }
            });
            IN_FLIGHT_FILE_CHUNKS.fetch_add(1, Ordering::SeqCst);
            let _ = tx_out.send(OutgoingMsg::FileTransfer {
                msg: end_msg,
                status_text: Some(format!("Menunggu konfirmasi: {}", file_name)),
            });

            logger::info(
                "app.tunnel_ft_comp",
                &format!("Queued final chunk for file transfer id={} name={}", transfer_id, file_name),
            );

            // Wait for receiver ACK (or queue drain fallback for legacy peers)
            if expect_ack {
                match ack_rx.recv_timeout(ACK_TIMEOUT) {
                    Ok(true) => {
                        logger::info("app.tunnel_ft_comp", &format!("Transfer ACK received: {}", file_name));
                        set_status(&win_opt, format!("Terkirim: {}", file_name));
                    }
                    Ok(false) => {
                        logger::warn("app.tunnel_ft_comp", &format!("Transfer NACK received: {}", file_name));
                        set_status(&win_opt, format!("Gagal mengirim {}: ditolak atau checksum tidak cocok", file_name));
                    }
                    Err(_) => {
                        let mut pending = PENDING_ACKS.lock().unwrap();
                        if let Some(pos) = pending.iter().position(|(pid, _)| pid == &transfer_id) {
                            pending.swap_remove(pos);
                        }
                        logger::warn("app.tunnel_ft_comp", &format!("Transfer ACK timeout: {}", file_name));
                        set_status(&win_opt, format!("Gagal mengirim {}: tidak ada konfirmasi penerima", file_name));
                    }
                }
            } else {
                let mut pending = PENDING_ACKS.lock().unwrap();
                if let Some(pos) = pending.iter().position(|(pid, _)| pid == &transfer_id) {
                    pending.swap_remove(pos);
                }
                let mut lost = false;
                while IN_FLIGHT_FILE_CHUNKS.load(Ordering::Relaxed) > 0 {
                    if TRANSFER_EPOCH.load(Ordering::SeqCst) != epoch {
                        lost = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                // Legacy receivers send no ACK: "Terkirim" here only means every byte left this socket.
                set_status(&win_opt, if lost {
                    format!("Gagal mengirim {}: koneksi terputus", file_name)
                } else {
                    format!("Terkirim (tanpa konfirmasi penerima): {}", file_name)
                });
            }
        }
        if skipped_folders {
            set_status(&win_opt, SKIPPED_FOLDERS.into());
        }
    });
}

/// Offers copied files to the partner (names and sizes only; contents are read when pulled) and sets the
/// status line. Folders go only to `file-folder` partners, as relative paths plus folder entries; their walk
/// runs on its own thread so the clipboard watcher keeps polling.
pub fn offer_files(paths: Vec<PathBuf>, target_clean: String, tx_out: Sender<OutgoingMsg>, win_opt: Option<Weak<MainWindow>>) {
    if target_clean.is_empty() || paths.is_empty() {
        return;
    }
    let generation = OFFER_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let has_dir = paths.iter().any(|p| p.is_dir());
    if !has_dir || !PEER_FILE_FOLDER.load(Ordering::SeqCst) {
        // Top-level files only, exactly as before folder support.
        let files: Vec<(Option<PathBuf>, Value)> = paths
            .iter()
            .filter_map(|p| {
                let size = fs::metadata(p).ok().filter(|m| m.is_file() && m.len() <= MAX_FILE_BYTES)?.len();
                Some((Some(p.clone()), json!({ "name": p.file_name()?.to_str()?, "size": size })))
            })
            .take(MAX_OFFER_FILES)
            .collect();
        let n = files.len();
        let status = match (publish_offer(generation, files, &target_clean, &tx_out), has_dir) {
            (true, false) => Some(format!("{} berkas siap ditempel di perangkat lain", paths.len())),
            (true, true) => Some(format!("{} berkas siap ditempel. {}", n, SKIPPED_FOLDERS)),
            (false, true) => Some(SKIPPED_FOLDERS.to_string()),
            (false, false) => None,
        };
        if let Some(status) = status {
            set_status(&win_opt, status);
        }
        return;
    }
    set_status(&win_opt, "Menyiapkan folder...".into());
    thread::spawn(move || {
        let status = match walk_tree(&paths, MAX_TREE_ENTRIES, MAX_TREE_BYTES) {
            Ok(entries) => {
                let listing = entries
                    .into_iter()
                    .map(|e| match e.dir {
                        true => (None, json!({ "name": e.rel, "dir": true })),
                        false => (Some(e.path), json!({ "name": e.rel, "size": e.size })),
                    })
                    .collect();
                publish_offer(generation, listing, &target_clean, &tx_out)
                    .then(|| format!("{} item siap ditempel di perangkat lain", paths.len()))
            }
            Err(status) => Some(status),
        };
        if let Some(status) = status.filter(|_| OFFER_GEN.load(Ordering::SeqCst) == generation) {
            set_status(&win_opt, status);
        }
    });
}

/// Records and sends one offer unless a newer copy already superseded it. `None` paths are folder entries.
fn publish_offer(generation: u64, entries: Vec<(Option<PathBuf>, Value)>, target_clean: &str, tx_out: &Sender<OutgoingMsg>) -> bool {
    if entries.is_empty() {
        return false;
    }
    // Held while sending so offers leave in copy order even when a folder walk finishes late.
    let mut offers = OUR_OFFERS.lock().unwrap();
    if OFFER_GEN.load(Ordering::SeqCst) != generation {
        return false;
    }
    let offer_id = format!("of_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros());
    let (paths, listing): (Vec<Option<PathBuf>>, Vec<Value>) = entries.into_iter().unzip();
    logger::info("app.clip_comp", &format!("Relaying file offer {} ({} entries).", offer_id, listing.len()));
    offers.push((offer_id.clone(), target_clean.to_string(), paths));
    let excess = offers.len().saturating_sub(4);
    offers.drain(..excess);
    let _ = tx_out.send(OutgoingMsg::Json(json!({
        "type": "relay-file",
        "targetClean": target_clean,
        "payload": { "action": "offer", "id": offer_id, "files": listing }
    })));
    true
}

/// Partner pasted one of our offered files: send it under the transfer ID it chose.
pub fn serve_pull(from: &str, payload: &Value, tx_out: &Sender<OutgoingMsg>, win_opt: Option<Weak<MainWindow>>) {
    let offer_id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let index = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as usize;
    let Some(tid) = payload
        .get("tid")
        .and_then(|v| v.as_str())
        .filter(|t| t.len() <= 80 && t.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'))
    else {
        return;
    };
    let path = OUR_OFFERS
        .lock()
        .unwrap()
        .iter()
        .find(|(id, partner, _)| id == offer_id && partner == from)
        .and_then(|(_, _, paths)| paths.get(index).cloned().flatten()); // folder entries are never pulled
    match path {
        Some(path) => {
            logger::info("app.ft_src_session", &format!("Pull for offer {} file {} ({}).", offer_id, index, tid));
            send_job(SendJob::Pull(path, tid.to_string()), from.to_string(), tx_out.clone(), win_opt, false);
        }
        None => logger::warn("app.ft_src_session", &format!("Rejected pull for unknown offer {} file {}.", offer_id, index)),
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    #[test]
    fn received_clipboard_matches_exact_file_set_only() {
        *RECEIVED_CLIPBOARD.lock().unwrap() = vec![r"C:\Users\a\Downloads\OpenDesk\x.txt".into()];
        assert!(is_received_clipboard(&[PathBuf::from(r"c:\users\a\downloads\opendesk\X.TXT")]));
        assert!(!is_received_clipboard(&[PathBuf::from(r"C:\other.txt")]));
        assert!(!is_received_clipboard(&[]));
        assert!(!is_received_clipboard(&[
            PathBuf::from(r"C:\Users\a\Downloads\OpenDesk\x.txt"),
            PathBuf::from(r"C:\other.txt"),
        ]));
    }
}
