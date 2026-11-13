//! Relay address changes from the UI or the CLI (`opendesk server <url>`), the viewer performance preset and the
//! local microphone default.

use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use core_engine::capture::{enumerate_monitors, refresh_rate};

use crate::domain::edition::Edition;
use crate::domain::performance::Performance;
use crate::domain::settings::normalize_gateway_url;
use crate::domain::{frame_rate, whats_new};
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

/// Whether a session starting now begins with this machine's microphone muted (`mic_on_at_start` in
/// `config.json`, read at every session start so a change applies to the next session).
pub fn mic_muted_at_start() -> bool {
    !storage::load_settings().mic_on_at_start()
}

/// Stores whether the microphone is on when a session starts. Running sessions keep their current state.
pub fn set_mic_on_at_start(on: bool) {
    let mut settings = storage::load_settings();
    if settings.mic_on_at_start == Some(on) {
        return;
    }
    settings.mic_on_at_start = Some(on);
    storage::save_settings(&settings);
    logger::info("settings", &format!("Microphone when a session starts: {}", if on { "on" } else { "off" }));
}

/// "What's new" to show now as (title, body), once per update. Never in the Host edition, which has no window.
/// When there is nothing to show (new install, rollback, no highlights, turned off), the running version is
/// stored as seen right away.
pub fn whats_new(edition: Edition) -> Option<(String, String)> {
    if edition == Edition::Headless {
        return None;
    }
    let current = env!("CARGO_PKG_VERSION");
    let mut settings = storage::load_settings();
    let releases = whats_new::highlights(whats_new::CHANGELOG);
    let shown = whats_new::to_show(&releases, &settings.last_seen_version, current, settings.show_whats_new);
    if shown.is_empty() {
        if settings.last_seen_version != current {
            settings.last_seen_version = current.to_string();
            storage::save_settings(&settings);
        }
        return None;
    }
    Some((format!("What's new in OpenDesk {current}"), whats_new::body(&shown)))
}

/// The user closed "What's new": it is not shown again for this version, or never with `dont_show_again`.
pub fn whats_new_seen(dont_show_again: bool) {
    let mut settings = storage::load_settings();
    settings.last_seen_version = env!("CARGO_PKG_VERSION").to_string();
    if dont_show_again {
        settings.show_whats_new = false;
        logger::info("settings", "What's new after updates: off");
    }
    storage::save_settings(&settings);
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
