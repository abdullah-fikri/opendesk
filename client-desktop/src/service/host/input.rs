//! Applies the hosted viewer's input and control events to this machine.

use std::sync::atomic::Ordering;

use crate::domain::resolution;
use crate::infra::{logger, privacy};
use crate::protocol::InputEvent;
use crate::service::context::AppContext;

const VK_TAB: u8 = 0x09;
const VK_MENU: u8 = 0x12;
const VK_ESCAPE: u8 = 0x1B;
const VK_LWIN: u8 = 0x5B;
/// Highest monitor index a viewer may select.
const MAX_MONITOR_INDEX: u64 = 63;

/// Handles one event from the viewer this machine hosts. Events arriving while nothing is hosted are ignored:
/// only an active session may drive this machine.
pub fn apply(ctx: &AppContext, event: InputEvent) {
    if !ctx.is_hosting() {
        return;
    }
    let host = &ctx.host;
    let injector = &ctx.injector;
    match event {
        InputEvent::MouseMove { norm_x, norm_y } => {
            let (x, y) = to_screen(ctx, norm_x, norm_y);
            injector.set_cursor_pos(x, y);
        }
        InputEvent::MouseDown { norm_x, norm_y, button } => {
            let (x, y) = to_screen(ctx, norm_x, norm_y);
            injector.set_cursor_pos(x, y);
            injector.mouse_down(button);
        }
        InputEvent::MouseUp { norm_x, norm_y, button } => {
            let (x, y) = to_screen(ctx, norm_x, norm_y);
            injector.set_cursor_pos(x, y);
            injector.mouse_up(button);
        }
        InputEvent::Click { norm_x, norm_y, button } => {
            let (x, y) = to_screen(ctx, norm_x, norm_y);
            injector.set_cursor_pos(x, y);
            injector.mouse_down(button);
            injector.mouse_up(button);
        }
        InputEvent::DoubleClick { norm_x, norm_y, button } => {
            let (x, y) = to_screen(ctx, norm_x, norm_y);
            injector.set_cursor_pos(x, y);
            for _ in 0..2 {
                injector.mouse_down(button);
                injector.mouse_up(button);
            }
        }
        InputEvent::Wheel { norm_x, norm_y, delta_y } => {
            let (x, y) = to_screen(ctx, norm_x, norm_y);
            injector.set_cursor_pos(x, y);
            injector.mouse_wheel(delta_y);
        }
        InputEvent::KeyDown { key_code } if key_code > 0 => host.key_down(injector, key_code),
        InputEvent::KeyUp { key_code } if key_code > 0 => host.key_up(injector, key_code),
        InputEvent::KeyCombo { key_code, ctrl, alt, shift } if key_code > 0 => {
            injector.send_key_combo(ctrl, alt, shift, key_code)
        }
        InputEvent::Text { text } => injector.send_unicode_text(&text),
        InputEvent::Clipboard { text } => {
            core_engine::clipboard::set_text(&text);
            ctx.mark_clipboard_synced();
        }
        InputEvent::Shortcut { name } => shortcut(ctx, &name),
        InputEvent::SwitchMonitor => {
            host.monitor_index.fetch_add(1, Ordering::SeqCst);
        }
        InputEvent::SelectMonitor { index } => {
            host.monitor_index.store(index.min(MAX_MONITOR_INDEX) as usize, Ordering::SeqCst)
        }
        InputEvent::SetResolution { mode, auto, width, height } => {
            let value = resolution::host_mode(Some(mode), auto == Some(true), width, height);
            host.resolution_mode.store(value, Ordering::SeqCst);
            logger::info(
                "host.input",
                &format!("Viewer requested resolution: mode={mode} auto={auto:?} {width:?}x{height:?}"),
            );
        }
        InputEvent::SetAudioQuality { quality } => {
            ctx.voice.audio_quality.store(quality.min(2) as usize, Ordering::SeqCst);
            logger::info("host.input", &format!("Viewer set voice quality: {}", quality.min(2)));
        }
        // A remote peer may silence this microphone, but only the local user can enable it.
        InputEvent::SetMic { muted: true } => {
            ctx.voice.is_mic_muted.store(true, Ordering::SeqCst);
            ctx.ui.hosting_mic(true);
        }
        InputEvent::SetPrivacy { mode } => privacy::set_host_privacy_mode(mode.clamp(0, 3)),
        InputEvent::SetBlockInput { blocked } => privacy::set_host_block_input(blocked),
        InputEvent::VideoKeyframe => host.keyframe_requested.store(true, Ordering::SeqCst),
        // Viewer-bound events and zero key codes have no meaning on a host.
        _ => {}
    }
}

/// Maps normalized coordinates onto the streamed monitor.
fn to_screen(ctx: &AppContext, norm_x: f64, norm_y: f64) -> (i32, i32) {
    let mon = *ctx.host.monitor_bounds.lock().unwrap();
    let x = mon.left + (norm_x.clamp(0.0, 1.0) * mon.width as f64).round() as i32;
    let y = mon.top + (norm_y.clamp(0.0, 1.0) * mon.height as f64).round() as i32;
    (x, y)
}

/// System shortcuts the viewer toolbar offers.
fn shortcut(ctx: &AppContext, name: &str) {
    let injector = &ctx.injector;
    let with_win = |key: u8| {
        injector.key_down(VK_LWIN);
        injector.key_tap(key);
        injector.key_up(VK_LWIN);
    };
    match name {
        "taskmgr" => injector.send_key_combo(true, false, true, VK_ESCAPE), // Ctrl+Shift+Esc
        "win" => injector.key_tap(VK_LWIN),
        "explorer" => with_win(b'E'),
        "desktop" => with_win(b'D'),
        "alt-tab" => {
            injector.key_down(VK_MENU);
            injector.key_tap(VK_TAB);
            injector.key_up(VK_MENU);
        }
        _ => {}
    }
}
