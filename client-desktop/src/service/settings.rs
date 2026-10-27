//! Relay address changes from the UI or the CLI (`opendesk server <url>`).

use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use crate::domain::settings::normalize_gateway_url;
use crate::infra::{logger, storage};
use crate::service::context::{AppContext, Ctx};

/// How often `config.json` is checked for changes made by the CLI.
const WATCH_INTERVAL: Duration = Duration::from_millis(1400);

/// Saves a new relay address and reconnects.
pub fn set_gateway(ctx: &AppContext, raw_url: &str) {
    let url = normalize_gateway_url(raw_url);
    let mut settings = storage::load_settings();
    settings.gateway_url = url.clone();
    storage::save_settings(&settings);
    apply_gateway(ctx, url);
}

/// Stores the token the relay issued when it enrolled this device; it is presented to that relay only.
pub fn store_device_token(ctx: &AppContext, token: &str) {
    if token.is_empty() {
        return;
    }
    let mut settings = storage::load_settings();
    settings.device_token = token.to_string();
    settings.device_token_relay = ctx.relay.gateway_url.lock().clone();
    storage::save_settings(&settings);
    logger::info("settings", "The relay enrolled this device; its token is stored.");
}

fn apply_gateway(ctx: &AppContext, url: String) {
    *ctx.relay.gateway_url.lock() = url.clone();
    ctx.ui.gateway_changed(url);
    ctx.relay.reconnect.store(true, Ordering::SeqCst);
}

/// Watches `config.json` so a CLI change reaches the running app within about a second.
pub fn spawn_watcher(ctx: Ctx) {
    crate::infra::thread::spawn_named("settings-watch", move || {
        loop {
            thread::sleep(WATCH_INTERVAL);
            let on_disk = storage::load_settings().gateway_url;
            let current = ctx.relay.gateway_url.lock().clone();
            if !on_disk.is_empty() && on_disk != current {
                logger::info("settings", &format!("Gateway changed on disk: {current} -> {on_disk}"));
                apply_gateway(&ctx, on_disk);
            }
        }
    });
}
