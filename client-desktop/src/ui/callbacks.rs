//! Binds every `MainWindow` callback to a use case. Callbacks only translate widget arguments; decisions
//! live in the service layer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, Timer, TimerMode};

use super::MainWindow;
use crate::domain::desk_id;
use crate::domain::keyboard::Mods;
use crate::protocol::ClientMessage;
use crate::service::context::Ctx;
use crate::service::history::History;
use crate::service::viewer::controller::{self, Mouse};
use crate::service::{host, sessions, settings};

/// Resizes reach the host this long after resizing stops: each one recreates its capturer and costs a key frame.
const VIEWPORT_DEBOUNCE: Duration = Duration::from_millis(300);

thread_local! {
    /// Debounce timer per session for viewport reports.
    static VIEWPORT_TIMERS: RefCell<HashMap<String, Rc<Timer>>> = RefCell::new(HashMap::new());
}

/// Registers a callback that receives the window and the context.
macro_rules! on {
    ($win:expr, $ctx:expr, $setter:ident, |$w:ident, $c:ident $(, $arg:ident)*| $body:expr) => {{
        let weak = $win.as_weak();
        let ctx = Arc::clone($ctx);
        $win.$setter(move |$($arg),*| {
            if let Some($w) = weak.upgrade() {
                let $c = &ctx;
                $body
            }
        });
    }};
}

/// Wires the dashboard, hosting and viewer-session callbacks.
pub fn bind(win: &MainWindow, ctx: &Ctx, history: &Arc<History>) {
    bind_dashboard(win, ctx, history);
    bind_hosting(win, ctx);
    bind_sessions(win, ctx);
    #[cfg(feature = "admin")]
    bind_monitor_wall(win, ctx);
}

/// Admin edition only: other editions leave these callbacks unbound and never show the wall.
#[cfg(feature = "admin")]
fn bind_monitor_wall(win: &MainWindow, ctx: &Ctx) {
    use crate::service::monitor_wall as wall;
    on!(win, ctx, on_open_monitor, |_w, c| wall::open(c));
    on!(win, ctx, on_close_monitor, |_w, c| wall::close(c));
    on!(win, ctx, on_monitor_step_page, |_w, c, delta| wall::step_page(c, delta));
    on!(win, ctx, on_monitor_set_grid, |_w, c, grid| wall::set_grid(c, grid));
    on!(win, ctx, on_monitor_set_rotation, |_w, c, on, secs| wall::set_rotation(c, on, secs));
    on!(win, ctx, on_monitor_set_all, |_w, c, all| wall::set_all(c, all));
    on!(win, ctx, on_monitor_toggle_host, |_w, c, id| wall::toggle_host(c, &id));
}

fn bind_dashboard(win: &MainWindow, ctx: &Ctx, history: &Arc<History>) {
    let hist = Arc::clone(history);
    on!(win, ctx, on_request_connect, |_w, c, target| {
        let clean = desk_id::clean(&target);
        c.ui.history(hist.touch(&clean));
        let alias = hist.alias_of(&clean);
        let formatted = desk_id::format(&clean);
        let (title, subtitle) = if alias.is_empty() { (formatted, String::new()) } else { (alias, formatted) };
        controller::request_connect(c, &clean, title, subtitle);
    });
    on!(win, ctx, on_copy_local_id, |w, c| {
        core_engine::clipboard::set_text(&desk_id::format(&c.local_id));
        c.mark_clipboard_synced();
        w.set_status_text("ID copied".into());
    });
    on!(win, ctx, on_refresh_hosts, |_w, c| c.send(ClientMessage::GetHosts));
    on!(win, ctx, on_save_gateway_url, |_w, c, url| settings::set_gateway(c, &url));
    on!(win, ctx, on_open_rename, |w, _c, id, alias| {
        w.set_rename_target_id(id);
        w.set_rename_input_val(alias);
        w.set_show_rename_modal(true);
    });
    let hist = Arc::clone(history);
    on!(win, ctx, on_save_rename, |_w, c, id, alias| c.ui.history(hist.rename(&id, &alias)));
    let hist = Arc::clone(history);
    on!(win, ctx, on_delete_history, |_w, c, id| c.ui.history(hist.remove(&id)));
}

