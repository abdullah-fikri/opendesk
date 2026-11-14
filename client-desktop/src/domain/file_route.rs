//! Which path each outgoing file transfer takes (`file-p2p`): the relay or the P2P path. A transfer keeps the
//! path its `start` took, and every entry of a folder the path of its first entry, so no message of a transfer
//! can overtake an earlier one on the other path.

use std::collections::HashMap;

/// Kind of file message being routed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Start,
    Chunk,
    End {
        group_end: bool,
    },
    /// Offers, pulls, acknowledgements: always the relay.
    Other,
}

/// Routes of the transfers in progress, keyed `target:id` (and `target:group` for folders); true = P2P.
#[derive(Default)]
pub struct FileRoutes(HashMap<String, bool>);

impl FileRoutes {
    /// Whether the message of transfer `id` (in folder `group`) to `target` goes over the P2P path; `p2p_now`
    /// tells whether a transfer starting now would.
    pub fn route(
        &mut self,
        target: &str,
        id: &str,
        group: Option<&str>,
        step: Step,
        p2p_now: impl FnOnce() -> bool,
    ) -> bool {
        let key = format!("{target}:{id}");
        match step {
            Step::Start => {
                let p2p = match group {
                    Some(g) => *self.0.entry(format!("{target}:{g}")).or_insert_with(p2p_now),
                    None => p2p_now(),
                };
                self.0.insert(key, p2p);
                p2p
            }
            Step::Chunk => self.routed(target, id),
            Step::End { group_end } => {
                let p2p = self.0.remove(&key).unwrap_or(false);
                if group_end && let Some(g) = group {
                    self.0.remove(&format!("{target}:{g}"));
                }
                p2p
            }
            Step::Other => false,
        }
    }

    /// Whether transfer `id` to `target` runs over the P2P path.
    pub fn routed(&self, target: &str, id: &str) -> bool {
        self.0.get(&format!("{target}:{id}")).copied().unwrap_or(false)
    }

    /// Forgets the P2P transfers to `target` (its path dropped); true if there were any.
    pub fn drop_p2p(&mut self, target: &str) -> bool {
        let prefix = format!("{target}:");
        let before = self.0.len();
        self.0.retain(|k, p2p| !(k.starts_with(&prefix) && *p2p));
        self.0.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transfer_keeps_the_path_it_started_on() {
        let mut r = FileRoutes::default();
        assert!(!r.route("1", "a", None, Step::Start, || false));
        // The P2P path came up meanwhile: the running transfer stays on the relay, a new one takes P2P.
        assert!(!r.route("1", "a", None, Step::Chunk, || true));
        assert!(r.route("1", "b", None, Step::Start, || true));
        assert!(r.routed("1", "b"));
        assert!(!r.route("1", "a", None, Step::End { group_end: false }, || true));
        assert!(!r.routed("1", "a"), "forgotten after its end");
    }

    #[test]
    fn folder_entries_follow_the_first_one() {
        let mut r = FileRoutes::default();
        assert!(r.route("1", "e1", Some("g"), Step::Start, || true));
        r.route("1", "e1", Some("g"), Step::End { group_end: false }, || false);
        assert!(r.route("1", "e2", Some("g"), Step::Start, || false), "path went down: the folder stays on P2P");
        assert!(r.route("1", "e2", Some("g"), Step::End { group_end: true }, || false));
        assert!(!r.route("1", "e3", Some("g"), Step::Start, || false), "a new folder decides again");
    }

    #[test]
    fn dropping_a_path_forgets_only_its_p2p_transfers() {
        let mut r = FileRoutes::default();
        r.route("1", "a", None, Step::Start, || true);
        r.route("1", "b", None, Step::Start, || false);
        r.route("2", "c", None, Step::Start, || true);
        assert!(r.drop_p2p("1"));
        assert!(!r.routed("1", "a") && r.routed("2", "c"));
        assert!(!r.drop_p2p("1"));
        assert!(!r.route("1", "x", None, Step::Other, || true));
    }
}
