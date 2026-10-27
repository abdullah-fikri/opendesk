//! Tray icon through the freedesktop StatusNotifierItem protocol (KDE, XFCE, Cinnamon, MATE, and GNOME with the
//! AppIndicator extension that Ubuntu enables by default). Without a tray host OpenDesk keeps running; launching
//! it again shows the window (see `autostart::ensure_single_instance`).

use std::sync::OnceLock;

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, ToolTip};
use parking_lot::Mutex;

use super::TrayAction;
use crate::infra::logger;

/// Handler for tray menu choices.
type TrayHandler = Box<dyn Fn(TrayAction) + Send + Sync>;

static HANDLER: OnceLock<Mutex<Option<TrayHandler>>> = OnceLock::new();

fn dispatch(action: TrayAction) {
    if let Some(lock) = HANDLER.get()
        && let Some(handler) = lock.lock().as_ref()
    {
        handler(action);
    }
}

/// Shows the main window, as a click on the tray icon does (a second launch of the app calls this).
pub fn request_show() {
    dispatch(TrayAction::ShowWindow);
}

/// Slint's `show()` raises the window; nothing to do natively.
pub fn bring_main_window_to_front(_window_title: &str) {}

/// The app icon as ARGB32 (network byte order) at 64x64, the size tray hosts scale from.
fn icon() -> Vec<Icon> {
    let Ok(img) = image::load_from_memory(include_bytes!("../../../icon.png")) else { return Vec::new() };
    let img = image::imageops::resize(&img.to_rgba8(), 64, 64, image::imageops::FilterType::Triangle);
    let data = img.pixels().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
    vec![Icon { width: 64, height: 64, data }]
}

struct OpenDeskTray {
    tooltip: String,
    icon: Vec<Icon>,
}

impl ksni::Tray for OpenDeskTray {
    fn id(&self) -> String {
        "opendesk".into()
    }

    fn title(&self) -> String {
        "OpenDesk".into()
    }

    fn icon_name(&self) -> String {
        "opendesk".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: self.tooltip.clone(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        dispatch(TrayAction::ShowWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let item = |label: &str, action: TrayAction| {
            StandardItem {
                label: label.into(),
                activate: Box::new(move |_: &mut Self| dispatch(action)),
                ..Default::default()
            }
            .into()
        };
        vec![
            item("Open OpenDesk", TrayAction::ShowWindow),
            MenuItem::Separator,
            item("Disconnect Remote Session", TrayAction::DisconnectSession),
            item("Reconnect to Relay", TrayAction::ReconnectRelay),
            item("Restart", TrayAction::RestartApp),
            MenuItem::Separator,
            item("Stop & Exit", TrayAction::ExitApp),
        ]
    }
}

/// The tray icon. Dropping it removes the icon.
pub struct SystemTray {
    handle: Option<Handle<OpenDeskTray>>,
}

impl SystemTray {
    /// Adds the tray icon with `tooltip`; menu choices call `on_action` on the tray thread.
    pub fn new<F>(tooltip: &str, on_action: F) -> Self
    where
        F: Fn(TrayAction) + Send + Sync + 'static,
    {
        *HANDLER.get_or_init(|| Mutex::new(None)).lock() = Some(Box::new(on_action));
        let tray = OpenDeskTray { tooltip: tooltip.to_string(), icon: icon() };
        let handle = match tray.spawn() {
            Ok(h) => Some(h),
            Err(e) => {
                logger::warn(
                    "tray",
                    &format!("No system tray available ({e}); run OpenDesk again to show the window."),
                );
                None
            }
        };
        Self { handle }
    }
}

impl Drop for SystemTray {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.shutdown();
        }
    }
}
