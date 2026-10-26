//! Clipboard follows the current partner: text copied here is sent to it, and copied files are offered (or,
//! to partners without offers, sent when this machine is the host). Copies of files we just received are
//! recognized by provenance and never sent back.

use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use crate::infra::logger;
use crate::protocol::ClientMessage;
use crate::service::context::Ctx;
use crate::service::file_transfer::{self, offer, sender};

/// Clipboard polling period.
const POLL: Duration = Duration::from_millis(350);

/// Starts the clipboard watcher thread.
pub fn spawn(ctx: Ctx) {
    thread::spawn(move || {
        loop {
            thread::sleep(POLL);
            let partner = ctx.partner();
            if partner.is_empty() {
                ctx.mark_clipboard_synced();
                continue;
            }
            let current = core_engine::clipboard::sequence_number();
            if current == 0 || current == ctx.clipboard_seq.load(Ordering::SeqCst) {
                continue;
            }
            ctx.clipboard_seq.store(current, Ordering::SeqCst);
            let viewed = ctx.viewers.get(&partner);
            let mark_seen = || {
                if let Some(s) = &viewed {
                    s.clip_seq.store(current, Ordering::SeqCst);
                }
            };

            // A viewer sends its files explicitly on Ctrl+V, so files are only auto-sent from the host side;
            // partners with offers get names and sizes now and pull on paste.
            let offers = file_transfer::caps_of(&partner).file_offer;
            if offers || ctx.is_hosting() {
                let files = core_engine::clipboard::files();
                if !files.is_empty() {
                    if !file_transfer::is_received_clipboard(&files) {
                        if offers {
                            mark_seen();
                            if let Some(s) = &viewed {
                                s.offered_seq.store(current, Ordering::SeqCst);
                            }
                            offer::offer_files(&ctx, files, partner);
                        } else {
                            logger::info(
                                "clipboard",
                                &format!("Host copied {} files; sending them to the viewer.", files.len()),
                            );
                            sender::send_files(&ctx, files, partner, false);
                        }
                    }
                    continue;
                }
            }
            if let Some(text) = core_engine::clipboard::text().filter(|t| !t.is_empty()) {
                mark_seen();
                ctx.send(ClientMessage::RelayClipboard { target_clean: partner, text });
            }
        }
    });
}
