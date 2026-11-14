//! Provenance of files this machine received, so copy-paste never sends them back to the partner they came
//! from: not the received items themselves, not anything inside a received folder, and not a copy of one moved
//! elsewhere (same name, and for files the same size). Entries expire, so a file edited or copied much later is
//! sent normally. Pure bookkeeping: callers supply paths, sizes and the time.

use std::time::{Duration, Instant};

/// How long a received item counts as the partner's own.
pub const RECEIVED_TTL: Duration = Duration::from_secs(600);
/// Most received items remembered; the oldest are forgotten first.
const MAX_ITEMS: usize = 256;

/// Why a copied path counts as one the partner sent us.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Echo {
    /// It is a received item.
    Same,
    /// It lies inside a received folder.
    Inside,
    /// Same name (and, for a file, size) as a received item: a copy of it pasted elsewhere.
    Copy,
}

impl Echo {
    /// Short reason for logs.
    pub fn reason(self) -> &'static str {
        match self {
            Echo::Same => "received from this partner",
            Echo::Inside => "inside a folder received from this partner",
            Echo::Copy => "copy of an item received from this partner",
        }
    }
}

struct Item {
    partner: String,
    /// Normalized (see [`normalize`]).
    path: String,
    /// `None` for a folder.
    size: Option<u64>,
    at: Instant,
}

/// Items received recently, per partner.
pub struct ReceivedFiles {
    items: Vec<Item>,
}

impl Default for ReceivedFiles {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceivedFiles {
    /// Nothing received yet.
    pub const fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Remembers `path` (a file of `size` bytes, or a folder for `None`) as received from `partner` at `now`.
    pub fn record(&mut self, partner: &str, path: &str, size: Option<u64>, now: Instant) {
        let path = normalize(path);
        self.items.retain(|i| now.duration_since(i.at) < RECEIVED_TTL && !(i.partner == partner && i.path == path));
        if self.items.len() >= MAX_ITEMS {
            self.items.remove(0);
        }
        self.items.push(Item { partner: partner.to_string(), path, size, at: now });
    }

    /// Why `path` (a file of `size` bytes, or a folder for `None`), copied for `partner` at `now`, is an item
    /// that partner sent us; `None` when it may be sent.
    pub fn echo(&self, partner: &str, path: &str, size: Option<u64>, now: Instant) -> Option<Echo> {
        let path = normalize(path);
        let live = self.items.iter().filter(|i| i.partner == partner && now.duration_since(i.at) < RECEIVED_TTL);
        let mut found = None;
        for item in live {
            let echo = if item.path == path {
                Echo::Same
            } else if item.size.is_none() && path.strip_prefix(&item.path).is_some_and(|rest| rest.starts_with('\\'))
            {
                Echo::Inside
            } else if name_of(&item.path) == name_of(&path) && item.size == size {
                Echo::Copy
            } else {
                continue;
            };
            // The most specific reason wins.
            if echo == Echo::Same {
                return Some(echo);
            }
            if found.is_none() || echo == Echo::Inside {
                found = Some(echo);
            }
        }
        found
    }
}

/// Case-insensitive Windows form: backslashes, no `\\?\` prefix, no trailing separator.
fn normalize(path: &str) -> String {
    let path = path.replace('/', "\\");
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    path.trim_end_matches('\\').to_lowercase()
}

fn name_of(normalized: &str) -> &str {
    normalized.rsplit('\\').next().unwrap_or(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "185305044";
    const FOLDER: &str = r"C:\Users\a\Downloads\OpenDesk\17140161581180";

    /// The 02:29:48 trace: the viewer received a folder from the host, the folder came back to its clipboard
    /// and Ctrl+V on the host sent all 86 MB back. Every way that folder can reappear is an echo for that host.
    #[test]
    fn a_received_folder_is_never_sent_back_to_its_sender() {
        let t0 = Instant::now();
        let mut received = ReceivedFiles::new();
        received.record(HOST, FOLDER, None, t0);
        let soon = t0 + Duration::from_secs(40);
        assert_eq!(received.echo(HOST, FOLDER, None, soon), Some(Echo::Same));
        assert_eq!(received.echo(HOST, &FOLDER.to_uppercase(), None, soon), Some(Echo::Same));
        assert_eq!(received.echo(HOST, &format!(r"\\?\{FOLDER}\"), None, soon), Some(Echo::Same));
        let inner = format!(r"{FOLDER}\Windows\driver.exe");
        assert_eq!(received.echo(HOST, &inner, Some(46_011_736), soon), Some(Echo::Inside));
        // Pasted onto the desktop first, then copied from there.
        let moved = r"C:\Users\a\Desktop\17140161581180";
        assert_eq!(received.echo(HOST, moved, None, soon), Some(Echo::Copy));
        // Another host may get it, and so may this one once it expired.
        assert_eq!(received.echo("999999999", FOLDER, None, soon), None);
        assert_eq!(received.echo(HOST, FOLDER, None, t0 + RECEIVED_TTL), None);
    }

    #[test]
    fn other_files_are_not_echoes() {
        let t0 = Instant::now();
        let mut received = ReceivedFiles::new();
        received.record(HOST, r"C:\d\OpenDesk\report.pdf", Some(85_808), t0);
        received.record(HOST, FOLDER, None, t0);
        // A file is not a folder: nothing lies "inside" it.
        assert_eq!(received.echo(HOST, r"C:\d\OpenDesk\report.pdf\x", Some(1), t0), None);
        // Same name, other size: an edited file, sent normally.
        assert_eq!(received.echo(HOST, r"C:\work\report.pdf", Some(90_000), t0), None);
        assert_eq!(received.echo(HOST, r"C:\work\report.pdf", Some(85_808), t0), Some(Echo::Copy));
        // A sibling whose name only starts like the folder is not inside it.
        assert_eq!(received.echo(HOST, &format!("{FOLDER}-old"), None, t0), None);
        assert_eq!(received.echo(HOST, r"C:\other.txt", Some(3), t0), None);
    }

    #[test]
    fn the_list_is_bounded_and_keeps_the_newest() {
        let t0 = Instant::now();
        let mut received = ReceivedFiles::new();
        for i in 0..MAX_ITEMS + 10 {
            received.record(HOST, &format!(r"C:\d\f{i}.bin"), Some(i as u64), t0);
        }
        assert_eq!(received.items.len(), MAX_ITEMS);
        assert_eq!(received.echo(HOST, r"C:\d\f0.bin", Some(0), t0), None);
        let last = MAX_ITEMS + 9;
        assert_eq!(received.echo(HOST, &format!(r"C:\d\f{last}.bin"), Some(last as u64), t0), Some(Echo::Same));
    }
}
