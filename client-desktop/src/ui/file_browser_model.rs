//! The file browser's two panes (ui/views/file_browser.slint): this computer on the left, the session's host on
//! the right. Local folders are read, and local operations run, on worker threads as the signed-in user; remote
//! ones arrive as [`BrowseUpdate`]s. Each pane keeps its selection (`domain::file_browser::Selection`: click,
//! Ctrl/Cmd+click, Shift+click, arrows), its sort column and a back history. Copying starts ordinary transfers
//! (`service::file_browser`). UI thread only.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{FileRow, MainWindow};
use crate::domain::file_browser::{BrowseMsg, Entry, HOST_OPS, Pick, Selection, SortKey, size_text, sort_by};
use crate::infra::{archive, shell};
use crate::service::context::Ctx;
use crate::service::file_browser::{self, Listing};
use crate::service::ui_port::BrowseUpdate;

/// Rows Page Up / Page Down move.
const PAGE: isize = 10;
/// Folders the Back button remembers per pane.
const MAX_BACK: usize = 50;
/// Local progress reaches the status line at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const OLD_HOST: &str = "This device runs an older OpenDesk. Update it to rename, delete, extract or compress there.";

/// One pane: the folder shown ("" = top level), its parent, its entries in the order shown, the selection, the
/// sort column, and the folders Back returns to.
#[derive(Default)]
struct Side {
    path: String,
    parent: Option<String>,
    entries: Vec<Entry>,
    sel: Selection,
    key: SortKey,
    desc: bool,
    back: Vec<String>,
    /// The listing on its way was asked for by Back (it must not be remembered as a place to go back to).
    going_back: bool,
}

impl Side {
    /// A new listing. The same folder again (a refresh) keeps the selection and cursor by path.
    fn set(&mut self, path: String, parent: Option<String>, entries: Vec<Entry>) {
        let same = path == self.path;
        let (keep, cursor) = if same { (self.picked(), self.cursor_path()) } else { (Vec::new(), None) };
        if !same && !self.going_back {
            self.back.push(std::mem::take(&mut self.path));
            if self.back.len() > MAX_BACK {
                self.back.remove(0);
            }
        }
        self.going_back = false;
        (self.path, self.parent, self.entries) = (path, parent, entries);
        sort_by(&mut self.entries, self.key, !self.desc);
        self.restore(&keep, cursor);
    }

    /// Re-sorts by the current column, keeping the selection and cursor.
    fn resort(&mut self) {
        let (keep, cursor) = (self.picked(), self.cursor_path());
        sort_by(&mut self.entries, self.key, !self.desc);
        self.restore(&keep, cursor);
    }

    fn restore(&mut self, keep: &[String], cursor: Option<String>) {
        let picked: Vec<usize> = (0..self.entries.len()).filter(|i| keep.contains(&self.entries[*i].path)).collect();
        let cursor = cursor.and_then(|c| self.entries.iter().position(|e| e.path == c));
        self.sel = Selection::with(self.entries.len(), &picked, cursor);
    }

    fn cursor_path(&self) -> Option<String> {
        self.sel.cursor().and_then(|c| self.entries.get(c)).map(|e| e.path.clone())
    }

    fn picked(&self) -> Vec<String> {
        self.sel.indices().into_iter().filter_map(|i| self.entries.get(i)).map(|e| e.path.clone()).collect()
    }

    /// The only selected entry, if exactly one is.
    fn single(&self) -> Option<&Entry> {
        match self.sel.indices().as_slice() {
            [one] => self.entries.get(*one),
            _ => None,
        }
    }

    /// "24 items", or "3 of 24 selected · 4.2 MB" (the size of the selected files).
    fn footer(&self) -> String {
        let n = self.entries.len();
        let count = self.sel.count();
        if count == 0 {
            return match n {
                0 => String::new(),
                1 => "1 item".into(),
                n => format!("{n} items"),
            };
        }
        let bytes: u64 = self.sel.indices().iter().filter_map(|i| self.entries.get(*i)).map(|e| e.size).sum();
        let size = if bytes > 0 { format!(" · {}", size_text(bytes)) } else { String::new() };
        format!("{count} of {n} selected{size}")
    }
}

#[derive(Default)]
struct State {
    ctx: Option<Ctx>,
    partner: String,
    local: Side,
    remote: Side,
    /// The host serves the explorer operations (its listings name them).
    remote_ops: bool,
    /// What the last remote operation is doing ("Extracting photos.zip"), for its progress.
    remote_job: String,
}

