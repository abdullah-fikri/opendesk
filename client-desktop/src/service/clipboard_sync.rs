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
                    let received = file_transfer::is_received_clipboard(&files);
                    let action = files_action(last_files.as_ref(), &files, received, offers, Instant::now());
                    let mark_offered = || {
                        mark_seen();
                        if let Some(s) = &viewed {
                            s.offered_seq.store(current, Ordering::SeqCst);
                        }
                    };
                    match action {
                        FilesAction::Offer => {
                            last_files = Some((files.clone(), Instant::now()));
                            mark_offered();
                            offer::offer_files(&ctx, files, partner);
                        }
                        FilesAction::AlreadyOffered => mark_offered(),
                        FilesAction::Send => {
                            last_files = Some((files.clone(), Instant::now()));
                            logger::info(
                                "clipboard",
                                &format!("Host copied {} files; sending them to the viewer.", files.len()),
                            );
                            sender::send_files(&ctx, files, partner, false);
                        }
                        FilesAction::Skip => {}
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

/// What the watcher does with files it found on a changed clipboard.
#[derive(Debug, PartialEq)]
enum FilesAction {
    /// A new copy for a partner with offers: send names and sizes.
    Offer,
    /// The copy just offered, seen again under a newer sequence number: the partner already has the offer, so
    /// only that number is recorded. Without it Ctrl+V in the viewer takes the copy for one the host never got
    /// and sends the files instead of pasting the offer (auto-pasting each top-level file on its own).
    AlreadyOffered,
    /// A new copy for a partner without offers (we host): send the files.
    Send,
    /// Files we just received, or the copy just sent: nothing to do.
    Skip,
}

/// Decides [`FilesAction`] for `files` given the last copy acted on (`last`, with when).
fn files_action(
    last: Option<&(Vec<PathBuf>, Instant)>,
    files: &[PathBuf],
    received: bool,
    offers: bool,
    now: Instant,
) -> FilesAction {
    if received {
        return FilesAction::Skip;
    }
    let repeat = last.is_some_and(|(f, at)| f.as_slice() == files && now.duration_since(*at) < SAME_COPY);
    match (repeat, offers) {
        (false, true) => FilesAction::Offer,
        (false, false) => FilesAction::Send,
        (true, true) => FilesAction::AlreadyOffered,
        (true, false) => FilesAction::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_seen_twice_is_offered_once_and_recorded_both_times() {
        let t0 = Instant::now();
        let files = vec![PathBuf::from(r"C:\a.txt"), PathBuf::from(r"C:\b.txt")];
        assert_eq!(files_action(None, &files, false, true, t0), FilesAction::Offer);
        let last = (files.clone(), t0);
        // Explorer changed the clipboard again for the same copy: the new sequence number must still count as
        // offered, or Ctrl+V sends the files instead of pasting the offer.
        let again = t0 + Duration::from_millis(400);
        assert_eq!(files_action(Some(&last), &files, false, true, again), FilesAction::AlreadyOffered);
        assert_eq!(files_action(Some(&last), &files, false, false, again), FilesAction::Skip);
        // The same files copied again later are a new copy.
        assert_eq!(files_action(Some(&last), &files, false, true, t0 + SAME_COPY), FilesAction::Offer);
        assert_eq!(files_action(Some(&last), &files[..1], false, true, again), FilesAction::Offer);
        assert_eq!(files_action(None, &files, false, false, t0), FilesAction::Send);
        assert_eq!(files_action(None, &files, true, true, t0), FilesAction::Skip);
    }
}
