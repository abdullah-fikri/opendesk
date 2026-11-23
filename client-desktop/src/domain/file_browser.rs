//! File browser messages (two-pane browser of a session's host, like AnyDesk's file manager): the viewer lists the
//! host's folders and asks for files; uploads and downloads then run as ordinary transfers (`start` carrying `dest`
//! or `browse`, see `service::file_browser`). The messages travel only over a direct path (the file channel, else
//! the P2P path) as file stream messages of kind [`MSG_BROWSE`]: the relay never sees or forwards them.
//!
//! Paths are always built by the side that owns them: a listing names its own parent and every entry's full path,
//! so a Windows viewer browsing a Linux host never joins paths with the wrong separator.
//!
//! Explorer operations (rename, delete, extract, compress; docs/features/file-browser-explorer.md) came later: a
//! host that serves them lists them in every `listing` (`ops`), and a viewer only offers what the host listed, so
//! an older host (whose `ops` is empty) is never sent a request it would drop without answering.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::domain::transfer::safe_rel_path;

/// File stream message kind (next to `domain::bulk::MSG_JSON` and `MSG_BINARY`).
pub const MSG_BROWSE: u8 = 3;
/// Most entries in one listing; a bigger folder says so with `more`.
pub const MAX_ENTRIES: usize = 5000;
/// Operations beyond list/get/mkdir this build serves as a host; announced in every listing.
pub const HOST_OPS: [&str; 4] = ["rename", "delete", "extract", "compress"];

/// One file or folder in a listing.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Entry {
    /// Shown name ("report.pdf", or "C:\" / "Documents" in the top level).
    pub name: String,
    /// Full path on the side that listed it; what the other side sends back to open or fetch it.
    pub path: String,
    pub dir: bool,
    pub size: u64,
    /// Last write time, Unix milliseconds (0 = unknown).
    pub mtime: u64,
}

/// A browser message; `req` pairs every answer with its request.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum BrowseMsg {
    /// Viewer: list `path` ("" = the top level: drives and the user's folders).
    List { req: u64, path: String },
    /// Viewer: send these files and folders; their transfers carry `browse: req`.
    Get { req: u64, paths: Vec<String> },
    /// Viewer: create folder `name` inside `path`.
    Mkdir { req: u64, path: String, name: String },
    /// Viewer: give the file or folder `path` the new name `name` (same folder). Op "rename".
    Rename { req: u64, path: String, name: String },
    /// Viewer: delete these files and folders (the viewer asked its user first). Op "delete".
    Delete { req: u64, paths: Vec<String> },
    /// Viewer: unpack archive `path` next to it, or with `folder` into a new folder named after it. Op "extract".
    Extract {
        req: u64,
        path: String,
        #[serde(default)]
        folder: bool,
    },
    /// Viewer: pack these items (all in one folder) into a new .zip in that folder. Op "compress".
    Compress { req: u64, paths: Vec<String> },
    /// Host: the entries of `path`; `parent` is "" at the top level. `ops` = the [`HOST_OPS`] it serves (empty
    /// from hosts older than them).
    Listing {
        req: u64,
        path: String,
        parent: Option<String>,
        entries: Vec<Entry>,
        more: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ops: Vec<String>,
    },
    /// Host: how far a long request (extract, compress) got, in bytes.
    Progress { req: u64, done: u64, total: u64 },
    /// Host: the request was carried out; `note` optionally says how ("Extracted 12 items").
    Done {
        req: u64,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        note: String,
    },
    /// Host: the request failed, with a reason fit for the user.
    Failed { req: u64, message: String },
}

impl BrowseMsg {
    pub fn req(&self) -> u64 {
        match self {
            Self::List { req, .. }
            | Self::Get { req, .. }
            | Self::Mkdir { req, .. }
            | Self::Rename { req, .. }
            | Self::Delete { req, .. }
            | Self::Extract { req, .. }
            | Self::Compress { req, .. }
            | Self::Listing { req, .. }
            | Self::Progress { req, .. }
            | Self::Done { req, .. }
            | Self::Failed { req, .. } => *req,
        }
    }

    /// True for what a viewer asks (as opposed to a host's answer).
    pub fn is_request(&self) -> bool {
        matches!(
            self,
            Self::List { .. }
                | Self::Get { .. }
                | Self::Mkdir { .. }
                | Self::Rename { .. }
                | Self::Delete { .. }
                | Self::Extract { .. }
                | Self::Compress { .. }
        )
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Folders first, then by name without regard to case (as Explorer shows them).
pub fn sort(entries: &mut [Entry]) {
    sort_by(entries, SortKey::Name, true);
}

/// Column a pane is sorted by (a click on its header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Date,
}

impl SortKey {
    /// From the header index the UI uses (0 name, 1 size, 2 date).
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Size,
            2 => Self::Date,
            _ => Self::Name,
        }
    }

    pub fn index(self) -> i32 {
        self as i32
    }
}