impl State {
    fn side(&self, remote: bool) -> &Side {
        if remote { &self.remote } else { &self.local }
    }

    fn side_mut(&mut self, remote: bool) -> &mut Side {
        if remote { &mut self.remote } else { &mut self.local }
    }
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
    window.set_browser_remote_ops(false);
    window.set_browser_local_trash(shell::can_trash());
    window.set_browser_reveal_label(
        if cfg!(windows) {
            "Show in Explorer"
        } else if cfg!(target_os = "macos") {
            "Show in Finder"
        } else {
            "Show in Folder"
        }
        .into(),
    );
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
        BrowseUpdate::Remote { partner, path, parent, entries, more, ops } if partner == current => {
            let full = HOST_OPS.iter().all(|op| ops.iter().any(|o| o == op));
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.remote.set(path, parent, entries);
                s.remote_ops = full;
            });
            window.set_browser_remote_ops(full);
            window.set_browser_remote_note(if more { "Showing the first 5000 items.".into() } else { "".into() });
            show(window);
        }
        BrowseUpdate::Failed { partner, message } if partner == current => {
            STATE.with(|s| s.borrow_mut().remote.going_back = false);
            window.set_browser_remote_note(message.into());
        }
        BrowseUpdate::OpFailed { partner, message } if partner == current => {
            window.set_browser_status(message.into());
            refresh_remote(window);
        }
        BrowseUpdate::Progress { partner, done, total } if partner == current => {
            let job = STATE.with(|s| s.borrow().remote_job.clone());
            window.set_browser_status(progress_text(&job, done, total).into());
        }
        BrowseUpdate::Connected { partner } if partner == current => {
            window.set_browser_connected(true);
            go_remote(window, String::new());
        }
        BrowseUpdate::Ended { partner, message } if partner == current => {
            window.set_browser_connected(false);
            window.set_browser_remote_ops(false);
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.remote = Side::default();
                s.remote_ops = false;
            });
            show(window);
            window.set_browser_remote_note(message.into());
        }
        BrowseUpdate::Done { partner, note } if partner == current => {
            // A new folder, a rename, an extraction, a started download: show the remote folder as it is now.
            if !note.is_empty() {
                window.set_browser_status(note.into());
            }
            refresh_remote(window);
        }
        _ => {}
    }
}

fn progress_text(job: &str, done: u64, total: u64) -> String {
    let job = if job.is_empty() { "Working" } else { job };
    match (done * 100).checked_div(total) {
        Some(pct) => format!("{job}… {}%", pct.min(100)),
        None => format!("{job}…"),
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
            Err(e) => {
                STATE.with(|s| s.borrow_mut().local.going_back = false);
                w.set_browser_local_note(e.into());
            }
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

fn go(window: &MainWindow, remote: bool, path: String) {
    if remote { go_remote(window, path) } else { go_local(window, path) }
}

fn refresh_remote(window: &MainWindow) {
    let path = STATE.with(|s| s.borrow().remote.path.clone());
    go_remote(window, path);
}

fn refresh_local(window: &MainWindow) {
    let path = STATE.with(|s| s.borrow().local.path.clone());
    go_local(window, path);
}

pub fn up(window: &MainWindow, remote: bool) {
    if let Some(p) = STATE.with(|s| s.borrow().side(remote).parent.clone()) {
        go(window, remote, p);
    }
}

fn back(window: &MainWindow, remote: bool) {
    let target = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let side = s.side_mut(remote);
        let target = side.back.pop();
        side.going_back = target.is_some();
        target
    });
    if let Some(p) = target {
        go(window, remote, p);
    }
}

/// A click on row `index`; `mode` 0 = plain, 1 = Ctrl/Cmd (toggle), 2 = Shift (range).
/// Rows change in place, never rebuilt, so the second click of a double click reaches the same row.
pub fn select(window: &MainWindow, remote: bool, index: usize, mode: i32) {
    change_selection(window, remote, |sel| sel.pick(index, Pick::from_mode(mode)));
}

