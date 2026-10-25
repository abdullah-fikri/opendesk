//! File-transfer limits and path rules shared by sending, receiving and clipboard offers.

/// Largest single file.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Most entries (files and folders) in one folder transfer or offer.
pub const MAX_TREE_ENTRIES: usize = 10_000;
/// Most bytes in one folder transfer or offer.
pub const MAX_TREE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// FILEDESCRIPTORW.cFileName holds 260 UTF-16 units including the terminator.
const MAX_REL_UTF16: usize = 259;
/// Longest transfer, group or offer ID accepted from the wire.
const MAX_ID_LEN: usize = 80;

/// True for IDs made of ASCII letters, digits and `_`, at most 80 characters.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// Validates a relative path that came from the partner and returns its components.
///
/// SECURITY: this is the only gate between a remote peer and the local file system. It rejects
/// absolute/UNC paths, drive letters and streams (`:`), `.`/`..`, empty components, reserved device names,
/// characters Windows cannot store, trailing dots/spaces (Windows would silently alias them) and over-long
/// paths. Both `\` and `/` separate components.
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

/// Replaces characters Windows cannot store in a file name with `_`.
pub fn sanitize_file_name(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

/// Progress percentage, capped at 99 until the transfer is confirmed.
pub fn percent(done: u64, total: u64) -> u64 {
    (done * 100).checked_div(total).map_or(99, |p| p.min(99))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_rel_path_accepts_relative_trees_only() {
        assert_eq!(safe_rel_path(r"Project\src\main.rs"), Some(vec!["Project", "src", "main.rs"]));
        assert_eq!(safe_rel_path("a/b.txt"), Some(vec!["a", "b.txt"]));
        for ok in ["COM10", "console.txt", "a.b.c", "Läporan 2024", "nulled.txt"] {
            assert!(safe_rel_path(ok).is_some(), "{ok}");
        }
        let long_comp = "x".repeat(256);
        let long_path = format!("{}\\{}", "a".repeat(200), "b".repeat(59)); // 260 units
        for bad in [
            "",
            r"\abs",
            "/abs",
            r"\\server\share",
            r"C:\x",
            "C:x",
            "a:stream",
            r"a\..\b",
            "..",
            ".",
            r"a\\b",
            r"a\",
            r"a\.\b",
            "CON",
            "nul.txt",
            r"x\com1",
            "LPT9.log",
            "AUX .txt",
            "COM¹",
            "a\u{1}b",
            "a<b",
            "a|b",
            "a?",
            "a*",
            "a\"b",
            "trail.",
            "trail ",
            &long_comp,
            &long_path,
        ] {
            assert!(safe_rel_path(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn ids_names_and_progress() {
        assert!(is_safe_id("ft_123") && !is_safe_id("../g") && !is_safe_id("") && !is_safe_id(&"a".repeat(81)));
        assert_eq!(sanitize_file_name(r"a\b:c?.txt"), "a_b_c_.txt");
        assert_eq!((percent(50, 100), percent(100, 100), percent(0, 0)), (50, 99, 99));
    }
}
