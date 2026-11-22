//! File browser (`domain::file_browser`). The host answers the listings and fetches of its session's viewer, as the
//! signed-in user (`infra::user_scope`); the viewer keeps its requests, places the files that arrive for them in
//! the folder it chose, and shows the answers. Uploads are transfers whose `start` names the host folder (`dest`).
//!
//! SECURITY: a host serves only the viewer of its current session (the one that already controls its screen), and
//! every file system call for it runs as the signed-in user, so the browser reaches nothing that user could not.
//! A viewer places a received file only for a request it made itself (`browse` = one of its own request numbers).
//! Every path or name from the network passes `valid_host_dir` / `valid_host_item` / `valid_folder_name` before
//! any file system call; archives are unpacked by `infra::archive` (zip-slip, link and size guards). Destructive
//! requests (delete) are only sent after the viewer's user confirmed them; a host never overwrites on rename or
//! extract (taken names fail or get a free name).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::domain::file_browser::{
    BrowseMsg, Entry, HOST_OPS, MAX_ENTRIES, sort, valid_folder_name, valid_host_dir, valid_host_item,
};
use crate::domain::transfer::MAX_TREE_BYTES;
use crate::infra::{archive, logger, shell, user_scope};
use crate::protocol::{FilePayload, Outgoing};
use crate::service::context::{AppContext, Ctx};
use crate::service::file_transfer::send_browsed;
use crate::service::file_transfer::tree;
use crate::service::ui_port::BrowseUpdate;

/// A listing request unanswered this long is forgotten (its answer would no longer be shown).
const LIST_KEEP: Duration = Duration::from_secs(30);
/// A download request keeps placing the files that arrive for it this long.
const GET_KEEP: Duration = Duration::from_secs(6 * 3600);

static NEXT_REQ: AtomicU64 = AtomicU64::new(1);
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
/// Download folders whose files open as soon as they are placed (Open on a remote file).
static OPEN_DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// Host: at most this often a progress message for a long request.
const PROGRESS_EVERY: Duration = Duration::from_millis(400);

/// A request this viewer sent and still expects answers (or files) for.
struct Pending {
    partner: String,
    req: u64,
    /// Where its files go (downloads); None for listings and operations.
    into: Option<PathBuf>,
    /// A listing (its failure is shown in the pane, an operation's in the status line).
    list: bool,
    at: Instant,
}

/// A listing: the folder, its parent ("" = the top level; None at the top level), the entries, and whether the
/// folder held more than [`MAX_ENTRIES`].
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
    pub more: bool,
}

// ---- Viewer side ----

fn remember(partner: &str, into: Option<PathBuf>, list: bool) -> u64 {
    let req = NEXT_REQ.fetch_add(1, Ordering::Relaxed);
    let mut pending = PENDING.lock();
    // Operations (an extraction can take a while) are kept like downloads.
    pending.retain(|p| p.at.elapsed() < if p.list { LIST_KEEP } else { GET_KEEP });
    pending.push(Pending { partner: partner.to_string(), req, into, list, at: Instant::now() });
    req
}

fn send(ctx: &AppContext, partner: &str, msg: &BrowseMsg) {
    let _ = ctx.outbox.send(Outgoing::Browse { target: partner.to_string(), payload: msg.encode() });
}

/// Asks `partner` for the entries of `path` ("" = its top level); the answer comes as [`BrowseUpdate`].
pub fn list_remote(ctx: &AppContext, partner: &str, path: &str) {
    let req = remember(partner, None, true);
    send(ctx, partner, &BrowseMsg::List { req, path: path.to_string() });
}

/// Asks `partner` to send `paths`; they are saved in `into` on arrival.
pub fn download(ctx: &AppContext, partner: &str, paths: Vec<String>, into: PathBuf) {
    if paths.is_empty() {
        return;
    }
    let req = remember(partner, Some(into), false);
    send(ctx, partner, &BrowseMsg::Get { req, paths });
}

