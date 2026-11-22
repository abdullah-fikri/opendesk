//! File browser (`domain::file_browser`). The host answers the listings and fetches of its session's viewer, as the
//! signed-in user (`infra::user_scope`); the viewer keeps its requests, places the files that arrive for them in
//! the folder it chose, and shows the answers. Uploads are transfers whose `start` names the host folder (`dest`).
//!
//! SECURITY: a host serves only the viewer of its current session (the one that already controls its screen), and
//! every file system call for it runs as the signed-in user, so the browser reaches nothing that user could not.
//! A viewer places a received file only for a request it made itself (`browse` = one of its own request numbers).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::domain::file_browser::{BrowseMsg, Entry, MAX_ENTRIES, sort, valid_folder_name};
use crate::infra::{logger, user_scope};
use crate::protocol::{FilePayload, Outgoing};
use crate::service::context::{AppContext, Ctx};
use crate::service::file_transfer::send_browsed;
use crate::service::ui_port::BrowseUpdate;

/// A listing request unanswered this long is forgotten (its answer would no longer be shown).
const LIST_KEEP: Duration = Duration::from_secs(30);
/// A download request keeps placing the files that arrive for it this long.
const GET_KEEP: Duration = Duration::from_secs(6 * 3600);

static NEXT_REQ: AtomicU64 = AtomicU64::new(1);
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

/// A request this viewer sent and still expects answers (or files) for.
struct Pending {
    partner: String,
    req: u64,
    /// Where its files go (downloads); None for listings and new folders.
    into: Option<PathBuf>,
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

fn remember(partner: &str, into: Option<PathBuf>) -> u64 {
    let req = NEXT_REQ.fetch_add(1, Ordering::Relaxed);
    let mut pending = PENDING.lock();
    pending.retain(|p| p.at.elapsed() < if p.into.is_some() { GET_KEEP } else { LIST_KEEP });
    pending.push(Pending { partner: partner.to_string(), req, into, at: Instant::now() });
    req
}

fn send(ctx: &AppContext, partner: &str, msg: &BrowseMsg) {
    let _ = ctx.outbox.send(Outgoing::Browse { target: partner.to_string(), payload: msg.encode() });
}

/// Asks `partner` for the entries of `path` ("" = its top level); the answer comes as [`BrowseUpdate`].
pub fn list_remote(ctx: &AppContext, partner: &str, path: &str) {
    let req = remember(partner, None);
    send(ctx, partner, &BrowseMsg::List { req, path: path.to_string() });
}

/// Asks `partner` to send `paths`; they are saved in `into` on arrival.
pub fn download(ctx: &AppContext, partner: &str, paths: Vec<String>, into: PathBuf) {
    if paths.is_empty() {
        return;
    }
    let req = remember(partner, Some(into));
    send(ctx, partner, &BrowseMsg::Get { req, paths });
}

/// Sends local `paths` to `partner`'s folder `remote_dir`.
pub fn upload(ctx: &AppContext, partner: &str, paths: Vec<PathBuf>, remote_dir: String) {
    send_browsed(ctx, paths, partner.to_string(), None, Some(remote_dir));
}

/// Creates folder `name` in `partner`'s folder `path`; a fresh listing of `path` follows.
pub fn mkdir_remote(ctx: &AppContext, partner: &str, path: &str, name: &str) {
    let req = remember(partner, None);
    send(ctx, partner, &BrowseMsg::Mkdir { req, path: path.to_string(), name: name.to_string() });
}

/// Lists a folder of this machine (as the signed-in user).
pub fn list_local(path: &str) -> Result<Listing, String> {
    user_scope::as_user(|| list(path)).map_err(str::to_string)?
}

/// Creates folder `name` in local folder `path` (as the signed-in user).
pub fn mkdir_local(path: &str, name: &str) -> Result<(), String> {
    user_scope::as_user(|| mkdir(path, name)).map_err(str::to_string)?
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
    user_scope::as_user(|| {
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
    .map_err(str::to_string)?
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
    match msg {
        BrowseMsg::List { path, .. } => match list_local(&path) {
            Ok(l) => BrowseMsg::Listing { req, path: l.path, parent: l.parent, entries: l.entries, more: l.more },
            Err(e) => failed(e),
        },
        BrowseMsg::Mkdir { path, name, .. } => match mkdir_local(&path, &name) {
            Ok(()) => BrowseMsg::Done { req },
            Err(e) => failed(e),
        },
        BrowseMsg::Get { paths, .. } => {
            let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).filter(|p| p.is_absolute()).collect();
            logger::info("file_browser", &format!("Viewer {from} fetches {} item(s).", paths.len()));
            send_browsed(ctx, paths, from.to_string(), Some(req), None);
            BrowseMsg::Done { req }
        }
        _ => failed("Unknown request".into()),
    }
}

/// Viewer: an answer to one of its requests.
fn on_answer(ctx: &AppContext, from: &str, msg: BrowseMsg) {
    let req = msg.req();
    let known = {
        let mut pending = PENDING.lock();
        let found = pending.iter().position(|p| p.partner == from && p.req == req);
        // Listings and new folders are answered once; downloads stay for their files.
        match found {
            Some(i) if pending[i].into.is_none() => {
                pending.remove(i);
                true
            }
            Some(_) => true,
            None => false,
        }
    };
    if !known {
        return;
    }
    let partner = from.to_string();
    match msg {
        BrowseMsg::Listing { path, parent, entries, more, .. } => {
            ctx.ui.browse(BrowseUpdate::Remote { partner, path, parent, entries, more })
        }
        BrowseMsg::Failed { message, .. } => ctx.ui.browse(BrowseUpdate::Failed { partner, message }),
        BrowseMsg::Done { .. } => ctx.ui.browse(BrowseUpdate::Done { partner }),
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
}
