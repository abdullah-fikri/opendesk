//! The `transfers` model of the main window: one row per file transfer, updated in place and removed a few
//! seconds after it finishes. UI thread only (Slint models are not `Send`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{Model, ModelRc, Timer, VecModel};

use super::{MainWindow, TransferItem};
use crate::service::ui_port::{TransferState, TransferUpdate};

/// Finished rows stay this long, failed ones longer so the reason can be read.
const DONE_LINGER: Duration = Duration::from_secs(4);
const FAILED_LINGER: Duration = Duration::from_secs(15);
/// Most rows shown; the oldest finished rows make room first.
const MAX_ROWS: usize = 6;

const ACTIVE: i32 = 0;
const DONE: i32 = 1;
const FAILED: i32 = 2;

thread_local! {
    static MODEL: RefCell<Option<Rc<VecModel<TransferItem>>>> = const { RefCell::new(None) };
    /// When each transfer started, for its speed.
    static STARTED: RefCell<HashMap<String, Instant>> = RefCell::new(HashMap::new());
}

/// Installs an empty model on the window. Call once, on the UI thread.
pub fn install(window: &MainWindow) {
    let model = Rc::new(VecModel::<TransferItem>::default());
    window.set_transfers(ModelRc::from(Rc::clone(&model)));
    MODEL.with(|m| *m.borrow_mut() = Some(model));
}

fn with_model(f: impl FnOnce(&VecModel<TransferItem>)) {
    MODEL.with(|m| {
        if let Some(model) = m.borrow().as_ref() {
            f(model);
        }
    });
}

/// Adds or updates the row of `update.id`; a finished row is removed after a while.
pub fn apply(update: TransferUpdate) {
    let started = STARTED.with(|s| *s.borrow_mut().entry(update.id.clone()).or_insert_with(Instant::now));
    let row = to_row(&update, started.elapsed());
    let id = update.id;
    with_model(|m| match (0..m.row_count()).find(|&i| m.row_data(i).is_some_and(|r| r.id == row.id)) {
        Some(i) => m.set_row_data(i, row.clone()),
        None => {
            if m.row_count() >= MAX_ROWS {
                let oldest_finished = (0..m.row_count()).find(|&i| m.row_data(i).is_some_and(|r| r.state != ACTIVE));
                m.remove(oldest_finished.unwrap_or(0));
            }
            m.push(row.clone());
        }
    });
    if row.state != ACTIVE {
        STARTED.with(|s| s.borrow_mut().remove(&id));
        let linger = if row.state == DONE { DONE_LINGER } else { FAILED_LINGER };
        Timer::single_shot(linger, move || remove_finished(&id));
    }
}

/// Removes the row of `id` (the panel's close button, or the linger timer) unless it is still running.
pub fn remove_finished(id: &str) {
    with_model(|m| {
        if let Some(i) = (0..m.row_count()).find(|&i| m.row_data(i).is_some_and(|r| r.id == id && r.state != ACTIVE)) {
            m.remove(i);
        }
    });
}

fn to_row(u: &TransferUpdate, elapsed: Duration) -> TransferItem {
    let progress = if u.total == 0 { 1.0 } else { (u.done as f64 / u.total as f64).min(1.0) as f32 };
    let (state, percent_text, detail) = match &u.state {
        TransferState::Active => {
            let secs = elapsed.as_secs_f64();
            let speed = if secs >= 1.0 { format!(" · {}/s", bytes(u.done as f64 / secs)) } else { String::new() };
            let detail = format!("{} / {}{speed}", bytes(u.done as f64), bytes(u.total as f64));
            (ACTIVE, format!("{}%", (progress * 100.0).floor().min(99.0)), detail)
        }
        TransferState::Verifying => (ACTIVE, "99%".into(), "Waiting for the other device to verify it...".into()),
        TransferState::Done => {
            let detail = if u.outgoing { "Sent and verified" } else { "Saved in Downloads\\OpenDesk" };
            (DONE, "Done".into(), format!("{} · {detail}", bytes(u.total as f64)))
        }
        TransferState::Failed(reason) => (FAILED, "Failed".into(), reason.clone()),
    };
    TransferItem {
        id: u.id.clone().into(),
        name: u.name.clone().into(),
        outgoing: u.outgoing,
        progress: if state == DONE { 1.0 } else { progress },
        percent_text: percent_text.into(),
        detail: detail.into(),
        state,
    }
}

/// "512 B", "3.2 KB", "10.4 MB", "1.2 GB".
fn bytes(n: f64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = n;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{value:.0} {}", UNITS[0]) } else { format!("{value:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(done: u64, total: u64, state: TransferState) -> TransferUpdate {
        TransferUpdate { id: "out:1:ft_1".into(), name: "a.bin".into(), outgoing: true, done, total, state }
    }

    #[test]
    fn bytes_uses_binary_units() {
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(10_926_102.0), "10.4 MB");
        assert_eq!(bytes(1536.0), "1.5 KB");
    }

    #[test]
    fn active_rows_never_show_100_percent() {
        let row = to_row(&update(999, 1000, TransferState::Active), Duration::from_secs(2));
        assert_eq!(row.percent_text.as_str(), "99%");
        assert_eq!(row.state, ACTIVE);
        assert!(row.detail.contains("/s"));
    }

    #[test]
    fn finished_rows_fill_the_bar_and_failed_ones_keep_the_reason() {
        let done = to_row(&update(1000, 1000, TransferState::Done), Duration::ZERO);
        assert_eq!((done.state, done.progress), (DONE, 1.0));
        let failed = to_row(&update(10, 1000, TransferState::Failed("Relay connection lost".into())), Duration::ZERO);
        assert_eq!((failed.state, failed.detail.as_str()), (FAILED, "Relay connection lost"));
    }
}
