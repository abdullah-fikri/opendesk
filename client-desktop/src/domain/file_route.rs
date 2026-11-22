//! Which path each outgoing file transfer takes: the relay, the P2P path (`file-p2p`), the direct file channel
//! (`file-channel`) or the relay file lane (`file-lane`). A transfer keeps the path its `start` took, and every entry of a folder the path of its first
//! entry, so no message of a transfer can overtake an earlier one on another path.

use std::collections::HashMap;

/// Kind of file message being routed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Start,
    Chunk,
    End {
        group_end: bool,
    },
    /// Offers, pulls, acknowledgements: the relay (the relay connection sends the ack of a P2P transfer back
    /// over the P2P path itself).
    Other,
}

/// Where a transfer's messages go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Path {
    #[default]
    Relay,
    /// The session's P2P path (`file-p2p`).
    P2p,
    /// The direct file channel (`file-channel`).
    Channel,
    /// Our relay file lane (`file-lane`): still through the relay, but on a socket of its own.
    Lane,
}

impl Path {
    /// The path for a new transfer, given which are up toward its target: a direct path first (the file channel,
    /// then the P2P path), else the relay file lane, else the relay socket.
    pub fn choose(channel: bool, p2p: bool, lane: bool) -> Self {
        if channel {
            Self::Channel
        } else if p2p {
            Self::P2p
        } else if lane {
            Self::Lane
        } else {
            Self::Relay
        }
    }
}

/// Routes of the transfers in progress, keyed `target:id` (and `target:group` for folders).
#[derive(Default)]
pub struct FileRoutes(HashMap<String, Path>);

impl FileRoutes {
    /// The path of the message of transfer `id` (in folder `group`) to `target`; `path_now` tells which path a
    /// transfer starting now would take.
    pub fn route(
        &mut self,
        target: &str,
        id: &str,
        group: Option<&str>,
        step: Step,
        path_now: impl FnOnce() -> Path,
    ) -> Path {
        let key = format!("{target}:{id}");
        match step {
            Step::Start => {
                let path = match group {
                    Some(g) => *self.0.entry(format!("{target}:{g}")).or_insert_with(path_now),
                    None => path_now(),
                };
                self.0.insert(key, path);
                path
            }
            Step::Chunk => self.routed(target, id),
            Step::End { group_end } => {
                let path = self.0.remove(&key).unwrap_or_default();
                if group_end && let Some(g) = group {
                    self.0.remove(&format!("{target}:{g}"));
                }
                path
            }
            Step::Other => Path::Relay,
        }
    }

    /// The path transfer `id` to `target` runs on.
    pub fn routed(&self, target: &str, id: &str) -> Path {
        self.0.get(&format!("{target}:{id}")).copied().unwrap_or_default()
    }

    /// Forgets the transfers to `target` on `path` (it dropped); true if there were any.
    pub fn drop_path(&mut self, target: &str, path: Path) -> bool {
        let prefix = format!("{target}:");
        let before = self.0.len();
        self.0.retain(|k, p| !(k.starts_with(&prefix) && *p == path));
        self.0.len() != before
    }

    /// Forgets the transfers to every target on `path` (it dropped); true if there were any.
    pub fn drop_everywhere(&mut self, path: Path) -> bool {
        let before = self.0.len();
        self.0.retain(|_, p| *p != path);
        self.0.len() != before
    }

    /// True while some transfer runs on `path`.
    pub fn uses(&self, path: Path) -> bool {
        self.0.values().any(|p| *p == path)
    }
}

#[cfg(test)]
mod tests {
    use super::Path::{Channel, Lane, P2p, Relay};
    use super::*;

    #[test]
    fn a_transfer_keeps_the_path_it_started_on() {
        let mut r = FileRoutes::default();
        assert_eq!(r.route("1", "a", None, Step::Start, || Relay), Relay);
        // A faster path came up meanwhile: the running transfer stays where it is, a new one takes it.
        assert_eq!(r.route("1", "a", None, Step::Chunk, || Channel), Relay);
        assert_eq!(r.route("1", "b", None, Step::Start, || Channel), Channel);
        assert_eq!(r.routed("1", "b"), Channel);
        assert_eq!(r.route("1", "a", None, Step::End { group_end: false }, || P2p), Relay);
        assert_eq!(r.routed("1", "a"), Relay, "forgotten after its end");
    }

    #[test]
    fn folder_entries_follow_the_first_one() {
        let mut r = FileRoutes::default();
        assert_eq!(r.route("1", "e1", Some("g"), Step::Start, || P2p), P2p);
        r.route("1", "e1", Some("g"), Step::End { group_end: false }, || Relay);
        assert_eq!(r.route("1", "e2", Some("g"), Step::Start, || Channel), P2p, "the folder stays on its path");
        assert_eq!(r.route("1", "e2", Some("g"), Step::End { group_end: true }, || Relay), P2p);
        assert_eq!(r.route("1", "e3", Some("g"), Step::Start, || Relay), Relay, "a new folder decides again");
    }

    #[test]
    fn dropping_a_path_forgets_only_its_transfers() {
        let mut r = FileRoutes::default();
        r.route("1", "a", None, Step::Start, || P2p);
        r.route("1", "b", None, Step::Start, || Relay);
        r.route("1", "c", None, Step::Start, || Channel);
        r.route("2", "d", None, Step::Start, || P2p);
        assert!(r.drop_path("1", P2p));
        assert_eq!((r.routed("1", "a"), r.routed("1", "c"), r.routed("2", "d")), (Relay, Channel, P2p));
        assert!(!r.drop_path("1", P2p));
        assert!(r.drop_path("1", Channel));
        assert_eq!(r.route("1", "x", None, Step::Other, || Channel), Relay);
    }

    #[test]
    fn the_lane_comes_after_direct_paths_and_before_the_relay_socket() {
        assert_eq!(Path::choose(true, true, true), Channel);
        assert_eq!(Path::choose(false, true, true), P2p);
        assert_eq!(Path::choose(false, false, true), Lane);
        assert_eq!(Path::choose(false, false, false), Relay);
        assert_eq!(Path::choose(true, false, false), Channel);
    }

    #[test]
    fn a_lost_lane_forgets_its_transfers_to_every_target() {
        let mut r = FileRoutes::default();
        r.route("1", "a", None, Step::Start, || Lane);
        r.route("2", "b", Some("g"), Step::Start, || Lane);
        r.route("2", "c", None, Step::Start, || Relay);
        assert!(r.uses(Lane));
        assert!(r.drop_everywhere(Lane));
        assert!(!r.uses(Lane) && !r.drop_everywhere(Lane));
        assert_eq!((r.routed("1", "a"), r.routed("2", "b"), r.routed("2", "c")), (Relay, Relay, Relay));
        // The folder's next entry decides again instead of following a lane that is gone.
        assert_eq!(r.route("2", "d", Some("g"), Step::Start, || Relay), Relay);
    }
}
