//! Clipboard follows the current partner: text copied here is sent to it, and copied files are offered (or,
//! to partners without offers, sent when this machine is the host). Items a partner sent us are recognized by
//! provenance (`domain::received_files`) and never sent back to it.

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
        // Files on the clipboard at the previous look in this session, and the partner it belongs to.
        let mut seen: (String, Option<Vec<PathBuf>>) = (String::new(), None);
        loop {
            thread::sleep(POLL);
            let partner = ctx.partner();
            if partner.is_empty() {
                ctx.mark_clipboard_synced();
                continue;
            }
            if seen.0 != partner {
                // What was copied before the session is not a copy for it: a host that sends on copy only
                // sends files that differ from this (read once, before any change).
                let baseline = ctx.is_hosting().then(|| user_clipboard::read().files);
                seen = (partner.clone(), baseline);
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
            let mut clip = user_clipboard::read();
            // Other programs (clipboard managers, an app gaining focus) touch the clipboard without copying
            // anything new; the files are then the same as at the previous look.
            let unchanged = seen.1.as_ref() == Some(&clip.files);
            seen.1 = Some(clip.files.clone());
            let offers = file_transfer::caps_of(&partner).file_offer;
            if offers || ctx.is_hosting() {
                let copied = std::mem::take(&mut clip.files);
                if !copied.is_empty() {
                    // Items the partner sent us stay out; only the rest is offered or sent.
                    let files = file_transfer::without_echoes(&partner, copied.clone());
                    let received = files.is_empty();
                    let files = if received { copied } else { files };
                    let look = Look { received, offers, unchanged };
                    let action = files_action(last_files.as_ref(), &files, look, Instant::now());
                    let mark_offered = || {
                        mark_seen();
                        if let Some(s) = &viewed {
                            s.offered_seq.store(current, Ordering::SeqCst);
                        }
                    };
                    let hosting = ctx.is_hosting();
                    let log = |decision: &str, reason: &str| {
                        file_transfer::log_clipboard_files(&partner, &files, decision, reason, hosting)
                    };
                    match action {
                        FilesAction::Offer => {
                            log("offer", "new copy; the partner takes offers");
                            last_files = Some((files.clone(), Instant::now()));
                            mark_offered();
                            offer::offer_files(&ctx, files, partner);
                        }
                        FilesAction::AlreadyOffered => {
                            log("skip", "same copy seen again; already offered");
                            mark_offered();
                        }
                        FilesAction::Send => {
                            log("send", "new copy on the host; the viewer cannot take offers");
                            last_files = Some((files.clone(), Instant::now()));
                            sender::send_files(&ctx, files, partner, false);
                        }
                        FilesAction::Skip if received => log("skip", "every item came from this partner"),
                        FilesAction::Skip if unchanged => {
                            log("skip", "clipboard touched but the files are the same as before; not a new copy")
                        }
                        FilesAction::Skip => log("skip", "same copy seen again; already sent"),
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
                None if !clip.files.is_empty() => file_transfer::log_clipboard_files(
                    &partner,
                    &clip.files,
                    "skip",
                    "viewer copy for a host without offers; sent on Ctrl+V in the session",
                    false,
                ),
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
    /// Files the partner sent us, the copy just sent, or files that were already there: nothing to do.
    Skip,
}

/// What the watcher knows about the files on a changed clipboard.
#[derive(Clone, Copy)]
struct Look {
    /// Every item came from the partner.
    received: bool,
    /// The partner takes offers.
    offers: bool,
    /// The clipboard held the same files at the previous look in this session (or when it started).
    unchanged: bool,
}

/// Decides [`FilesAction`] for `files` given the last copy acted on (`last`, with when).
fn files_action(last: Option<&(Vec<PathBuf>, Instant)>, files: &[PathBuf], look: Look, now: Instant) -> FilesAction {
    if look.received {
        return FilesAction::Skip;
    }
    let repeat = last.is_some_and(|(f, at)| f.as_slice() == files && now.duration_since(*at) < SAME_COPY);
    match (repeat, look.offers) {
        (false, true) => FilesAction::Offer,
        // Sending costs the whole files: only a real new copy does it, never a clipboard another program merely
        // touched, nor files copied before the session.
        (false, false) if look.unchanged => FilesAction::Skip,
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
        assert_eq!(files_action(None, &files, OFFERS, t0), FilesAction::Offer);
        let last = (files.clone(), t0);
        // Explorer changed the clipboard again for the same copy: the new sequence number must still count as
        // offered, or Ctrl+V sends the files instead of pasting the offer.
        let again = t0 + Duration::from_millis(400);
        assert_eq!(files_action(Some(&last), &files, OFFERS, again), FilesAction::AlreadyOffered);
        assert_eq!(files_action(Some(&last), &files, SENDS, again), FilesAction::Skip);
        // The same files copied again later are a new copy.
        assert_eq!(files_action(Some(&last), &files, OFFERS, t0 + SAME_COPY), FilesAction::Offer);
        assert_eq!(files_action(Some(&last), &files[..1], OFFERS, again), FilesAction::Offer);
        assert_eq!(files_action(None, &files, SENDS, t0), FilesAction::Send);
        assert_eq!(files_action(None, &files, Look { received: true, ..OFFERS }, t0), FilesAction::Skip);
    }

    const OFFERS: Look = Look { received: false, offers: true, unchanged: false };
    const SENDS: Look = Look { received: false, offers: false, unchanged: false };

    /// The 02:27-02:28 trace: a host without offers had two `.lock` files on its clipboard from before the
    /// session; every Alt+Tab made a program touch the clipboard and the files went to the viewer again, three
    /// times. Only a real new copy is sent now.
    #[test]
    fn a_host_sends_only_new_copies() {
        let t0 = Instant::now();
        let locks = vec![PathBuf::from(r"C:\ProgramData\x\system.conf.lock"), PathBuf::from(r"C:\x\service.conf.lock")];
        let touched = Look { unchanged: true, ..SENDS };
        assert_eq!(files_action(None, &locks, touched, t0), FilesAction::Skip, "copied before the session");
        let later = t0 + Duration::from_secs(22);
        assert_eq!(files_action(None, &locks, touched, later), FilesAction::Skip, "clipboard touched, same files");
        // Offers stay cheap and keep their own repeat rule.
        assert_eq!(files_action(None, &locks, Look { unchanged: true, ..OFFERS }, later), FilesAction::Offer);
        // A real copy of other files (or of these after something else was copied) is sent.
        assert_eq!(files_action(None, &locks, SENDS, later), FilesAction::Send);
    }
}