/// Folders always first (as Finder and Explorer keep them), then by `key` with ties by name; `ascending` false
/// reverses only the key order.
pub fn sort_by(entries: &mut [Entry], key: SortKey, ascending: bool) {
    let by_name = |a: &Entry, b: &Entry| a.name.to_lowercase().cmp(&b.name.to_lowercase());
    entries.sort_by(|a, b| {
        let order = match key {
            SortKey::Name => by_name(a, b),
            SortKey::Size => a.size.cmp(&b.size).then_with(|| by_name(a, b)),
            SortKey::Date => a.mtime.cmp(&b.mtime).then_with(|| by_name(a, b)),
        };
        b.dir.cmp(&a.dir).then(if ascending { order } else { order.reverse() })
    });
}

/// A new name for a folder or file: one path component, nothing that escapes the folder or names a device
/// ("CON", "nul.txt"), a stream ("a:b"), or a name Windows would silently alias ("name.").
pub fn valid_folder_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n.len() <= 200
        && n != "."
        && n != ".."
        && !n.chars().any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control())
        && safe_rel_path(n).is_some_and(|c| c.len() == 1)
}

/// A folder path the other side sent (to list, create in, fetch from): absolute, without `..`, NUL, an NT
/// device or verbatim prefix (`\\.\`, `\\?\`, `\??\`) that bypasses the normal path rules, or (Windows) a stream.
///
/// SECURITY: listings name full paths, so a host takes full paths back; this keeps them to ordinary ones. What
/// the signed-in user may touch is still decided by the OS (`infra::user_scope`).
pub fn valid_host_dir(path: &str) -> bool {
    let p = Path::new(path);
    let device = [r"\\.\", r"\\?\", r"\??\", "//./", "//?/"].iter().any(|pre| path.starts_with(pre));
    !path.is_empty()
        && path.len() <= 32_000
        && !path.contains('\0')
        && !device
        && p.is_absolute()
        && !p.components().any(|c| c == Component::ParentDir)
        && !path.split(['/', '\\']).any(|c| c == "..")
        && !(cfg!(windows) && has_stream(path))
}

/// A file or folder the other side wants changed (renamed, deleted, unpacked, packed): a [`valid_host_dir`]
/// that is not a drive or file system root.
pub fn valid_host_item(path: &str) -> bool {
    let p = Path::new(path);
    valid_host_dir(path) && p.parent().is_some() && p.file_name().is_some()
}

/// A `:` past the drive letter names an NTFS alternate data stream (`C:\a.txt:hidden`).
fn has_stream(path: &str) -> bool {
    let rest = match path.as_bytes() {
        [d, b':', ..] if d.is_ascii_alphabetic() => &path[2..],
        _ => path,
    };
    rest.contains(':')
}

/// How a click (or arrow key) changes a pane's selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// A plain click: only this row.
    Only,
    /// Ctrl/Cmd+click: this row in or out, the others kept.
    Toggle,
    /// Shift+click: every row from the anchor (the last plain or Ctrl click) to this one.
    Range,
}

impl Pick {
    /// From the UI's click mode (0 plain, 1 Ctrl/Cmd, 2 Shift).
    pub fn from_mode(mode: i32) -> Self {
        match mode {
            1 => Self::Toggle,
            2 => Self::Range,
            _ => Self::Only,
        }
    }
}

/// The selected rows of a pane, with Explorer's anchor (where a Shift range starts) and cursor (the focused row
/// the arrow keys move). Indices are rows as shown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    picked: Vec<bool>,
    anchor: Option<usize>,
    cursor: Option<usize>,
}

impl Selection {
    pub fn new(len: usize) -> Self {
        Self { picked: vec![false; len], anchor: None, cursor: None }
    }

    /// `len` rows with the rows `picked` selected and `cursor` focused (after re-sorting, for example).
    pub fn with(len: usize, picked: &[usize], cursor: Option<usize>) -> Self {
        let mut s = Self::new(len);
        for &i in picked.iter().filter(|i| **i < len) {
            s.picked[i] = true;
        }
        s.cursor = cursor.filter(|c| *c < len);
        s.anchor = s.cursor;
        s
    }

