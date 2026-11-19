//! The frameless main window (`no-frame` in `app_window.slint`): the app draws its own minimize, maximize and
//! close buttons, moves the window from empty bar space and resizes it from thin edges. Moving and resizing
//! hand over to the windowing system through winit, so snapping and multi-monitor moves behave natively.

use std::cell::Cell;

use image::imageops::FilterType;
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit::window::ResizeDirection;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};

use super::{Brand, MainWindow, WindowFrame};

/// The logo shown in the sidebars and top bars (light- and dark-theme variants), and its edge in logical
/// pixels there.
const LOGO_LIGHT_PNG: &[u8] = include_bytes!("../../ui/logo-light.png");
const LOGO_DARK_PNG: &[u8] = include_bytes!("../../ui/logo-dark.png");
const LOGO_EDGE: f32 = 28.0;

thread_local! {
    /// Physical edge the logo was last scaled to (0 = not yet).
    static LOGO_PX: Cell<u32> = const { Cell::new(0) };
}

/// Scales the logos down to the exact physical size of their slot with a Lanczos filter: the GPU renderer would
/// shrink the 512 px originals without smoothing, which looks jagged. Runs again when the scale factor changes.
fn refresh_logo(win: &MainWindow) {
    let px = (LOGO_EDGE * win.window().scale_factor()).round().max(1.0) as u32;
    if LOGO_PX.with(|c| c.replace(px)) == px {
        return;
    }
    let brand = win.global::<Brand>();
    if let Some(img) = scaled_logo(LOGO_LIGHT_PNG, px) {
        brand.set_logo_light(img);
    }
    if let Some(img) = scaled_logo(LOGO_DARK_PNG, px) {
        brand.set_logo_dark(img);
    }
}

fn scaled_logo(png: &[u8], px: u32) -> Option<Image> {
    let original = image::load_from_memory(png).ok()?;
    let small = image::imageops::resize(&original.to_rgba8(), px, px, FilterType::Lanczos3);
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(small.as_raw(), px, px);
    Some(Image::from_rgba8(buffer))
}

/// Binds the `WindowFrame` callbacks of `win`.
pub fn bind(win: &MainWindow) {
    refresh_logo(win);
    let frame = win.global::<WindowFrame>();

    let weak = win.as_weak();
    frame.on_minimize(move || {
        if let Some(w) = weak.upgrade() {
            w.window().set_minimized(true);
        }
    });

    let weak = win.as_weak();
    frame.on_toggle_maximize(move || {
        if let Some(w) = weak.upgrade() {
            let maximized = !w.window().is_maximized();
            w.window().set_maximized(maximized);
            w.global::<WindowFrame>().set_maximized(maximized);
        }
    });

    // Same path as the system close button: the close handler keeps OpenDesk running in the tray.
    let weak = win.as_weak();
    frame.on_close(move || {
        if let Some(w) = weak.upgrade() {
            w.window().dispatch_event(slint::platform::WindowEvent::CloseRequested);
        }
    });

    let weak = win.as_weak();
    frame.on_drag(move || {
        if let Some(w) = weak.upgrade() {
            w.window().with_winit_window(|ww| {
                let _ = ww.drag_window();
            });
        }
    });

    let weak = win.as_weak();
    frame.on_resize(move |edge| {
        let Some(direction) = resize_direction(edge) else { return };
        if let Some(w) = weak.upgrade() {
            w.window().with_winit_window(|ww| {
                let _ = ww.drag_resize_window(direction);
            });
        }
    });

    let weak = win.as_weak();
    frame.on_state_changed(move || {
        if let Some(w) = weak.upgrade() {
            refresh_logo(&w);
            let maximized = w.window().is_maximized();
            let frame = w.global::<WindowFrame>();
            if frame.get_maximized() != maximized {
                frame.set_maximized(maximized);
            }
        }
    });
}

/// Edge numbers of `ResizeEdges` in `window_frame.slint`.
fn resize_direction(edge: i32) -> Option<ResizeDirection> {
    Some(match edge {
        0 => ResizeDirection::East,
        1 => ResizeDirection::North,
        2 => ResizeDirection::NorthEast,
        3 => ResizeDirection::NorthWest,
        4 => ResizeDirection::South,
        5 => ResizeDirection::SouthEast,
        6 => ResizeDirection::SouthWest,
        7 => ResizeDirection::West,
        _ => return None,
    })
}