/// Applies `f` to a pane's selection and repaints only the rows whose state changed.
fn change_selection(window: &MainWindow, remote: bool, f: impl FnOnce(&mut Selection)) {
    let changed: Vec<(usize, bool)> = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let sel = &mut s.side_mut(remote).sel;
        if sel.is_empty() {
            return Vec::new();
        }
        let before: Vec<bool> = (0..sel.len()).map(|i| sel.is_picked(i)).collect();
        f(sel);
        before.iter().enumerate().filter(|(i, b)| sel.is_picked(*i) != **b).map(|(i, b)| (i, !*b)).collect()
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

/// Keys and argument-less menu entries of a pane (see ui/views/file_browser.slint).
pub fn action(window: &MainWindow, remote: bool, what: &str) {
    let (verb, extend) = match what.strip_suffix("-extend") {
        Some(v) => (v, true),
        None => (what, false),
    };
    let step = |delta: isize| change_selection(window, remote, |sel| sel.step(delta, extend));
    match verb {
        "up" => step(-1),
        "down" => step(1),
        "page-up" => step(-PAGE),
        "page-down" => step(PAGE),
        "home" => step(isize::MIN / 2),
        "end" => step(isize::MAX / 2),
        "all" => change_selection(window, remote, Selection::all),
        "clear" => change_selection(window, remote, Selection::clear),
        "open" => {
            let target = STATE.with(|s| {
                let s = s.borrow();
                let sel = &s.side(remote).sel;
                sel.cursor().filter(|c| sel.is_picked(*c)).or_else(|| sel.indices().first().copied())
            });
            if let Some(i) = target {
                open_entry(window, remote, i);
            }
        }
        "back" => back(window, remote),
        "refresh" => refresh(window),
        "copy-path" => copy_paths(window, remote),
        "reveal" if !remote => reveal(window),
        "extract-here" => extract(window, remote, false),
        "extract-folder" => extract(window, remote, true),
        "compress" => compress(window, remote),
        _ => {}
    }
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
    window.set_browser_status(format!("Copying {} to {into}…", items(n)).into());
    true
}

/// Double-click, Enter or Open: a folder opens in the pane; a local file opens in its default app; a remote file
/// is downloaded to a temporary folder and then opens the same way.
pub fn open_entry(window: &MainWindow, remote: bool, index: usize) {
    let (ctx, partner, entry) = STATE.with(|s| {
        let s = s.borrow();
        (s.ctx.clone(), s.partner.clone(), s.side(remote).entries.get(index).cloned())
    });
    let Some(entry) = entry else { return };
    if entry.dir {
        return go(window, remote, entry.path);
    }
    if remote {
        let Some(ctx) = ctx else { return };
        match file_browser::open_remote(&ctx, &partner, entry.path) {
            Ok(()) => window.set_browser_status(format!("Downloading {} to open it…", entry.name).into()),
            Err(e) => window.set_browser_status(e.into()),
        }
        return;
    }
    window.set_browser_status(format!("Opening {}…", entry.name).into());
    let weak = window.as_weak();
    // The shell can take a moment (it may start the app); never on the UI thread.
    crate::infra::thread::spawn_named("browse-open", move || {
        let result = shell::open(&PathBuf::from(&entry.path));
        let _ = weak.upgrade_in_event_loop(move |w| {
            w.set_browser_status(result.err().unwrap_or_default().into());
        });
    });
}

pub fn mkdir(window: &MainWindow, remote: bool, name: String) {
    let (ctx, partner, path) = STATE.with(|s| {
        let s = s.borrow();
        (s.ctx.clone(), s.partner.clone(), s.side(remote).path.clone())
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

/// Renames the only selected item of a pane.
pub fn rename(window: &MainWindow, remote: bool, name: String) {
    let Some(target) = STATE.with(|s| s.borrow().side(remote).single().map(|e| e.path.clone())) else { return };
    if remote {
        return remote_op(window, "Renaming".into(), |req| BrowseMsg::Rename { req, path: target, name });
    }
    match file_browser::rename_local(&target, &name) {
        Ok(()) => {
            window.set_browser_status("".into());
            refresh_local(window);
        }
        Err(e) => window.set_browser_status(e.into()),
    }
}

/// Deletes a pane's selection; the view asked the user first (ui/views/file_browser.slint). Local items go to the
/// Recycle Bin where there is one; remote ones are deleted for good (the prompt says so).
pub fn delete(window: &MainWindow, remote: bool) {
    let paths = STATE.with(|s| s.borrow().side(remote).picked());
    if paths.is_empty() {
        return;
    }
    let n = paths.len();
    if remote {
        return remote_op(window, format!("Deleting {}", items(n)), |req| BrowseMsg::Delete { req, paths });
    }
    window.set_browser_status(format!("Deleting {}…", items(n)).into());
    local_job(window, move |_| file_browser::delete_local(&paths, shell::can_trash()));
}

fn extract(window: &MainWindow, remote: bool, folder: bool) {
    let Some(entry) = STATE.with(|s| s.borrow().side(remote).single().cloned()) else { return };
    if entry.dir || archive::kind_of(&entry.name).is_none() {
        return;
    }
    let job = format!("Extracting {}", entry.name);
    if remote {
        return remote_op(window, job, |req| BrowseMsg::Extract { req, path: entry.path, folder });
    }
    window.set_browser_status(format!("{job}…").into());
    local_job(window, move |progress| {
        file_browser::extract_local(&entry.path, folder, &mut |d, t| progress(progress_text(&job, d, t)))
    });
}

fn compress(window: &MainWindow, remote: bool) {
    let paths = STATE.with(|s| s.borrow().side(remote).picked());
    if paths.is_empty() {
        return;
    }
    let job = format!("Compressing {}", items(paths.len()));
    if remote {
        return remote_op(window, job, |req| BrowseMsg::Compress { req, paths });
    }
    window.set_browser_status(format!("{job}…").into());
    local_job(window, move |progress| {
        file_browser::compress_local(&paths, &mut |d, t| progress(progress_text(&job, d, t)))
    });
}

/// Sends an explorer operation to the host, if it serves them; `job` describes it in the status line.
fn remote_op(window: &MainWindow, job: String, make: impl FnOnce(u64) -> BrowseMsg) {
    let (ctx, partner, ops) = STATE.with(|s| {
        let s = s.borrow();
        (s.ctx.clone(), s.partner.clone(), s.remote_ops)
    });
    let Some(ctx) = ctx else { return };
    if !ops || !window.get_browser_connected() {
        window.set_browser_status(OLD_HOST.into());
        return;
    }
    window.set_browser_status(format!("{job}…").into());
    STATE.with(|s| s.borrow_mut().remote_job = job);
    file_browser::request_remote(&ctx, &partner, make);
}

/// Runs a local operation on a worker thread; it may report progress lines (throttled), and its result (a note
/// or an error) ends in the status line, followed by a fresh listing of the local folder.
fn local_job(window: &MainWindow, job: impl FnOnce(&mut dyn FnMut(String)) -> Result<String, String> + Send + 'static) {
    let weak = window.as_weak();
    crate::infra::thread::spawn_named("browse-job", move || {
        let mut last = Instant::now();
        let progress_weak = weak.clone();
        let mut progress = move |line: String| {
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                let _ = progress_weak.upgrade_in_event_loop(move |w| w.set_browser_status(line.into()));
            }
        };
        let result = job(&mut progress);
        let _ = weak.upgrade_in_event_loop(move |w| {
            w.set_browser_status(result.unwrap_or_else(|e| e).into());
            refresh_local(&w);
        });
    });
}

/// Puts the selected items' full paths on the clipboard, one per line.
fn copy_paths(window: &MainWindow, remote: bool) {
    let (ctx, paths) = STATE.with(|s| (s.borrow().ctx.clone(), s.borrow().side(remote).picked()));
    if paths.is_empty() {
        return;
    }
    if core_engine::clipboard::set_text(&paths.join(if cfg!(windows) { "\r\n" } else { "\n" })) {
        // Ours, not something to sync to a session.
        if let Some(ctx) = ctx {
            ctx.mark_clipboard_synced();
        }
        window.set_browser_status(if paths.len() == 1 { "Path copied".into() } else { "Paths copied".into() });
    } else {
        window.set_browser_status("Clipboard is busy; try again".into());
    }
}

/// Shows the selected local item in Explorer (Finder).
fn reveal(window: &MainWindow) {
    let Some(path) = STATE.with(|s| s.borrow().local.picked().into_iter().next()) else { return };
    if let Err(e) = shell::reveal(&PathBuf::from(path)) {
        window.set_browser_status(e.into());
    }
}

/// A click on a column header: that column, or the other direction if it already is.
pub fn sort(window: &MainWindow, remote: bool, key: i32) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let side = s.side_mut(remote);
        let key = SortKey::from_index(key);
        if side.key == key {
            side.desc = !side.desc;
        } else {
            (side.key, side.desc) = (key, false);
        }
        side.resort();
    });
    show(window);
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
    window.set_browser_status(format!("Uploading {} to {into}…", items(n)).into());
    change_selection(window, false, Selection::clear);
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
    window.set_browser_status(format!("Downloading {} to {into}…", items(n)).into());
    change_selection(window, true, Selection::clear);
}