fn bind_hosting(win: &MainWindow, ctx: &Ctx) {
    on!(win, ctx, on_hosting_send_files, |_w, c| controller::pick_and_send_files(c, &c.partner()));
    on!(win, ctx, on_hosting_toggle_mic, |_w, c| host::toggle_mic(c));
    on!(win, ctx, on_end_hosting, |_w, c| sessions::end_hosting(c));
}

fn bind_sessions(win: &MainWindow, ctx: &Ctx) {
    on!(win, ctx, on_focus_session, |w, c, id| {
        controller::focus(c, &id);
        w.set_show_new_session(false);
    });
    on!(win, ctx, on_close_session, |_w, c, id| {
        VIEWPORT_TIMERS.with(|t| t.borrow_mut().remove(id.as_str()));
        controller::close(c, &id, None);
    });
    on!(win, ctx, on_set_view_layout, |_w, c, layout| controller::set_layout(c, layout));
    on!(win, ctx, on_new_session_page, |w, _c| w.set_show_new_session(true));

    on!(win, ctx, on_session_mouse_move, |_w, c, id, x, y| controller::mouse(c, &id, Mouse::Move, x, y));
    on!(win, ctx, on_session_mouse_down, |_w, c, id, x, y, b| controller::mouse(c, &id, Mouse::Down(b), x, y));
    on!(win, ctx, on_session_mouse_up, |_w, c, id, x, y, b| controller::mouse(c, &id, Mouse::Up(b), x, y));
    on!(win, ctx, on_session_mouse_wheel, |_w, c, id, x, y, d| controller::mouse(c, &id, Mouse::Wheel(d), x, y));
    on!(win, ctx, on_session_key_event, |_w, c, id, text, pressed, repeat, ctrl, alt, shift| {
        controller::key_event(c, &id, &text, pressed, repeat, Mods { ctrl, alt, shift });
    });
    on!(win, ctx, on_session_release_keys, |_w, c, id| controller::release_keys(c, &id));
    on!(win, ctx, on_session_viewport_resized, |_w, c, id, width, height| {
        controller::viewport_changed(c, &id, width, height);
        let timer = VIEWPORT_TIMERS
            .with(|t| Rc::clone(t.borrow_mut().entry(id.to_string()).or_insert_with(|| Rc::new(Timer::default()))));
        let (ctx, id) = (Arc::clone(c), id.to_string());
        timer.start(TimerMode::SingleShot, VIEWPORT_DEBOUNCE, move || controller::report_viewport(&ctx, &id));
    });

    // Actions on a connected session only.
    on!(win, ctx, on_select_monitor, |_w, c, id, index| {
        if c.viewers.is_live(&id) {
            controller::select_monitor(c, &id, index);
        }
    });
    on!(win, ctx, on_session_send_files, |_w, c, id| {
        if c.viewers.is_live(&id) {
            controller::pick_and_send_files(c, &id);
        }
    });
    on!(win, ctx, on_session_toggle_mic, |_w, c, id| {
        if c.viewers.is_live(&id) {
            controller::toggle_mic(c, &id);
        }
    });
    on!(win, ctx, on_session_set_privacy, |_w, c, id, mode| {
        if c.viewers.is_live(&id) {
            controller::set_privacy(c, &id, mode);
        }
    });
    on!(win, ctx, on_session_toggle_block_input, |_w, c, id| {
        if c.viewers.is_live(&id) {
            controller::toggle_block_input(c, &id);
        }
    });
    on!(win, ctx, on_session_shortcut, |_w, c, id, name| {
        if c.viewers.is_live(&id) {
            controller::shortcut(c, &id, &name);
        }
    });
    // Settings may change while a session connects; they are sent once it is up.
    on!(win, ctx, on_session_set_resolution, |_w, c, id, mode| controller::set_resolution(c, &id, mode));
    on!(win, ctx, on_session_set_audio_quality, |_w, c, id, quality| controller::set_audio_quality(c, &id, quality));
}
