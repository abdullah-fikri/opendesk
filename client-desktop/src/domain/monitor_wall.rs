//! Monitor-wall rules (admin edition, see `docs/features/monitor-wall.md` at the repository root): which hosts
//! the wall shows, how they split into pages and when the page rotates. Pure data and arithmetic.

use std::time::{Duration, Instant};

use super::settings::MonitorSettings;

/// Shortest and longest automatic page interval.
pub const ROTATE_MIN_SECS: u32 = 5;
pub const ROTATE_MAX_SECS: u32 = 600;

/// One device the wall may show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WallHost {
    /// Clean desk ID.
    pub id: String,
    pub name: String,
    pub online: bool,
}

/// Tiles per row and column: 2, 3 or 4.
pub fn grid(settings: &MonitorSettings) -> usize {
    settings.grid.clamp(2, 4) as usize
}

/// Tiles on one page (4, 9 or 16).
pub fn per_page(settings: &MonitorSettings) -> usize {
    grid(settings) * grid(settings)
}

/// Page interval, or `None` when rotation is off.
pub fn rotate_interval(settings: &MonitorSettings) -> Option<Duration> {
    settings.rotate.then(|| Duration::from_secs(settings.rotate_secs.clamp(ROTATE_MIN_SECS, ROTATE_MAX_SECS) as u64))
}

/// The hosts on the wall in a stable order (name, then ID), so pages do not reshuffle when a device comes or goes:
/// every online device in "all" mode, else the hand-picked ones, offline included so a missing machine shows.
pub fn wall_hosts(hosts: &[WallHost], settings: &MonitorSettings) -> Vec<WallHost> {
    let mut out: Vec<WallHost> = hosts
        .iter()
        .filter(|h| if settings.all { h.online } else { settings.selected.contains(&h.id) })
        .cloned()
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Number of pages for `n` hosts (at least one, even when empty).
pub fn page_count(n: usize, per_page: usize) -> usize {
    n.div_ceil(per_page.max(1)).max(1)
}

/// The hosts on `page` (clamped to the last page).
pub fn page_of<T>(list: &[T], per_page: usize, page: usize) -> &[T] {
    let per_page = per_page.max(1);
    let page = page.min(page_count(list.len(), per_page) - 1);
    let start = page * per_page;
    &list[start.min(list.len())..(start + per_page).min(list.len())]
}

/// `page` moved by `delta`, wrapping around both ends.
pub fn step_page(page: usize, delta: i32, pages: usize) -> usize {
    let pages = pages.max(1) as i64;
    (page as i64 + delta as i64).rem_euclid(pages) as usize
}

/// Adds or removes `id` from the hand-picked hosts.
pub fn toggle_selected(settings: &mut MonitorSettings, id: &str) {
    if let Some(i) = settings.selected.iter().position(|s| s == id) {
        settings.selected.remove(i);
    } else {
        settings.selected.push(id.to_string());
    }
}

/// Automatic page switching. A manual page change restarts the countdown.
#[derive(Clone, Copy, Debug)]
pub struct Rotation {
    since: Instant,
}

impl Rotation {
    pub fn new(now: Instant) -> Self {
        Self { since: now }
    }

    /// Restarts the countdown.
    pub fn reset(&mut self, now: Instant) {
        self.since = now;
    }

    /// True (and restarts the countdown) when the page should advance now.
    pub fn due(&mut self, interval: Option<Duration>, pages: usize, now: Instant) -> bool {
        match interval {
            Some(every) if pages > 1 && now.duration_since(self.since) >= every => {
                self.since = now;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str, name: &str, online: bool) -> WallHost {
        WallHost { id: id.into(), name: name.into(), online }
    }

    fn settings(all: bool, selected: &[&str]) -> MonitorSettings {
        MonitorSettings { all, selected: selected.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn selection_all_shows_online_devices_and_picked_shows_offline_ones_too() {
        let hosts = [host("3", "beta", true), host("1", "Alpha", false), host("2", "alpha", true)];
        let all: Vec<_> = wall_hosts(&hosts, &settings(true, &[])).into_iter().map(|h| h.id).collect();
        assert_eq!(all, ["2", "3"], "online only, sorted by name");
        let picked: Vec<_> = wall_hosts(&hosts, &settings(false, &["3", "1", "9"])).into_iter().map(|h| h.id).collect();
        assert_eq!(picked, ["1", "3"], "hand-picked, offline included, unknown IDs skipped, name order");
        assert!(wall_hosts(&hosts, &settings(false, &[])).is_empty());
    }

    #[test]
    fn toggling_a_host_adds_and_removes_it() {
        let mut s = settings(false, &[]);
        toggle_selected(&mut s, "123456789");
        toggle_selected(&mut s, "222222222");
        toggle_selected(&mut s, "123456789");
        assert_eq!(s.selected, ["222222222"]);
    }

    #[test]
    fn pages_split_by_grid_and_clamp() {
        let mut s = MonitorSettings::default();
        for (g, n) in [(2, 4), (3, 9), (4, 16), (1, 4), (9, 16)] {
            s.grid = g;
            assert_eq!(per_page(&s), n, "grid {g}");
        }
        let list: Vec<u32> = (0..20).collect();
        assert_eq!(page_count(20, 16), 2);
        assert_eq!(page_count(0, 16), 1);
        assert_eq!(page_count(16, 16), 1);
        assert_eq!(page_of(&list, 16, 0).len(), 16);
        assert_eq!(page_of(&list, 16, 1), &[16, 17, 18, 19]);
        assert_eq!(page_of(&list, 16, 7), &[16, 17, 18, 19], "past the end clamps to the last page");
        assert!(page_of::<u32>(&[], 9, 3).is_empty());
        assert_eq!(page_of(&list, 9, 2), &[18, 19]);
    }

    #[test]
    fn page_steps_wrap() {
        assert_eq!(step_page(0, 1, 3), 1);
        assert_eq!(step_page(2, 1, 3), 0);
        assert_eq!(step_page(0, -1, 3), 2);
        assert_eq!(step_page(5, 1, 1), 0);
        assert_eq!(step_page(0, 1, 0), 0);
    }

    #[test]
    fn rotation_advances_on_its_interval_only_with_several_pages() {
        let t0 = Instant::now();
        let mut s = MonitorSettings { rotate: true, rotate_secs: 10, ..Default::default() };
        let mut r = Rotation::new(t0);
        let every = rotate_interval(&s);
        assert!(!r.due(every, 3, t0 + Duration::from_secs(9)));
        assert!(r.due(every, 3, t0 + Duration::from_secs(10)));
        assert!(!r.due(every, 3, t0 + Duration::from_secs(15)), "countdown restarted");
        assert!(r.due(every, 3, t0 + Duration::from_secs(20)));
        assert!(!r.due(every, 1, t0 + Duration::from_secs(60)), "one page never rotates");
        r.reset(t0 + Duration::from_secs(60));
        assert!(!r.due(every, 3, t0 + Duration::from_secs(65)), "a manual change restarts the countdown");
        s.rotate_secs = 1;
        assert_eq!(rotate_interval(&s), Some(Duration::from_secs(ROTATE_MIN_SECS as u64)));
        s.rotate = false;
        assert_eq!(rotate_interval(&s), None);
        assert!(!r.due(None, 3, t0 + Duration::from_secs(3600)));
    }
}