/// Open on a remote file: fetches it into a fresh temporary folder; it opens with its default app as soon as
/// it is placed there (the transfer panel shows the download meanwhile).
pub fn open_remote(ctx: &AppContext, partner: &str, path: String) -> Result<(), String> {
    let dir = std::env::temp_dir().join("OpenDesk").join("open").join(format!(
        "{}-{}",
        crate::service::file_transfer::now_ms(),
        NEXT_REQ.load(Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).map_err(|e| format!("Cannot prepare a temporary folder: {}", friendly(&e)))?;
    OPEN_DIRS.lock().push(dir.clone());
    download(ctx, partner, vec![path], dir);
    Ok(())
}

/// Sends local `paths` to `partner`'s folder `remote_dir`.
pub fn upload(ctx: &AppContext, partner: &str, paths: Vec<PathBuf>, remote_dir: String) {
    send_browsed(ctx, paths, partner.to_string(), None, Some(remote_dir));
}

/// Creates folder `name` in `partner`'s folder `path`; a fresh listing of `path` follows.
pub fn mkdir_remote(ctx: &AppContext, partner: &str, path: &str, name: &str) {
    let req = remember(partner, None, false);
    send(ctx, partner, &BrowseMsg::Mkdir { req, path: path.to_string(), name: name.to_string() });
}

/// Asks `partner` for one of the explorer operations (`HOST_OPS`; only offered when its listing named them).
/// The answer comes as [`BrowseUpdate::Done`] / [`BrowseUpdate::OpFailed`], long ones with progress first.
pub fn request_remote(ctx: &AppContext, partner: &str, make: impl FnOnce(u64) -> BrowseMsg) {
    let req = remember(partner, None, false);
    send(ctx, partner, &make(req));
}

/// Lists a folder of this machine (as the signed-in user).
pub fn list_local(path: &str) -> Result<Listing, String> {
    user_scope::as_user(|| list(path)).map_err(str::to_string)?
}

/// Creates folder `name` in local folder `path` (as the signed-in user).
pub fn mkdir_local(path: &str, name: &str) -> Result<(), String> {
    user_scope::as_user(|| mkdir(path, name)).map_err(str::to_string)?
}

/// Renames local item `path` to `name` (same folder), as the signed-in user.
pub fn rename_local(path: &str, name: &str) -> Result<(), String> {
    user_scope::as_user(|| rename(path, name)).map_err(str::to_string)?
}

/// Deletes local items, to the Recycle Bin with `trash` (Windows); returns what to tell the user.
pub fn delete_local(paths: &[String], trash: bool) -> Result<String, String> {
    user_scope::as_user(|| delete(paths, trash)).map_err(str::to_string)?
}

/// Unpacks local archive `path` next to it (or into a new folder named after it with `folder`).
pub fn extract_local(path: &str, folder: bool, progress: &mut dyn FnMut(u64, u64)) -> Result<String, String> {
    user_scope::as_user(|| extract(path, folder, progress)).map_err(str::to_string)?
}

/// Packs local items (all in one folder) into a new .zip in that folder.
pub fn compress_local(paths: &[String], progress: &mut dyn FnMut(u64, u64)) -> Result<String, String> {
    user_scope::as_user(|| compress(paths, progress)).map_err(str::to_string)?
}

/// Where a file that starts arriving from `from` goes, if it belongs to the file browser: the folder of this
/// viewer's own download request, or (on a host) the folder its session's viewer uploads to.
pub fn place_for(ctx: &AppContext, from: &str, payload: &FilePayload) -> Option<PathBuf> {
    if let Some(req) = payload.browse {
        return PENDING.lock().iter().find(|p| p.partner == from && p.req == req).and_then(|p| p.into.clone());
    }
    let dest = payload.dest.as_deref()?;
    (ctx.is_hosting() && ctx.partner() == from && Path::new(dest).is_absolute()).then(|| PathBuf::from(dest))
}

/// Moves a received file or folder from Downloads into `dir`, as the signed-in user, under a free name. Returns
/// where it ended up.
pub fn place(saved: &Path, dir: &Path) -> Result<PathBuf, String> {
    let placed = user_scope::as_user(|| {
        if !dir.is_dir() {
            return Err(format!("{} is not a folder", dir.display()));
        }
        let name = saved.file_name().ok_or("no file name")?.to_string_lossy().to_string();
        let target = free_name(dir, &name);
        if fs::rename(saved, &target).is_err() {
            // Another drive: copy, then remove the copy in Downloads.
            copy_all(saved, &target).map_err(|e| format!("could not save in {}: {e}", dir.display()))?;
            let _ = if saved.is_dir() { fs::remove_dir_all(saved) } else { fs::remove_file(saved) };
        }
        Ok(target)
    })
    .map_err(str::to_string)??;
    // Open on a remote file: it is here now.
    if placed.is_file() && OPEN_DIRS.lock().iter().any(|d| d == dir) {
        match shell::open(&placed) {
            Ok(()) => logger::info("file_browser", &format!("Opened {}", placed.display())),
            Err(e) => logger::warn("file_browser", &format!("Could not open {}: {e}", placed.display())),
        }
    }
    Ok(placed)
}

// ---- Both sides: a browser message from `from` ----

/// A file browser message from `from`, received over a direct path.
pub fn on_message(ctx: &Ctx, from: &str, payload: &[u8]) {
    let Some(msg) = BrowseMsg::decode(payload) else { return };
    if !msg.is_request() {
        return on_answer(ctx, from, msg);
    }
    // Only the viewer of this host's session may browse it.
    if !(ctx.is_hosting() && ctx.partner() == from) {
        logger::warn("file_browser", &format!("Ignored a file browser request from {from}: not this host's viewer."));
        return;
    }
    let (ctx, from) = (Ctx::clone(ctx), from.to_string());
    crate::infra::thread::spawn_named("file-browser", move || {
        let answer = serve(&ctx, &from, msg);
        send(&ctx, &from, &answer);
    });
}

/// A file browser message that came as a sealed `browse` event (no direct path; see `send_browse`).
pub fn on_event(ctx: &Ctx, from: &str, msg: serde_json::Value) {
    on_message(ctx, from, &serde_json::to_vec(&msg).unwrap_or_default());
}

/// Host: carries out one request of its viewer.
fn serve(ctx: &AppContext, from: &str, msg: BrowseMsg) -> BrowseMsg {
    let req = msg.req();
    let failed = |message: String| BrowseMsg::Failed { req, message };
    let done = |result: Result<String, String>| match result {
        Ok(note) => BrowseMsg::Done { req, note },
        Err(e) => failed(e),
    };
    // Long requests report how far they got, at most every PROGRESS_EVERY.
    let mut last = Instant::now();
    let mut progress = |d: u64, t: u64| {
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            send(ctx, from, &BrowseMsg::Progress { req, done: d, total: t });
        }
    };
    match msg {
        BrowseMsg::List { path, .. } if path.is_empty() || valid_host_dir(&path) => match list_local(&path) {
            Ok(l) => BrowseMsg::Listing {
                req,
                path: l.path,
                parent: l.parent,
                entries: l.entries,
                more: l.more,
                ops: HOST_OPS.iter().map(|s| s.to_string()).collect(),
            },
            Err(e) => failed(e),
        },
        BrowseMsg::Mkdir { path, name, .. } if valid_host_dir(&path) => {
            done(mkdir_local(&path, &name).map(|()| String::new()))
        }
        BrowseMsg::Get { paths, .. } => {
            let paths: Vec<PathBuf> = paths.into_iter().filter(|p| valid_host_dir(p)).map(PathBuf::from).collect();
            logger::info("file_browser", &format!("Viewer {from} fetches {} item(s).", paths.len()));
            send_browsed(ctx, paths, from.to_string(), Some(req), None);
            BrowseMsg::Done { req, note: String::new() }
        }
        BrowseMsg::Rename { path, name, .. } => {
            logger::info("file_browser", &format!("Viewer {from} renames an item."));
            done(rename_local(&path, &name).map(|()| String::new()))
        }
        BrowseMsg::Delete { paths, .. } => {
            logger::info("file_browser", &format!("Viewer {from} deletes {} item(s).", paths.len()));
            // For good: under SYSTEM the Recycle Bin would be the wrong user's (the viewer's prompt says so).
            done(delete_local(&paths, false))
        }
        BrowseMsg::Extract { path, folder, .. } => {
            logger::info("file_browser", &format!("Viewer {from} extracts an archive."));
            done(extract_local(&path, folder, &mut progress))
        }
        BrowseMsg::Compress { paths, .. } => {
            logger::info("file_browser", &format!("Viewer {from} compresses {} item(s).", paths.len()));
            done(compress_local(&paths, &mut progress))
        }
        BrowseMsg::List { .. } | BrowseMsg::Mkdir { .. } => failed("That path cannot be used".into()),
        _ => failed("Unknown request".into()),
    }
}

/// Viewer: an answer to one of its requests.
fn on_answer(ctx: &AppContext, from: &str, msg: BrowseMsg) {
    let req = msg.req();
    let list = {
        let mut pending = PENDING.lock();
        let Some(i) = pending.iter().position(|p| p.partner == from && p.req == req) else { return };
        let list = pending[i].list;
        // Listings and operations are answered once (after any progress); downloads stay for their files.
        if pending[i].into.is_none() && !matches!(msg, BrowseMsg::Progress { .. }) {
            pending.remove(i);
        }
        list
    };
    let partner = from.to_string();
    match msg {
        BrowseMsg::Listing { path, parent, entries, more, ops, .. } => {
            ctx.ui.browse(BrowseUpdate::Remote { partner, path, parent, entries, more, ops })
        }
        BrowseMsg::Failed { message, .. } if list => ctx.ui.browse(BrowseUpdate::Failed { partner, message }),
        BrowseMsg::Failed { message, .. } => ctx.ui.browse(BrowseUpdate::OpFailed { partner, message }),
        BrowseMsg::Done { note, .. } => ctx.ui.browse(BrowseUpdate::Done { partner, note }),
        BrowseMsg::Progress { done, total, .. } => ctx.ui.browse(BrowseUpdate::Progress { partner, done, total }),
        _ => {}
    }
}

// ---- File system (the caller decides as whom) ----

fn list(path: &str) -> Result<Listing, String> {
    if path.is_empty() {
        return Ok(Listing { path: String::new(), parent: None, entries: top_level(), more: false });
    }
    let dir = Path::new(path);
    let read = fs::read_dir(dir).map_err(|e| format!("Cannot open {path}: {}", friendly(&e)))?;
    let mut entries = Vec::new();
    let mut more = false;
    for item in read.flatten() {
        if entries.len() >= MAX_ENTRIES {
            more = true;
            break;
        }
        let Ok(meta) = item.metadata() else { continue };
        entries.push(Entry {
            name: item.file_name().to_string_lossy().to_string(),
            path: item.path().to_string_lossy().to_string(),
            dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64),
        });
    }
    sort(&mut entries);
    let parent = Some(dir.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());
    Ok(Listing { path: path.to_string(), parent, entries, more })
}

