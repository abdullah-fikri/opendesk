//! Use cases that span both roles: ending everything (relay told us, or the connection dropped), the tray's
//! "Disconnect", and ending the hosted session from the window.

use std::sync::atomic::Ordering;

use crate::infra::{privacy, virtual_clipboard};
use crate::protocol::ClientMessage;
use crate::service::context::AppContext;
use crate::service::host;
use crate::service::viewer::controller;

/// Ends every session locally without telling the relay (it ended them, or the connection is gone).
pub fn end_all(ctx: &AppContext) {
    controller::end_all(ctx);
    virtual_clipboard::clear_offer(&ctx.partner());
    host::stop(ctx);
    privacy::set_viewer_hook_state(false, "");
    ctx.voice.is_session_active.store(false, Ordering::SeqCst);
    ctx.voice.is_mic_muted.store(true, Ordering::SeqCst);
    ctx.set_partner("");
    ctx.ui.hosting_ended();
}

/// Ends every session and tells the relay (tray "Disconnect", exit). Returns how many partners were told.
pub fn disconnect_all(ctx: &AppContext) -> usize {
    let mut targets: Vec<String> = ctx.viewers.all().iter().map(|s| s.id.clone()).collect();
    let partner = ctx.partner();
    if !partner.is_empty() && !targets.contains(&partner) {
        targets.push(partner);
    }
    for target in &targets {
        ctx.send(ClientMessage::disconnect(target));
    }
    end_all(ctx);
    targets.len()
}

/// Ends the hosted session at the local user's request. Viewer sessions of this machine stay open.
pub fn end_hosting(ctx: &AppContext) {
    let partner = ctx.partner();
    virtual_clipboard::clear_offer(&partner);
    host::stop(ctx);
    if !partner.is_empty() {
        ctx.send(ClientMessage::disconnect(&partner));
    }
    if ctx.viewers.is_empty() {
        privacy::set_viewer_hook_state(false, "");
        ctx.voice.is_session_active.store(false, Ordering::SeqCst);
        ctx.voice.is_mic_muted.store(true, Ordering::SeqCst);
        ctx.set_partner("");
    } else {
        // Input target, keyboard hook and voice go back to the focused viewer session.
        controller::apply_focus(ctx);
    }
    ctx.ui.hosting_ended();
}
