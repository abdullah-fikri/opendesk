//! Relay address changes from the UI or the CLI (`opendesk server <url>`), and the viewer performance preset.

use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use core_engine::capture::{enumerate_monitors, refresh_rate};

use crate::domain::frame_rate;
use crate::domain::performance::Performance;
use crate::domain::settings::normalize_gateway_url;
use crate::infra::{logger, storage};
use crate::protocol::InputEvent;
use crate::service::context::{AppContext, Ctx};

/// How often `config.json` is checked for changes made by the CLI.
const WATCH_INTERVAL: Duration = Duration::from_millis(1400);

/// Saves a new relay address and reconnects; the same address again keeps the connection (and its sessions).
pub fn set_gateway(ctx: &AppContext, raw_url: &str) {
    let url = normalize_gateway_url(raw_url);
    let mut settings = storage::load_settings();
    if settings.gateway_url == url && *ctx.relay.gateway_url.lock() == url {
        return;
    }
    settings.gateway_url = url.clone();
    storage::save_settings(&settings);
    apply_gateway(ctx, url);
}

/// Stores the viewer performance preset and applies it at once: every viewed host gets `set-performance`.
/// Hosts before presets only read the registration caps (`fps-N`), so without a session the connection
/// re-registers; with one, they keep the old cap until the next connection.
pub fn set_performance(ctx: &AppContext, preset: Performance) {
    let mut settings = storage::load_settings();
    if settings.performance == Some(preset) {
        return;
    }
    settings.set_performance(preset);
    storage::save_settings(&settings);
    logger::info("settings", &format!("Performance preset: {}", preset.cap()));
    let sessions = ctx.viewers.all();
    if sessions.is_empty() && !ctx.is_hosting() {
        ctx.relay.reconnect.store(true, Ordering::SeqCst);
        return;
    }
    let event = performance_event(preset);
    for s in sessions.iter().filter(|s| s.is_connected()) {
        ctx.send_input(&s.id, event.clone());
    }
}

/// The `set-performance` event for `preset`: the preset and the frame-rate cap it stands for on this machine.
pub fn performance_event(preset: Performance) -> InputEvent {
    let fps = viewer_fps_cap(preset);
    InputEvent::SetPerformance { preset, max_fps: fps }
}

/// The frame-rate cap this viewer advertises for `preset`: its fixed rate, or for High the fastest monitor's
/// refresh rate, lowered by `OPENDESK_MAX_FPS`.
pub fn viewer_fps_cap(preset: Performance) -> u32 {
    let monitor_hz = enumerate_monitors().into_iter().map(refresh_rate).max().unwrap_or(frame_rate::DEFAULT_FPS);
    let env = frame_rate::env_cap(std::env::var(frame_rate::MAX_FPS_ENV).ok().as_deref());
    frame_rate::viewer_cap(preset.fps_setting(), monitor_hz, env)
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
