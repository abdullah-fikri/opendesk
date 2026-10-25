//! The `monitor_tiles` model of the main window (admin edition): one row per tile of the visible page, updated
//! in place so a tile keeps its picture across state changes. UI thread only (Slint models are not `Send`).

use std::cell::RefCell;
use std::rc::Rc;

use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use super::{MainWindow, MonitorPick, MonitorTile};
use crate::service::ui_port::{WallPick, WallTile};

thread_local! {
    static TILES: RefCell<Option<Rc<VecModel<MonitorTile>>>> = const { RefCell::new(None) };
}

/// Installs an empty model on the window. Call once, on the UI thread.
pub fn install(window: &MainWindow) {
    let model = Rc::new(VecModel::<MonitorTile>::default());
    window.set_monitor_tiles(ModelRc::from(Rc::clone(&model)));
    TILES.with(|m| *m.borrow_mut() = Some(model));
}

fn with_model(f: impl FnOnce(&VecModel<MonitorTile>)) {
    TILES.with(|m| {
        if let Some(model) = m.borrow().as_ref() {
            f(model);
        }
    });
}

/// Shows the tiles of the visible page; a host that stays on the page keeps its last picture.
pub fn set_tiles(tiles: Vec<WallTile>) {
    with_model(|m| {
        let rows: Vec<MonitorTile> = tiles
            .into_iter()
            .map(|t| {
                let old = m.iter().find(|r| r.id == t.id.as_str());
                MonitorTile {
                    id: t.id.into(),
                    title: t.title.into(),
                    subtitle: t.subtitle.into(),
                    state: t.state as i32,
                    has_frame: old.as_ref().is_some_and(|r| r.has_frame),
                    frame: old.map(|r| r.frame).unwrap_or_default(),
                }
            })
            .collect();
        if rows.len() == m.row_count() {
            for (i, row) in rows.into_iter().enumerate() {
                m.set_row_data(i, row);
            }
        } else {
            m.set_vec(rows);
        }
    });
}

/// New pictures for tiles on the page; others are ignored.
pub fn set_frames(frames: Vec<(String, SharedPixelBuffer<Rgba8Pixel>)>) {
    with_model(|m| {
        for (id, pixels) in frames {
            if let Some(i) = m.iter().position(|r| r.id == id.as_str())
                && let Some(mut row) = m.row_data(i)
            {
                row.frame = Image::from_rgba8(pixels);
                row.has_frame = true;
                m.set_row_data(i, row);
            }
        }
    });
}

/// Rows of the device picker.
pub fn picks(picks: Vec<WallPick>) -> ModelRc<MonitorPick> {
    let items: Vec<MonitorPick> = picks
        .into_iter()
        .map(|p| MonitorPick { id: p.id.into(), name: p.name.into(), online: p.online, selected: p.selected })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(items)))
}