/// The top level: drives (or `/`) and the user's own folders.
fn top_level() -> Vec<Entry> {
    let folder = |name: &str, path: PathBuf| Entry {
        name: name.to_string(),
        path: path.to_string_lossy().to_string(),
        dir: true,
        ..Entry::default()
    };
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from) {
        for name in ["Desktop", "Documents", "Downloads"] {
            let p = home.join(name);
            if p.is_dir() {
                out.push(folder(name, p));
            }
        }
        out.push(folder(if cfg!(windows) { "Home" } else { "Home (~)" }, home));
    }
    if cfg!(windows) {
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            if Path::new(&root).is_dir() {
                out.push(folder(&root, PathBuf::from(&root)));
            }
        }
    } else {
        out.push(folder("/", PathBuf::from("/")));
    }
    out
}

fn mkdir(path: &str, name: &str) -> Result<(), String> {
    if !valid_folder_name(name) || !Path::new(path).is_absolute() {
        return Err("That folder name cannot be used".into());
    }
    fs::create_dir(Path::new(path).join(name.trim())).map_err(|e| format!("Cannot create the folder: {}", friendly(&e)))
}

fn rename(path: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if !valid_host_item(path) || !valid_folder_name(name) {
        return Err("That name cannot be used".into());
    }
    let from = Path::new(path);
    let old = from.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if old == name {
        return Ok(());
    }
    let to = from.parent().map(|p| p.join(name)).ok_or("That name cannot be used")?;
    // Never replace another item; a case-only change ("a.txt" -> "A.txt") is the same item on Windows.
    if fs::symlink_metadata(&to).is_ok() && !old.eq_ignore_ascii_case(name) {
        return Err(format!("An item named \"{name}\" already exists"));
    }
    fs::rename(from, &to).map_err(|e| format!("Cannot rename {old}: {}", friendly(&e)))
}