pub fn refresh(window: &MainWindow) {
    refresh_local(window);
    refresh_remote(window);
}

fn items(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

/// Selection counts, the single selected names, cursors and footers (not the rows).
fn show_counts(window: &MainWindow) {
    STATE.with(|s| {
        let s = s.borrow();
        let cursor = |side: &Side| side.sel.cursor().map_or(-1, |c| c as i32);
        let single = |side: &Side| side.single().map(|e| e.name.clone()).unwrap_or_default();
        window.set_browser_local_selected(s.local.sel.count() as i32);
        window.set_browser_remote_selected(s.remote.sel.count() as i32);
        window.set_browser_local_cursor(cursor(&s.local));
        window.set_browser_remote_cursor(cursor(&s.remote));
        window.set_browser_local_single(single(&s.local).into());
        window.set_browser_remote_single(single(&s.remote).into());
        window.set_browser_local_footer(s.local.footer().into());
        window.set_browser_remote_footer(s.remote.footer().into());
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
        window.set_browser_local_can_back(!s.local.back.is_empty());
        window.set_browser_remote_can_back(!s.remote.back.is_empty());
        window.set_browser_local_sort(s.local.key.index());
        window.set_browser_remote_sort(s.remote.key.index());
        window.set_browser_local_sort_desc(s.local.desc);
        window.set_browser_remote_sort_desc(s.remote.desc);
        LOCAL.with(|m| fill(m, &s.local));
        REMOTE.with(|m| fill(m, &s.remote));
    });
    show_counts(window);
}

fn fill(model: &VecModel<FileRow>, side: &Side) {
    let rows: Vec<FileRow> = side
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| FileRow {
            name: e.name.clone().into(),
            size_text: if e.dir { "".into() } else { size_text(e.size).into() },
            date_text: date_text(e.mtime).into(),
            is_dir: e.dir,
            selected: side.sel.is_picked(i),
            archive_stem: if !e.dir && archive::kind_of(&e.name).is_some() {
                archive::stem_of(&e.name).into()
            } else {
                "".into()
            },
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
    use super::*;

    fn entry(name: &str, dir: bool, size: u64) -> Entry {
        Entry { name: name.into(), path: format!("/x/{name}"), dir, size, mtime: 0 }
    }

    #[test]
    fn dates_convert_from_unix_milliseconds() {
        // 2026-11-20 22:35:15 UTC; the local rendering depends on the machine's zone, so only check it parsed.
        let shown = date_text(1_795_214_115_000);
        assert!(!shown.is_empty() && !shown.contains('T'), "{shown}");
        assert_eq!(date_text(0), "");
    }

    #[test]
    fn a_refresh_keeps_the_selection_and_navigation_remembers_back() {
        let mut side = Side::default();
        side.set("/x".into(), Some("/".into()), vec![entry("b", false, 5), entry("a", false, 7), entry("D", true, 0)]);
        assert_eq!(side.back, [""], "the top level is where Back returns to");
        side.sel.pick(1, Pick::Only); // "a" (folders first, then names)
        side.sel.pick(2, Pick::Toggle); // "b"
        assert_eq!(side.footer(), "2 of 3 selected · 12 B");
        side.set("/x".into(), Some("/".into()), vec![entry("a", false, 7), entry("b", false, 5), entry("c", false, 1)]);
        assert_eq!(side.picked(), ["/x/a", "/x/b"]);
        assert_eq!(side.back.len(), 1, "a refresh is not a new place");
        (side.key, side.desc) = (SortKey::Size, true);
        side.resort();
        assert_eq!(side.entries[0].name, "a");
        assert_eq!(side.picked(), ["/x/a", "/x/b"]);
        side.set("/y".into(), Some("/".into()), vec![]);
        assert_eq!((side.back.as_slice(), side.sel.count()), (["".to_string(), "/x".to_string()].as_slice(), 0));
        side.going_back = true;
        side.set("/x".into(), Some("/".into()), vec![]);
        assert_eq!(side.back.len(), 2, "going back does not add the folder it left");
        assert_eq!(side.footer(), "");
    }

    #[test]
    fn progress_reads_as_a_percentage() {
        assert_eq!(progress_text("Extracting a.zip", 50, 200), "Extracting a.zip… 25%");
        assert_eq!(progress_text("", 1, 0), "Working…");
    }
}
