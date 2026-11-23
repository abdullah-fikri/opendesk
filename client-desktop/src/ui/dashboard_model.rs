//! The dashboard lists (session history and relay hosts): keeps the last full lists from the services, shows
//! the rows that match the search box and keeps the selected row valid. UI thread only (Slint models and
//! timers are not `Send`).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

use super::{HistoryItem, HostItem, MainWindow};
use crate::domain::desk_id;
use crate::domain::list_search::{self, Query};

/// The lists are filtered once typing pauses this long, not on every keystroke.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);

/// Dashboard tab that shows the relay's host list (admin edition); every other value shows the history.
const HOSTS_TAB: i32 = 1;
/// Dashboard tab that shows one community's group: the host list filtered by `State::members`.
pub const COMMUNITY_TAB: i32 = 2;

struct State {
    window: Weak<MainWindow>,
    history: Vec<HistoryItem>,
    hosts: Vec<HostItem>,
    /// Clean IDs of the devices the community tab shows (`community_model`); None shows every host.
    members: Option<HashSet<String>>,
    /// Search text currently applied to the lists.
    query: String,
    /// Restarted by each keystroke, so only the last one filters.
    debounce: Timer,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Starts with empty lists. Call once, on the UI thread, before the window runs.
pub fn install(window: &MainWindow) {
    let state = State {
        window: window.as_weak(),
        history: Vec::new(),
        hosts: Vec::new(),
        members: None,
        query: String::new(),
        debounce: Timer::default(),
    };
    STATE.with(|s| *s.borrow_mut() = Some(state));
}

fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            f(state);
        }
    });
}

/// Replaces the full session history and shows the rows matching the current search.
pub fn set_history(items: Vec<HistoryItem>) {
    with_state(|s| {
        s.history = items;
        show(s);
    });
}

/// Replaces the full relay host list and shows the rows matching the current search.
pub fn set_hosts(items: Vec<HostItem>) {
    with_state(|s| {
        s.hosts = items;
        show(s);
    });
}

/// Limits the community tab to these devices (clean IDs); None when no community is open.
#[cfg_attr(not(feature = "admin"), allow(dead_code))]
pub fn set_members(members: Option<HashSet<String>>) {
    with_state(|s| {
        s.members = members;
        show(s);
    });
}

/// Shows the rows again with the current search (the sidebar's host filter changed).
pub fn refresh() {
    with_state(|s| show(s));
}

/// The search box changed: filters once typing pauses. Clearing it shows every row at once.
pub fn search_edited(text: SharedString) {
    with_state(|s| {
        if text.trim().is_empty() {
            s.debounce.stop();
            s.query.clear();
            show(s);
            return;
        }
        s.debounce.start(TimerMode::SingleShot, SEARCH_DEBOUNCE, move || {
            with_state(|s| {
                s.query = text.to_string();
                show(s);
            });
        });
    });
}

/// Moves the selection `delta` rows through the shown rows of the active tab (arrow keys).
pub fn move_selection(w: &MainWindow, delta: i32) {
    let ids = shown_ids(w);
    let selected = w.get_list_selected();
    let current = ids.iter().position(|id| *id == selected);
    if let Some(i) = list_search::step(ids.len(), current, delta) {
        w.set_list_selected(ids[i].clone());
    }
}

/// Connects to the selected row of the active tab (Enter); an offline relay host is skipped.
pub fn connect_selected(w: &MainWindow) {
    let selected = w.get_list_selected();
    if selected.is_empty() {
        return;
    }
    let connectable = if hosts_tab_active(w) {
        w.get_server_hosts().iter().any(|h| h.desk_id == selected && h.online)
    } else {
        w.get_history_items().iter().any(|h| h.desk_id == selected)
    };
    if connectable {
        w.invoke_request_connect(selected);
    }
}

fn hosts_tab_active(w: &MainWindow) -> bool {
    let tab = w.get_active_dashboard_tab();
    w.get_is_admin() && (tab == HOSTS_TAB || tab == COMMUNITY_TAB)
}

/// Desk IDs of the rows the active tab shows, in order.
fn shown_ids(w: &MainWindow) -> Vec<SharedString> {
    if hosts_tab_active(w) {
        w.get_server_hosts().iter().map(|h| h.desk_id).collect()
    } else {
        w.get_history_items().iter().map(|h| h.desk_id).collect()
    }
}