fn delete(paths: &[String], trash: bool) -> Result<String, String> {
    if paths.is_empty() {
        return Ok(String::new());
    }
    if let Some(bad) = paths.iter().find(|p| !valid_host_item(p)) {
        return Err(format!("{bad} cannot be deleted"));
    }
    let items: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let n = items.len();
    let what = if n == 1 { "1 item".to_string() } else { format!("{n} items") };
    if trash {
        shell::trash(&items)?;
        return Ok(format!("Moved {what} to the Recycle Bin"));
    }
    for p in &items {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let meta = fs::symlink_metadata(p).map_err(|e| format!("Cannot delete {name}: {}", friendly(&e)))?;
        // A link is removed itself, never what it points to (remove_dir_all does not follow links either).
        let result = if meta.is_dir() {
            fs::remove_dir_all(p)
        } else {
            fs::remove_file(p).or_else(|e| if meta.file_type().is_symlink() { fs::remove_dir(p) } else { Err(e) })
        };
        result.map_err(|e| format!("Cannot delete {name}: {}", friendly(&e)))?;
    }
    Ok(format!("Deleted {what}"))
}

fn extract(path: &str, folder: bool, progress: &mut dyn FnMut(u64, u64)) -> Result<String, String> {
    if !valid_host_item(path) {
        return Err("That archive cannot be used".into());
    }
    let file = Path::new(path);
    let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if archive::kind_of(&name).is_none() {
        return Err(format!("{name} is not an archive OpenDesk can extract (.zip, .tar, .tar.gz)"));
    }
    let parent = file.parent().ok_or("That archive cannot be used")?;
    let into = if folder {
        let stem = archive::stem_of(&name);
        let stem = if valid_folder_name(stem) { stem } else { "Extracted" };
        tree::create_dir_unique(parent, stem).ok_or("Cannot create the folder to extract into")?
    } else {
        parent.to_path_buf()
    };
    match archive::extract(file, &into, &archive::LIMITS, progress) {
        Ok(report) if folder => {
            Ok(format!("{} into {}", report.note(), into.file_name().unwrap_or_default().to_string_lossy()))
        }
        Ok(report) => Ok(report.note()),
        Err(e) => {
            if folder {
                let _ = fs::remove_dir(&into);
            }
            Err(e)
        }
    }
}

