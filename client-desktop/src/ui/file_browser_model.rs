//! The file browser's two panes (ui/views/file_browser.slint): this computer on the left, the session's host on
//! the right. Local folders are read on a worker thread as the signed-in user; remote ones arrive as
//! [`BrowseUpdate`]s. Copying starts ordinary transfers (`service::file_browser`). UI thread only.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{FileRow, MainWindow};
use crate::domain::file_browser::{Entry, size_text};
use crate::service::context::Ctx;
use crate::service::file_browser::{self, Listing};
use crate::service::ui_port::BrowseUpdate;

/// One pane: the folder shown ("" = top level), its parent, its entries and which are selected.
#[derive(Default)]
struct Side {
    path: String,
    parent: Option<String>,
    entries: Vec<Entry>,
    selected: Vec<bool>,
}

impl Side {
    fn set(&mut self, path: String, parent: Option<String>, entries: Vec<Entry>) {
        self.selected = vec![false; entries.len()];
        (self.path, self.parent, self.entries) = (path, parent, entries);
    }

    fn picked(&self) -> Vec<String> {
        self.entries.iter().zip(&self.selected).filter(|(_, s)| **s).map(|(e, _)| e.path.clone()).collect()
    }
}

#[derive(Default)]
struct State {
    ctx: Option<Ctx>,
    partner: String,
    local: Side,
    remote: Side,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static LOCAL: Rc<VecModel<FileRow>> = Rc::new(VecModel::default());
    static REMOTE: Rc<VecModel<FileRow>> = Rc::new(VecModel::default());
}

/// Installs the (empty) pane models. Call once, on the UI thread.
pub fn install(window: &MainWindow) {
    LOCAL.with(|m| window.set_browser_local_rows(ModelRc::from(Rc::clone(m))));
    REMOTE.with(|m| window.set_browser_remote_rows(ModelRc::from(Rc::clone(m))));
}

/// Opens the browser for device `partner` (`title` = its name): without an open session one is requested for
/// files only (no screen); both panes start at their top level.
pub fn open(window: &MainWindow, ctx: &Ctx, partner: &str, title: &str) {
    let title = if title.trim().is_empty() { crate::domain::desk_id::format(partner) } else { title.to_string() };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        *s = State { ctx: Some(Ctx::clone(ctx)), partner: partner.to_string(), ..State::default() };
    });
    window.set_browser_title(title.into());
    window.set_browser_status("".into());
    window.set_browser_connected(false);
    window.set_browser_open(true);
    show(window);
    go_local(window, String::new());
    if !crate::service::viewer::controller::request_files(ctx, partner) {
        window.set_browser_remote_note("No room for another connection. Close a session first.".into());
        return;
    }
    if ctx.viewers.is_live(partner) {
        window.set_browser_connected(true);
        go_remote(window, String::new());
    } else {
        window.set_browser_remote_note("Connecting…".into());
    }
}

pub fn close(window: &MainWindow) {
    let (ctx, partner) = STATE.with(|s| (s.borrow().ctx.clone(), s.borrow().partner.clone()));
    window.set_browser_open(false);
    STATE.with(|s| *s.borrow_mut() = State::default());
    show(window);
    if let Some(ctx) = ctx {
        crate::service::viewer::controller::browser_closed(&ctx, &partner);
    }
}

/// A remote answer from the service.
pub fn apply(window: &MainWindow, update: BrowseUpdate) {
    let current = STATE.with(|s| s.borrow().partner.clone());
    match update {
        BrowseUpdate::Remote { partner, path, parent, entries, more } if partner == current => {
            STATE.with(|s| s.borrow_mut().remote.set(path, parent, entries));
            window.set_browser_remote_note(if more { "Showing the first 5000 items.".into() } else { "".into() });
            show(window);
        }
        BrowseUpdate::Failed { partner, message } if partner == current => {
            window.set_browser_remote_note(message.into());
        }
        BrowseUpdate::Connected { partner } if partner == current => {
            window.set_browser_connected(true);
            go_remote(window, String::new());
        }
        BrowseUpdate::Ended { partner, message } if partner == current => {
            window.set_browser_connected(false);
            STATE.with(|s| s.borrow_mut().remote = Side::default());
            show(window);
            window.set_browser_remote_note(message.into());
        }
        BrowseUpdate::Done { partner } if partner == current => {
            // A new folder or a started download: show the remote folder as it is now.
            let path = STATE.with(|s| s.borrow().remote.path.clone());
            go_remote(window, path);
        }
        _ => {}
    }
}