    pub fn len(&self) -> usize {
        self.picked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    pub fn is_picked(&self, i: usize) -> bool {
        self.picked.get(i).copied().unwrap_or(false)
    }

    pub fn count(&self) -> usize {
        self.picked.iter().filter(|p| **p).count()
    }

    pub fn indices(&self) -> Vec<usize> {
        (0..self.len()).filter(|i| self.picked[*i]).collect()
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn pick(&mut self, i: usize, how: Pick) {
        if i >= self.len() {
            return;
        }
        match how {
            Pick::Only => self.only(i),
            Pick::Toggle => {
                self.picked[i] = !self.picked[i];
                self.anchor = Some(i);
            }
            Pick::Range => {
                let anchor = *self.anchor.get_or_insert(i);
                self.span(anchor, i);
            }
        }
        self.cursor = Some(i);
    }

    pub fn all(&mut self) {
        self.picked.iter_mut().for_each(|p| *p = true);
    }

    pub fn clear(&mut self) {
        self.picked.iter_mut().for_each(|p| *p = false);
    }

    /// Arrow keys: moves the cursor `delta` rows (clamped); with `extend` (Shift) the selection spans from the
    /// anchor to the new cursor, otherwise only the new cursor row is selected. Without a cursor the first press
    /// lands on the first (down) or last (up) row.
    pub fn step(&mut self, delta: isize, extend: bool) {
        let Some(last) = self.len().checked_sub(1) else { return };
        let next = match self.cursor {
            Some(c) => c.saturating_add_signed(delta).min(last),
            None if delta >= 0 => 0,
            None => last,
        };
        if extend {
            let anchor = *self.anchor.get_or_insert(self.cursor.unwrap_or(next));
            self.span(anchor, next);
        } else {
            self.only(next);
        }
        self.cursor = Some(next);
    }

    fn only(&mut self, i: usize) {
        self.clear();
        self.picked[i] = true;
        self.anchor = Some(i);
    }

    fn span(&mut self, a: usize, b: usize) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        for (j, p) in self.picked.iter_mut().enumerate() {
            *p = (lo..=hi).contains(&j);
        }
    }
}

/// "1.4 MB" style size for the browser's size column.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_with_their_op_names() {
        let list = BrowseMsg::List { req: 7, path: "C:\\Users".into() };
        let json: serde_json::Value = serde_json::from_slice(&list.encode()).unwrap();
        assert_eq!(json["op"], "list");
        assert_eq!(BrowseMsg::decode(&list.encode()), Some(list.clone()));
        assert!(list.is_request());
        let listing =
            BrowseMsg::Listing { req: 7, path: "/".into(), parent: None, entries: vec![], more: false, ops: vec![] };
        assert_eq!(BrowseMsg::decode(&listing.encode()).map(|m| m.req()), Some(7));
        assert!(!listing.is_request());
        assert_eq!(BrowseMsg::decode(b"{\"op\":\"format-disk\",\"req\":1}"), None, "unknown operations are ignored");
    }

    #[test]
    fn explorer_ops_round_trip_and_stay_compatible() {
        for msg in [
            BrowseMsg::Rename { req: 1, path: "/a/b".into(), name: "c".into() },
            BrowseMsg::Delete { req: 2, paths: vec!["/a/b".into(), "/a/c".into()] },
            BrowseMsg::Extract { req: 3, path: "/a/x.zip".into(), folder: true },
            BrowseMsg::Compress { req: 4, paths: vec!["/a/b".into()] },
            BrowseMsg::Progress { req: 5, done: 10, total: 20 },
            BrowseMsg::Done { req: 6, note: "Extracted 3 items".into() },
        ] {
            assert_eq!(BrowseMsg::decode(&msg.encode()), Some(msg.clone()));
        }
        assert!(BrowseMsg::Delete { req: 1, paths: vec![] }.is_request());
        assert!(!BrowseMsg::Progress { req: 1, done: 0, total: 0 }.is_request());
        // What an older host sends still decodes (no `ops`, no `note`) ...
        let old = br#"{"op":"listing","req":9,"path":"/","parent":null,"entries":[],"more":false}"#;
        assert!(matches!(BrowseMsg::decode(old), Some(BrowseMsg::Listing { ops, .. }) if ops.is_empty()));
        assert_eq!(BrowseMsg::decode(br#"{"op":"done","req":4}"#), Some(BrowseMsg::Done { req: 4, note: "".into() }));
        // ... and an older viewer still reads a new done (unknown fields are skipped), which stays as it was.
        assert_eq!(BrowseMsg::Done { req: 4, note: String::new() }.encode(), br#"{"op":"done","req":4}"#);
        let listing = BrowseMsg::Listing {
            req: 1,
            path: "/".into(),
            parent: None,
            entries: vec![],
            more: false,
            ops: HOST_OPS.iter().map(|s| s.to_string()).collect(),
        };
        let json: serde_json::Value = serde_json::from_slice(&listing.encode()).unwrap();
        assert_eq!(json["ops"][1], "delete");
        // Extract without `folder` means "here".
        assert_eq!(
            BrowseMsg::decode(br#"{"op":"extract","req":1,"path":"/a.zip"}"#),
            Some(BrowseMsg::Extract { req: 1, path: "/a.zip".into(), folder: false })
        );
    }

    #[test]
    fn columns_sort_with_folders_first() {
        let e = |name: &str, dir: bool, size: u64, mtime: u64| Entry {
            name: name.into(),
            dir,
            size,
            mtime,
            ..Entry::default()
        };
        let mut list = vec![e("b", false, 5, 1), e("a", false, 9, 3), e("Z", true, 0, 2), e("c", false, 1, 2)];
        sort_by(&mut list, SortKey::Size, false);
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Z", "a", "b", "c"]);
        sort_by(&mut list, SortKey::Date, true);
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Z", "b", "c", "a"]);
        assert_eq!(SortKey::from_index(SortKey::Date.index()), SortKey::Date);
    }

    #[test]
    fn clicks_select_like_explorer() {
        let mut s = Selection::new(6);
        s.pick(1, Pick::Only);
        assert_eq!(s.indices(), [1]);
        s.pick(4, Pick::Range);
        assert_eq!(s.indices(), [1, 2, 3, 4]);
        // Shift again re-spans from the same anchor (shrinks), it does not add.
        s.pick(2, Pick::Range);
        assert_eq!(s.indices(), [1, 2]);
        s.pick(5, Pick::Toggle);
        assert_eq!(s.indices(), [1, 2, 5]);
        s.pick(1, Pick::Toggle);
        assert_eq!(s.indices(), [2, 5]);
        // The toggle moved the anchor: Shift now spans from row 1.
        s.pick(3, Pick::Range);
        assert_eq!(s.indices(), [1, 2, 3]);
        s.pick(0, Pick::Only);
        assert_eq!((s.indices(), s.cursor()), (vec![0], Some(0)));
        s.pick(9, Pick::Only);
        assert_eq!(s.indices(), [0], "rows past the end are ignored");
        s.all();
        assert_eq!(s.count(), 6);
        s.clear();
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn a_shift_click_without_an_anchor_selects_one_row() {
        let mut s = Selection::new(3);
        s.pick(2, Pick::Range);
        assert_eq!(s.indices(), [2]);
    }

    #[test]
    fn arrow_keys_move_and_extend() {
        let mut s = Selection::new(5);
        s.step(1, false);
        assert_eq!((s.indices(), s.cursor()), (vec![0], Some(0)));
        s.step(1, true);
        s.step(1, true);
        assert_eq!(s.indices(), [0, 1, 2]);
        s.step(-1, true);
        assert_eq!(s.indices(), [0, 1]);
        s.step(10, false);
        assert_eq!((s.indices(), s.cursor()), (vec![4], Some(4)));
        s.step(-10, true);
        assert_eq!(s.count(), 5);
        let mut up = Selection::new(3);
        up.step(-1, false);
        assert_eq!(up.cursor(), Some(2));
        let mut none = Selection::new(0);
        none.step(1, true);
        assert_eq!(none.cursor(), None);
        let kept = Selection::with(4, &[3, 1, 7], Some(3));
        assert_eq!((kept.indices(), kept.cursor()), (vec![1, 3], Some(3)));
    }

    #[test]
    fn host_paths_stay_ordinary() {
        let good: &[&str] = if cfg!(windows) {
            &[r"C:\Users\me\a.txt", r"D:\", r"\\server\share\x"]
        } else {
            &["/home/me/a.txt", "/", "/tmp/a:b"]
        };
        for p in good {
            assert!(valid_host_dir(p), "{p}");
        }
        let bad: &[&str] = if cfg!(windows) {
            &[
                "",
                "relative\\x",
                r"C:\a\..\b",
                r"\\.\PhysicalDrive0",
                r"\\?\C:\x",
                r"\??\C:\x",
                r"C:\a.txt:hidden",
                "C:\\a\0b",
                "C:x",
            ]
        } else {
            &["", "relative/x", "/a/../b", "/a/b/..", "//./x", "/a\0b"]
        };
        for p in bad {
            assert!(!valid_host_dir(p), "{p:?}");
        }
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let item = if cfg!(windows) { r"C:\Users" } else { "/home" };
        assert!(valid_host_dir(root) && !valid_host_item(root), "a root cannot be renamed or deleted");
        assert!(valid_host_item(item));
    }

    #[test]
    fn folders_come_first_then_names_without_case() {
        let e = |name: &str, dir: bool| Entry { name: name.into(), dir, ..Entry::default() };
        let mut list = vec![e("b.txt", false), e("Zeta", true), e("A.txt", false), e("alpha", true)];
        sort(&mut list);
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Zeta", "A.txt", "b.txt"]);
    }

    #[test]
    fn folder_names_stay_inside_their_folder() {
        assert!(valid_folder_name("New folder"));
        assert!(valid_folder_name("report.pdf"));
        for bad in ["", " ", ".", "..", "a/b", "a\\b", "C:", "x?", "con\u{0}", "CON", "nul.txt", "name."] {
            assert!(!valid_folder_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn sizes_read_like_explorer() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