/// Sidebar filter of the relay hosts (`host_filter` in `app_window.slint`).
const SHOW_ONLINE_ONLY: i32 = 1;
const SHOW_OFFLINE_ONLY: i32 = 2;

/// The hosts matching `query`, limited to `members` (clean IDs) when given, split into online and offline; the
/// sidebar filter (`SHOW_*`) may empty one side.
fn split_hosts(
    hosts: &[HostItem],
    query: &Query,
    filter: i32,
    members: Option<&HashSet<String>>,
) -> (Vec<HostItem>, Vec<HostItem>) {
    let (online, offline): (Vec<HostItem>, Vec<HostItem>) = hosts
        .iter()
        .filter(|h| members.is_none_or(|m| m.contains(&desk_id::clean(&h.desk_id))))
        .filter(|h| query.matches(&h.desk_id, &[&h.alias, &h.device_name, &h.platform]))
        .cloned()
        .partition(|h| h.online);
    let online = if filter == SHOW_OFFLINE_ONLY { Vec::new() } else { online };
    let offline = if filter == SHOW_ONLINE_ONLY { Vec::new() } else { offline };
    (online, offline)
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// Writes the matching rows and the totals to the window, and drops a selection whose row is no longer shown.
/// Relay hosts are split into online and offline sections (the sidebar filter may hide one); `server_hosts`
/// holds both in display order, which the arrow keys follow.
fn show(s: &State) {
    let Some(w) = s.window.upgrade() else { return };
    let query = Query::parse(&s.query);
    let history: Vec<HistoryItem> =
        s.history.iter().filter(|h| query.matches(&h.desk_id, &[&h.alias])).cloned().collect();
    let members = if w.get_active_dashboard_tab() == COMMUNITY_TAB { s.members.as_ref() } else { None };
    let (online, offline) = split_hosts(&s.hosts, &query, w.get_host_filter(), members);
    let shown: Vec<HostItem> = online.iter().chain(offline.iter()).cloned().collect();
    let online_total = s.hosts.iter().filter(|h| h.online).count();
    w.set_history_total(s.history.len() as i32);
    w.set_host_total(s.hosts.len() as i32);
    w.set_host_online_total(online_total as i32);
    w.set_host_offline_total((s.hosts.len() - online_total) as i32);
    w.set_list_query(if query.is_empty() { SharedString::new() } else { s.query.trim().into() });
    w.set_history_items(model(history));
    w.set_hosts_online(model(online));
    w.set_hosts_offline(model(offline));
    w.set_server_hosts(model(shown));
    let selected = w.get_list_selected();
    if !selected.is_empty() && !shown_ids(&w).contains(&selected) {
        w.set_list_selected(SharedString::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str, name: &str, online: bool) -> HostItem {
        HostItem { desk_id: desk_id::format(id).into(), device_name: name.into(), online, ..Default::default() }
    }

    #[test]
    fn community_tab_shows_only_the_group_members() {
        let hosts = vec![
            host("111111111", "Kasir 1", true),
            host("222222222", "Kasir 2", false),
            host("333333333", "Office", true),
        ];
        let all = Query::parse("");
        let (on, off) = split_hosts(&hosts, &all, 0, None);
        assert_eq!((on.len(), off.len()), (2, 1));

        let group: HashSet<String> = ["111111111".to_string(), "222222222".to_string()].into();
        let (on, off) = split_hosts(&hosts, &all, 0, Some(&group));
        assert_eq!(on.iter().map(|h| h.device_name.as_str()).collect::<Vec<_>>(), ["Kasir 1"]);
        assert_eq!(off.iter().map(|h| h.device_name.as_str()).collect::<Vec<_>>(), ["Kasir 2"]);

        // Search and the sidebar filter still apply inside a group.
        let (on, off) = split_hosts(&hosts, &Query::parse("kasir 2"), 0, Some(&group));
        assert!(on.is_empty() && off.len() == 1);
        let (on, off) = split_hosts(&hosts, &all, SHOW_OFFLINE_ONLY, Some(&group));
        assert!(on.is_empty() && off.len() == 1);

        // An empty group shows nothing (not everything).
        let (on, off) = split_hosts(&hosts, &all, 0, Some(&HashSet::new()));
        assert!(on.is_empty() && off.is_empty());
    }
}