/// Opens local folder `path` ("" = top level); read on a worker thread.
pub fn go_local(window: &MainWindow, path: String) {
    window.set_browser_local_note("Loading…".into());
    let weak = window.as_weak();
    crate::infra::thread::spawn_named("browse-local", move || {
        let result = file_browser::list_local(path.trim());
        let _ = weak.upgrade_in_event_loop(move |w| match result {
            Ok(Listing { path, parent, entries, more }) => {
                STATE.with(|s| s.borrow_mut().local.set(path, parent, entries));
                w.set_browser_local_note(if more { "Showing the first 5000 items.".into() } else { "".into() });
                show(&w);
            }
            Err(e) => w.set_browser_local_note(e.into()),
        });
    });
}

/// Opens remote folder `path` ("" = top level); the listing comes back through [`apply`].
pub fn go_remote(window: &MainWindow, path: String) {
    let (ctx, partner) = STATE.with(|s| (s.borrow().ctx.clone(), s.borrow().partner.clone()));
    let Some(ctx) = ctx else { return };
    // The browser speaks over the file channel's version of the protocol; an older device would never answer.
    if !crate::service::file_transfer::caps_of(&partner).file_channel {
        window.set_browser_remote_note(
            "This device runs an older OpenDesk without the file browser. Update it to browse its files.".into(),
        );
        return;
    }
    window.set_browser_remote_note("Loading…".into());
    file_browser::list_remote(&ctx, &partner, path.trim());
}

pub fn up(window: &MainWindow, remote: bool) {
    let parent = STATE.with(|s| {
        let s = s.borrow();
        if remote { s.remote.parent.clone() } else { s.local.parent.clone() }
    });
    if let Some(p) = parent {
        if remote { go_remote(window, p) } else { go_local(window, p) }
    }
}

/// A click on row `index`: `add` (Ctrl) toggles it in the selection, otherwise it becomes the only selected row.
/// Rows change in place, never rebuilt, so the second click of a double click reaches the same row.
pub fn select(window: &MainWindow, remote: bool, index: usize, add: bool) {
    let changed: Vec<(usize, bool)> = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let side = if remote { &mut s.remote } else { &mut s.local };
        if index >= side.selected.len() {
            return Vec::new();
        }
        let next: Vec<bool> = (0..side.selected.len())
            .map(|i| if add { if i == index { !side.selected[i] } else { side.selected[i] } } else { i == index })
            .collect();
        let changed = next.iter().enumerate().filter(|(i, v)| side.selected[*i] != **v).map(|(i, v)| (i, *v)).collect();
        side.selected = next;
        changed
    });
    let model = if remote { &REMOTE } else { &LOCAL };
    model.with(|m| {
        for (i, on) in changed {
            if let Some(mut row) = m.row_data(i) {
                row.selected = on;
                m.set_row_data(i, row);
            }
        }
    });
    show_counts(window);
}

/// Files dropped from Explorer while the browser is open: copied into the open remote folder. False when the
/// browser is closed (the drop then goes to the session as before).
pub fn on_external_drop(window: &MainWindow, paths: Vec<PathBuf>) -> bool {
    if !window.get_browser_open() {
        return false;
    }
    let (ctx, partner, into) = STATE.with(|s| {
        let s = s.borrow();
        (s.ctx.clone(), s.partner.clone(), s.remote.path.clone())
    });
    let Some(ctx) = ctx else { return true };
    if into.is_empty() || !window.get_browser_connected() {
        window.set_browser_status("Open a folder on the remote side first, then drop the files on it.".into());
        return true;
    }
    let n = paths.len();
    file_browser::upload(&ctx, &partner, paths, into.clone());
    window.set_browser_status(format!("Copying {n} item(s) to {into}…").into());
    true
}

/// Double-click: a folder opens.
pub fn open_entry(window: &MainWindow, remote: bool, index: usize) {
    let target = STATE.with(|s| {
        let s = s.borrow();
        let side = if remote { &s.remote } else { &s.local };
        side.entries.get(index).filter(|e| e.dir).map(|e| e.path.clone())
    });
    if let Some(path) = target {
        if remote { go_remote(window, path) } else { go_local(window, path) }
    }
}

pub fn mkdir(window: &MainWindow, remote: bool, name: String) {
    let (ctx, partner, path) = STATE.with(|s| {
        let s = s.borrow();
        (s.ctx.clone(), s.partner.clone(), if remote { s.remote.path.clone() } else { s.local.path.clone() })
    });
    if path.is_empty() || name.trim().is_empty() {
        return;
    }
    if remote {
        if let Some(ctx) = ctx {
            file_browser::mkdir_remote(&ctx, &partner, &path, &name);
        }
        return;
    }
    match file_browser::mkdir_local(&path, &name) {
        Ok(()) => go_local(window, path),
        Err(e) => window.set_browser_status(e.into()),
    }
}

