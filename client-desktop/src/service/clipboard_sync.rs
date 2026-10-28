//! Clipboard follows the current partner: text copied here is sent to it, and copied files are offered (or,
//! to partners without offers, sent when this machine is the host). Copies of files we just received are
//! recognized by provenance and never sent back.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::infra::{logger, user_clipboard};
use crate::protocol::ClientMessage;
use crate::service::context::Ctx;
use crate::service::file_transfer::{self, offer, sender};

/// Clipboard polling period.
const POLL: Duration = Duration::from_millis(350);
/// One copy in Explorer changes the clipboard several times within a second; the same files copied again within
/// this window are not sent again.
const SAME_COPY: Duration = Duration::from_secs(2);
/// Set once an unreadable clipboard was logged.
static UNREADABLE_LOGGED: AtomicBool = AtomicBool::new(false);

/// Starts the clipboard watcher thread.
pub fn spawn(ctx: Ctx) {
    crate::infra::thread::spawn_named("clipboard-watch", move || {
        let mut last_files: Option<(Vec<PathBuf>, Instant)> = None;
        loop {
            thread::sleep(POLL);
            let partner = ctx.partner();
            if partner.is_empty() {
                ctx.mark_clipboard_synced();
                continue;
            }
            let current = core_engine::clipboard::sequence_number();
            if cfg!(windows) && current == 0 && !UNREADABLE_LOGGED.swap(true, Ordering::Relaxed) {
                // Windows answers 0 to a process without clipboard access (elsewhere 0 means empty): nothing
                // copied here is ever sent.
                logger::warn("clipboard", "This process cannot see the clipboard; copies here are not sent.");
            }
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
            let clip = user_clipboard::read();
            let offers = file_transfer::caps_of(&partner).file_offer;
            if offers || ctx.is_hosting() {
                let files = clip.files;
                if !files.is_empty() {
                    let repeat = last_files.as_ref().is_some_and(|(f, at)| *f == files && at.elapsed() < SAME_COPY);
                    if !repeat && !file_transfer::is_received_clipboard(&files) {
                        last_files = Some((files.clone(), Instant::now()));
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
            // Lengths only: the clipboard may hold passwords.
            match clip.text {
                Some(text) => {
                    mark_seen();
                    logger::info("clipboard", &format!("Sending copied text ({} chars) to {partner}.", text.len()));
                    ctx.send(ClientMessage::RelayClipboard { target_clean: partner, text });
                }
                None => logger::info("clipboard", "Clipboard changed, but it holds no text to send."),
            }
        }
    });
}
