//! Composition root: builds the shared context, starts the background services and, unless this is the
//! headless edition, the window and tray. Wiring only; no behavior lives here.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use slint::ComponentHandle;

use crate::domain::edition::Edition;
use crate::infra::audio::VoiceEngine;
use crate::infra::{autostart, logger, privacy, storage};
use crate::protocol::Outgoing;
use crate::service::context::{AppContext, Ctx};
use crate::service::history::History;
use crate::service::ui_port::{NoUi, UiPort};
use crate::service::{clipboard_sync, relay, settings};
use crate::ui::{self, MainWindow, SlintUi};

/// Command-line options of the app itself (CLI commands are handled earlier by `cli`).
struct Options {
    /// `--background` / `--minimized`: start hidden in the tray (autostart, watchdog).
    background: bool,
    /// `--id=<desk id>`: run with another desk ID, e.g. a second instance for local testing.
    desk_id: Option<String>,
}

impl Options {
    fn parse() -> Self {
        let mut opts = Options { background: false, desk_id: None };
        for arg in std::env::args().skip(1) {
            if let Some(id) = arg.strip_prefix("--id=") {
                opts.desk_id = Some(id.to_string());
            } else if arg == "--background" || arg == "--minimized" {
                opts.background = true;
            }
        }
        opts
    }
}

/// Runs OpenDesk until the process exits.
pub fn run() -> Result<(), slint::PlatformError> {
    let edition = Edition::current();
    let opts = Options::parse();
    core_engine::capture::enable_per_monitor_dpi_awareness();
    let mut settings = storage::load_settings();
    if let Some(id) = opts.desk_id {
        settings.desk_id = id;
    }

    // A second launch wakes the running instance and exits.
    autostart::ensure_single_instance(opts.background || edition == Edition::Headless);
    logger::info("app", &format!("OpenDesk {} starting (edition: {}).", env!("CARGO_PKG_VERSION"), edition.name()));
    autostart::init_background_guardian();
    if let core_engine::capture::HostSupport::Unsupported(reason) = core_engine::capture::host_support() {
        logger::warn("app", &format!("Viewer only, this device cannot be controlled: {reason}"));
    }

    let (outbox, outbox_rx) = mpsc::channel::<Outgoing>();
    let voice = VoiceEngine::new(outbox.clone());
    privacy::init_privacy_and_hooks(outbox.clone());
    let build = |ui: Arc<dyn UiPort>| -> Ctx {
        Arc::new(AppContext::new(
            edition,
            settings.desk_id.clone(),
            settings.gateway_url.clone(),
            outbox.clone(),
            ui,
            Arc::clone(&voice),
        ))
    };

    if edition == Edition::Headless {
        // Incoming sessions only: no window, taskbar entry or tray icon.
        let ctx = build(Arc::new(NoUi));
        logger::info("app", &format!("Headless host. Desk ID: {} | Relay: {}", ctx.local_id, settings.gateway_url));
        start_services(&ctx, outbox_rx);
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    let window = MainWindow::new()?;
    let ctx = build(Arc::new(SlintUi::new(&window)));
    let history = Arc::new(History::load());
    ui::bind(&window, &ctx, &history, &settings.gateway_url);
    let _tray = ui::install_tray(&window, &ctx);
    ui::enable_file_drop(&window, &ctx, opts.background);
    start_services(&ctx, outbox_rx);
    run_window(&window, opts.background)
}

/// Windows: the window is always "shown" for Slint and hidden natively when it goes to the tray.
#[cfg(windows)]
fn run_window(window: &MainWindow, _background: bool) -> Result<(), slint::PlatformError> {
    window.run()
}

/// Elsewhere the window is really hidden when it goes to the tray, so the event loop must outlive it; a
/// background start (autostart) does not show it at all.
#[cfg(not(windows))]
fn run_window(window: &MainWindow, background: bool) -> Result<(), slint::PlatformError> {
    if !background {
        window.show()?;
    }
    slint::run_event_loop_until_quit()
}

fn start_services(ctx: &Ctx, outbox: mpsc::Receiver<Outgoing>) {
    relay::spawn(Arc::clone(ctx), outbox);
    clipboard_sync::spawn(Arc::clone(ctx));
    settings::spawn_watcher(Arc::clone(ctx));
    // Tells the service (when it runs this agent) about sessions and the relay, for auto-update.
    #[cfg(windows)]
    crate::service::update::agent::spawn(Arc::clone(ctx));
}
