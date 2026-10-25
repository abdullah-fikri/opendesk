//! The `sessions` model of the main window: one row per viewer session, updated in place.
//! UI thread only (Slint models are not `Send`).

use std::cell::RefCell;
use std::rc::Rc;

use slint::{Model, ModelRc, VecModel};

use super::{MainWindow, SessionView};

/// Status line of a session that has not shown its first frame yet.
pub const CONNECTING: &str = "Connecting...";

thread_local! {
    static MODEL: RefCell<Option<Rc<VecModel<SessionView>>>> = const { RefCell::new(None) };
}

/// Installs an empty model on the window. Call once, on the UI thread.
pub fn install(window: &MainWindow) {
    let model = Rc::new(VecModel::<SessionView>::default());
    window.set_sessions(ModelRc::from(Rc::clone(&model)));
    MODEL.with(|m| *m.borrow_mut() = Some(model));
}

fn with_model<R>(f: impl FnOnce(&VecModel<SessionView>) -> R) -> Option<R> {
    MODEL.with(|m| m.borrow().as_ref().map(|model| f(model)))
}

/// Updates one row in place. Returns false if the session has no row.
pub fn update(id: &str, f: impl FnOnce(&mut SessionView)) -> bool {
    with_model(|m| {
        for i in 0..m.row_count() {
            if let Some(mut row) = m.row_data(i).filter(|r| r.id == id) {
                f(&mut row);
                m.set_row_data(i, row);
                return true;
            }
        }
        false
    })
    .unwrap_or(false)
}

/// Adds a row for `id` unless one exists.
pub fn ensure(id: &str, title: String, subtitle: String, resolution_mode: i32, audio_quality: i32) {
    with_model(|m| {
        if m.iter().any(|r| r.id == id) {
            return;
        }
        m.push(SessionView {
            id: id.into(),
            title: title.into(),
            subtitle: subtitle.into(),
            connecting: true,
            cursor: 1,
            path: "Relay".into(),
            status: CONNECTING.into(),
            resolution_mode,
            audio_quality,
            ..Default::default()
        });
    });
}

/// Removes the row of `id`.
pub fn remove(id: &str) {
    with_model(|m| {
        if let Some(i) = m.iter().position(|r| r.id == id) {
            m.remove(i);
        }
    });
}

/// Removes every row.
pub fn clear() {
    with_model(|m| m.set_vec(Vec::new()));
}

/// True when no session row exists.
pub fn is_empty() -> bool {
    with_model(|m| m.row_count() == 0).unwrap_or(true)
}