fn compress(paths: &[String], progress: &mut dyn FnMut(u64, u64)) -> Result<String, String> {
    if paths.is_empty() || paths.iter().any(|p| !valid_host_item(p)) {
        return Err("Those items cannot be compressed".into());
    }
    let roots: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let parent = roots[0].parent().ok_or("Those items cannot be compressed")?.to_path_buf();
    if roots.iter().any(|r| r.parent() != Some(parent.as_path())) {
        return Err("Pick items from one folder to compress them together".into());
    }
    let entries = tree::walk(&roots, archive::LIMITS.entries, MAX_TREE_BYTES)
        .map_err(|e| e.trim_start_matches("Failed: ").to_string())?;
    if entries.is_empty() {
        return Err("Nothing to compress".into());
    }
    let items: Vec<archive::Item> = entries
        .into_iter()
        .map(|e| archive::Item { path: e.path, name: e.rel.replace('\\', "/"), dir: e.dir })
        .collect();
    let base = match roots.as_slice() {
        [one] if one.is_dir() => one.file_name().map(|n| n.to_string_lossy().to_string()),
        [one] => one.file_stem().map(|n| n.to_string_lossy().to_string()),
        _ => None,
    }
    .filter(|b| valid_folder_name(b))
    .unwrap_or_else(|| "Archive".into());
    let out = free_name(&parent, &format!("{base}.zip"));
    let n = archive::compress(&items, &out, progress)?;
    let shown = out.file_name().unwrap_or_default().to_string_lossy().to_string();
    Ok(format!("Compressed {n} item{} into {shown}", if n == 1 { "" } else { "s" }))
}

/// `dir/name`, or `dir/name (1).ext`, `(2)`... when taken.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..10_000).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap_or(first)
}

fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        fs::create_dir(to)?;
        for item in fs::read_dir(from)? {
            let item = item?;
            copy_all(&item.path(), &to.join(item.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

fn friendly(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "access denied".into(),
        std::io::ErrorKind::NotFound => "not found".into(),
        _ => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opendesk-browse-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn listing_names_full_paths_and_the_parent() {
        let dir = temp("list");
        fs::create_dir(dir.join("Sub")).unwrap();
        fs::write(dir.join("b.txt"), b"hello").unwrap();
        let l = list(&dir.to_string_lossy()).unwrap();
        assert_eq!(l.parent.as_deref(), dir.parent().map(|p| p.to_string_lossy().to_string()).as_deref());
        assert_eq!(
            l.entries.iter().map(|e| (e.name.as_str(), e.dir, e.size)).collect::<Vec<_>>(),
            [("Sub", true, 0), ("b.txt", false, 5)]
        );
        assert_eq!(PathBuf::from(&l.entries[1].path), dir.join("b.txt"));
        assert!(list(&dir.join("missing").to_string_lossy()).is_err());
        let top = list("").unwrap();
        assert!(top.parent.is_none() && top.entries.iter().all(|e| e.dir && Path::new(&e.path).is_absolute()));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn placing_never_overwrites() {
        let dir = temp("place");
        let downloads = temp("place-src");
        fs::write(dir.join("a.txt"), b"old").unwrap();
        fs::write(downloads.join("a.txt"), b"new").unwrap();
        let placed = place(&downloads.join("a.txt"), &dir).unwrap();
        assert_eq!(placed, dir.join("a (1).txt"));
        assert_eq!(fs::read(dir.join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(&placed).unwrap(), b"new");
        assert!(!downloads.join("a.txt").exists());
        assert!(place(&downloads.join("gone.txt"), &dir.join("no-such-dir")).is_err());
        assert!(mkdir(&dir.to_string_lossy(), "..").is_err());
        assert!(mkdir(&dir.to_string_lossy(), "New").is_ok() && dir.join("New").is_dir());
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(downloads);
    }

    #[test]
    fn rename_and_delete_stay_in_their_folder() {
        let dir = temp("ops");
        let s = |p: PathBuf| p.to_string_lossy().to_string();
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::write(dir.join("b.txt"), b"b").unwrap();
        assert!(rename(&s(dir.join("a.txt")), "b.txt").unwrap_err().contains("already exists"));
        for bad in ["../x.txt", "sub/x.txt", "CON", ""] {
            assert!(rename(&s(dir.join("a.txt")), bad).is_err(), "{bad}");
        }
        rename(&s(dir.join("a.txt")), "c.txt").unwrap();
        assert!(dir.join("c.txt").is_file() && !dir.join("a.txt").exists());
        fs::create_dir_all(dir.join("Sub/Deep")).unwrap();
        fs::write(dir.join("Sub/Deep/x"), b"x").unwrap();
        let note = delete(&[s(dir.join("Sub")), s(dir.join("c.txt"))], false).unwrap();
        assert_eq!(note, "Deleted 2 items");
        assert!(!dir.join("Sub").exists() && !dir.join("c.txt").exists() && dir.join("b.txt").exists());
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        assert!(delete(&[root.to_string()], false).is_err(), "a root is never deleted");
        assert!(delete(&["relative".to_string()], false).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn compress_then_extract_into_a_new_folder() {
        let dir = temp("zip");
        let s = |p: PathBuf| p.to_string_lossy().to_string();
        fs::create_dir_all(dir.join("Photos/2024")).unwrap();
        fs::write(dir.join("Photos/2024/a.jpg"), b"jpeg").unwrap();
        fs::write(dir.join("notes.txt"), b"notes").unwrap();
        let note = compress(&[s(dir.join("Photos")), s(dir.join("notes.txt"))], &mut |_, _| {}).unwrap();
        assert_eq!(note, "Compressed 4 items into Archive.zip");
        let one = compress(&[s(dir.join("Photos"))], &mut |_, _| {}).unwrap();
        assert!(one.ends_with("Photos.zip"), "{one}");
        let note = extract(&s(dir.join("Archive.zip")), true, &mut |_, _| {}).unwrap();
        assert_eq!(note, "Extracted 4 items into Archive");
        assert_eq!(fs::read(dir.join("Archive/Photos/2024/a.jpg")).unwrap(), b"jpeg");
        // A second time: a new folder, nothing overwritten.
        extract(&s(dir.join("Archive.zip")), true, &mut |_, _| {}).unwrap();
        assert!(dir.join("Archive (1)/notes.txt").is_file());
        assert!(extract(&s(dir.join("notes.txt")), false, &mut |_, _| {}).is_err());
        let other = temp("zip-other");
        fs::write(other.join("x"), b"x").unwrap();
        assert!(compress(&[s(dir.join("notes.txt")), s(other.join("x"))], &mut |_, _| {}).is_err());
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(other);
    }
}