/// Copies the selected local items into the open remote folder.
pub fn upload(window: &MainWindow) {
    let (ctx, partner, picked, into) = STATE.with(|s| {
        let s = s.borrow();
        (s.ctx.clone(), s.partner.clone(), s.local.picked(), s.remote.path.clone())
    });
    let Some(ctx) = ctx else { return };
    if picked.is_empty() || into.is_empty() {
        return;
    }
    let n = picked.len();
    file_browser::upload(&ctx, &partner, picked.into_iter().map(PathBuf::from).collect(), into.clone());
    window.set_browser_status(format!("Uploading {n} item(s) to {into}…").into());
    clear_selection(window, false);
}

/// Copies the selected remote items into the open local folder.
pub fn download(window: &MainWindow) {
    let (ctx, partner, picked, into) = STATE.with(|s| {
        let s = s.borrow();
        (s.ctx.clone(), s.partner.clone(), s.remote.picked(), s.local.path.clone())
    });
    let Some(ctx) = ctx else { return };
    if picked.is_empty() || into.is_empty() {
        return;
    }
    let n = picked.len();
    file_browser::download(&ctx, &partner, picked, PathBuf::from(&into));
    window.set_browser_status(format!("Downloading {n} item(s) to {into}…").into());
    clear_selection(window, true);
}

pub fn refresh(window: &MainWindow) {
    let (local, remote) = STATE.with(|s| (s.borrow().local.path.clone(), s.borrow().remote.path.clone()));
    go_local(window, local);
    go_remote(window, remote);
}

fn clear_selection(window: &MainWindow, remote: bool) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let side = if remote { &mut s.remote } else { &mut s.local };
        side.selected.iter_mut().for_each(|x| *x = false);
    });
    let model = if remote { &REMOTE } else { &LOCAL };
    model.with(|m| {
        for i in 0..m.row_count() {
            if let Some(mut row) = m.row_data(i).filter(|r| r.selected) {
                row.selected = false;
                m.set_row_data(i, row);
            }
        }
    });
    show_counts(window);
}

/// Paths, "up" and selection counts (not the rows).
fn show_counts(window: &MainWindow) {
    STATE.with(|s| {
        let s = s.borrow();
        window.set_browser_local_selected(s.local.selected.iter().filter(|x| **x).count() as i32);
        window.set_browser_remote_selected(s.remote.selected.iter().filter(|x| **x).count() as i32);
    });
}

/// Puts the state on the window.
fn show(window: &MainWindow) {
    STATE.with(|s| {
        let s = s.borrow();
        window.set_browser_local_path(s.local.path.clone().into());
        window.set_browser_remote_path(s.remote.path.clone().into());
        window.set_browser_local_can_up(s.local.parent.is_some());
        window.set_browser_remote_can_up(s.remote.parent.is_some());
        window.set_browser_local_selected(s.local.selected.iter().filter(|x| **x).count() as i32);
        window.set_browser_remote_selected(s.remote.selected.iter().filter(|x| **x).count() as i32);
        LOCAL.with(|m| fill(m, &s.local));
        REMOTE.with(|m| fill(m, &s.remote));
    });
}

fn fill(model: &VecModel<FileRow>, side: &Side) {
    let rows: Vec<FileRow> = side
        .entries
        .iter()
        .zip(&side.selected)
        .map(|(e, sel)| FileRow {
            name: e.name.clone().into(),
            size_text: if e.dir { "".into() } else { size_text(e.size).into() },
            date_text: date_text(e.mtime).into(),
            is_dir: e.dir,
            selected: *sel,
        })
        .collect();
    model.set_vec(rows);
}

/// Local "HH:MM · DD/MM" of a Unix millisecond time; "" when unknown.
fn date_text(ms: u64) -> String {
    if ms == 0 {
        return String::new();
    }
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let utc = format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60);
    crate::infra::clock::local_short(&utc)
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates_convert_from_unix_milliseconds() {
        // 2026-11-20 22:35:15 UTC; the local rendering depends on the machine's zone, so only check it parsed.
        let shown = super::date_text(1_795_214_115_000);
        assert!(!shown.is_empty() && !shown.contains('T'), "{shown}");
        assert_eq!(super::date_text(0), "");
    }
}
