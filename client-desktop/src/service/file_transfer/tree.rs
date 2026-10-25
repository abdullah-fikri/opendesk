//! Folder walking for outgoing selections and collision-free creation of received files and folders.

use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::transfer::{MAX_FILE_BYTES, safe_rel_path};
use crate::infra::logger;

/// One entry of a walked selection. `rel` uses `\` separators; folders have `dir` set and size 0.
#[derive(Debug)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub rel: String,
    pub size: u64,
    pub dir: bool,
}

/// Expands the selected paths into files and folders (folders before their contents) without reading any
/// file contents. Symlinks and junctions inside folders are skipped (no loops, nothing outside the
/// selection). Errors are user-facing status lines.
pub fn walk(roots: &[PathBuf], max_entries: usize, max_bytes: u64) -> Result<Vec<TreeEntry>, String> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut push = |out: &mut Vec<TreeEntry>, e: TreeEntry| -> Result<(), String> {
        if safe_rel_path(&e.rel).is_none() {
            return Err(format!("Failed: unsupported name or path: {}", e.rel));
        }
        if e.size > MAX_FILE_BYTES {
            return Err(format!("Failed: {} is larger than 2 GB", e.rel));
        }
        total += e.size;
        if out.len() >= max_entries {
            return Err(format!("Failed: the folder has too many items (max {max_entries})"));
        }
        if total > max_bytes {
            return Err(format!("Failed: total size exceeds {} GB", max_bytes >> 30));
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
            let entries = match fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) => {
                    // Unreadable folder (access denied): keep it as an empty folder rather than failing everything.
                    logger::warn("file_transfer", &format!("Skipping unreadable folder {dir:?}: {e}"));
                    continue;
                }
            };
            let mut children: Vec<_> = entries.filter_map(Result::ok).collect();
            children.sort_by_key(|c| c.file_name());
            for child in children {
                let Ok(ft) = child.file_type() else { continue };
                if ft.is_symlink() {
                    continue;
                }
                let Some(child_name) = child.file_name().to_str().map(str::to_string) else {
                    return Err(format!("Failed: unsupported name in {rel}"));
                };
                let child_rel = format!("{rel}\\{child_name}");
                if ft.is_dir() {
                    push(&mut out, TreeEntry { path: child.path(), rel: child_rel.clone(), size: 0, dir: true })?;
                    stack.push((child.path(), child_rel));
                } else if ft.is_file() {
                    let size = child.metadata().map(|m| m.len()).unwrap_or(0);
                    push(&mut out, TreeEntry { path: child.path(), rel: child_rel, size, dir: false })?;
                }
            }
        }
    }
    Ok(out)
}

/// Creates `base\name`, or `name (1)`, ... if taken; `create_dir` never reuses an existing folder.
pub fn create_dir_unique(base: &Path, name: &str) -> Option<PathBuf> {
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

/// Hard-links `staging` to the first free `name`, `stem (1).ext`, ... in `dir`. `hard_link` never replaces an
/// existing file, so a received file can never overwrite one already there.
pub fn link_unique(staging: &Path, dir: &Path, name: &str) -> Option<PathBuf> {
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

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::service::file_transfer::now_ms;
    use std::env;

    /// Creates a small folder tree for tests and returns its root.
    pub fn temp_tree(tag: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("opendesk-walk-{}-{}-{}", tag, std::process::id(), now_ms()));
        fs::create_dir_all(root.join("Project/src/deep")).unwrap();
        fs::create_dir_all(root.join("Project/empty")).unwrap();
        fs::write(root.join("Project/src/main.rs"), b"fn main() {}").unwrap();
        fs::write(root.join("Project/src/deep/x.txt"), b"x").unwrap();
        fs::write(root.join("loose.txt"), b"12345").unwrap();
        root
    }

    #[test]
    fn link_unique_never_overwrites_existing_file() {
        let dir = env::temp_dir().join(format!("opendesk-test-{}", now_ms()));
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
    fn walk_lists_folders_before_contents_with_relative_names() {
        let root = temp_tree("order");
        let entries = walk(&[root.join("Project"), root.join("loose.txt")], 100, 1 << 20).unwrap();
        let rels: Vec<(&str, bool, u64)> = entries.iter().map(|e| (e.rel.as_str(), e.dir, e.size)).collect();
        assert_eq!(rels.len(), 7);
        for (rel, dir, size) in [
            ("Project", true, 0),
            (r"Project\empty", true, 0),
            (r"Project\src", true, 0),
            (r"Project\src\deep", true, 0),
            (r"Project\src\main.rs", false, 12),
            (r"Project\src\deep\x.txt", false, 1),
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
    fn walk_enforces_entry_and_size_limits() {
        let root = temp_tree("limits");
        let sel = [root.join("Project")];
        assert!(walk(&sel, 6, 1 << 20).is_ok());
        assert!(walk(&sel, 5, 1 << 20).unwrap_err().contains("max 5"));
        assert!(walk(&sel, 100, 12).unwrap_err().contains("total size"));
        let _ = fs::remove_dir_all(root);
    }
}
