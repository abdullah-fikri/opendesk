//! The dashboard lists (session history and relay hosts): keeps the last full lists from the services, shows
//! the rows that match the search box and keeps the selected row valid. UI thread only (Slint models and
//! timers are not `Send`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

use super::{HistoryItem, HostItem, MainWindow};
use crate::domain::list_search::{self, Query};

/// The lists are filtered once typing pauses this long, not on every keystroke.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);

/// Dashboard tab that shows the relay's host list (admin edition); every other value shows the history.
const HOSTS_TAB: i32 = 1;

struct State {
    window: Weak<MainWindow>,
    history: Vec<HistoryItem>,
    hosts: Vec<HostItem>,
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
    w.get_is_admin() && w.get_active_dashboard_tab() == HOSTS_TAB
}

/// Desk IDs of the rows the active tab shows, in order.
fn shown_ids(w: &MainWindow) -> Vec<SharedString> {
    if hosts_tab_active(w) {
        w.get_server_hosts().iter().map(|h| h.desk_id).collect()
    } else {
        w.get_history_items().iter().map(|h| h.desk_id).collect()
    }
}

/// Writes the matching rows and the totals to the window, and drops a selection whose row is no longer shown.
fn show(s: &State) {
    let Some(w) = s.window.upgrade() else { return };
    let query = Query::parse(&s.query);
    let history: Vec<HistoryItem> =
        s.history.iter().filter(|h| query.matches(&h.desk_id, &[&h.alias])).cloned().collect();
    let hosts: Vec<HostItem> = s
        .hosts
        .iter()
        .filter(|h| query.matches(&h.desk_id, &[&h.alias, &h.device_name, &h.platform]))
        .cloned()
        .collect();
    w.set_history_total(s.history.len() as i32);
    w.set_host_total(s.hosts.len() as i32);
    w.set_list_query(if query.is_empty() { SharedString::new() } else { s.query.trim().into() });
    w.set_history_items(ModelRc::from(Rc::new(VecModel::from(history))));
    w.set_server_hosts(ModelRc::from(Rc::new(VecModel::from(hosts))));
    let selected = w.get_list_selected();
    if !selected.is_empty() && !shown_ids(&w).contains(&selected) {
        w.set_list_selected(SharedString::new());
    }
}
