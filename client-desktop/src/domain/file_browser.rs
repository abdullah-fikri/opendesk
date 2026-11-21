//! File browser messages (two-pane browser of a session's host, like AnyDesk's file manager): the viewer lists the
//! host's folders and asks for files; uploads and downloads then run as ordinary transfers (`start` carrying `dest`
//! or `browse`, see `service::file_browser`). The messages travel only over a direct path (the file channel, else
//! the P2P path) as file stream messages of kind [`MSG_BROWSE`]: the relay never sees or forwards them.
//!
//! Paths are always built by the side that owns them: a listing names its own parent and every entry's full path,
//! so a Windows viewer browsing a Linux host never joins paths with the wrong separator.

use serde::{Deserialize, Serialize};

/// File stream message kind (next to `domain::bulk::MSG_JSON` and `MSG_BINARY`).
pub const MSG_BROWSE: u8 = 3;
/// Most entries in one listing; a bigger folder says so with `more`.
pub const MAX_ENTRIES: usize = 5000;

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
    /// Host: the entries of `path`; `parent` is "" at the top level.
    Listing { req: u64, path: String, parent: Option<String>, entries: Vec<Entry>, more: bool },
    /// Host: the request was carried out.
    Done { req: u64 },
    /// Host: the request failed, with a reason fit for the user.
    Failed { req: u64, message: String },
}

impl BrowseMsg {
    pub fn req(&self) -> u64 {
        match self {
            Self::List { req, .. }
            | Self::Get { req, .. }
            | Self::Mkdir { req, .. }
            | Self::Listing { req, .. }
            | Self::Done { req }
            | Self::Failed { req, .. } => *req,
        }
    }

    /// True for what a viewer asks (as opposed to a host's answer).
    pub fn is_request(&self) -> bool {
        matches!(self, Self::List { .. } | Self::Get { .. } | Self::Mkdir { .. })
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
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

/// A new folder's name: one path component, nothing that escapes the folder or names a device.
pub fn valid_folder_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n.len() <= 200
        && n != "."
        && n != ".."
        && !n.chars().any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control())
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
        let listing = BrowseMsg::Listing { req: 7, path: "/".into(), parent: None, entries: vec![], more: false };
        assert_eq!(BrowseMsg::decode(&listing.encode()).map(|m| m.req()), Some(7));
        assert!(!listing.is_request());
        assert_eq!(BrowseMsg::decode(b"{\"op\":\"format-disk\",\"req\":1}"), None, "unknown operations are ignored");
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
        for bad in ["", " ", ".", "..", "a/b", "a\\b", "C:", "x?", "con\u{0}"] {
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
