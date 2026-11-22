//! Clipboard file offers (AnyDesk style): copying files sends only their names and sizes; the partner exposes
//! them as virtual clipboard files and pulls each one when it is pasted.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use parking_lot::Mutex;

use super::sender::{SKIPPED_FOLDERS, send_pulled};
use super::{Stamp, caps_of, limits_to, now_micros, tree};
use crate::domain::transfer::{MAX_TREE_ENTRIES, is_safe_id};
use crate::infra::logger;
use crate::protocol::{ClientMessage, FilePayload, OfferEntry, Outgoing};
use crate::service::context::AppContext;

/// Most top-level files in an offer without folders.
const MAX_OFFER_FILES: usize = 100;
/// Offers kept for pulls; older ones can no longer be pasted.
const KEPT_OFFERS: usize = 4;

/// An offer we made. `None` paths mark folder entries, which are never pulled.
struct Offer {
    id: String,
    partner: String,
    paths: Vec<Option<PathBuf>>,
}

/// Our offers, newest last. A pull may only ask for these.
static OFFERS: Mutex<Vec<Offer>> = Mutex::new(Vec::new());
/// Bumped per clipboard copy so a slow folder walk never publishes over a newer copy.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Offers copied `paths` to `target` and sets the status line. Folders go only to `file-folder` partners; their
/// walk runs on its own thread so the clipboard watcher keeps polling.
pub fn offer_files(ctx: &AppContext, paths: Vec<PathBuf>, target: String) {
    offer_files_at(ctx, paths, target, None);
}

/// Like [`offer_files`]; `drop` (0..10000 each) says the files were dropped at that point of the partner's
/// screen, so it drags them there instead of putting them on its clipboard (`file-drop` partners only).
pub fn offer_files_at(ctx: &AppContext, paths: Vec<PathBuf>, target: String, drop: Option<(u32, u32)>) {
    let drop = drop.filter(|_| caps_of(&target).file_drop);
    if target.is_empty() || paths.is_empty() {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let has_dir = paths.iter().any(|p| p.is_dir());
    let limits = limits_to(&target);
    if !has_dir || !caps_of(&target).file_folder {
        // Top-level files only.
        let files: Vec<(Option<PathBuf>, OfferEntry)> = paths
            .iter()
            .filter_map(|p| {
                let meta = fs::metadata(p).ok().filter(|m| m.is_file() && m.len() <= limits.file)?;
                let name = p.file_name()?.to_str()?.to_string();
                let entry = Stamp::of(&meta).entry(name, Some(meta.len()), None);
                Some((Some(p.clone()), entry))
            })
            .take(MAX_OFFER_FILES)
            .collect();
        let count = files.len();
        let status = match (publish(generation, files, &target, &ctx.outbox, drop), has_dir) {
            (true, false) => Some(format!("{count} files ready to paste on the other device")),
            (true, true) => Some(format!("{count} files ready to paste. {SKIPPED_FOLDERS}")),
            (false, true) => Some(SKIPPED_FOLDERS.to_string()),
            (false, false) => None,
        };
        if let Some(status) = status {
            ctx.ui.status(status);
        }
        return;
    }
    ctx.ui.status("Preparing folder...".into());
    let (outbox, ui) = (ctx.outbox.clone(), Arc::clone(&ctx.ui));
    crate::infra::thread::spawn_named("files-offer", move || {
        let status = match tree::walk(&paths, MAX_TREE_ENTRIES, limits) {
            Ok(entries) => {
                let listing = entries
                    .into_iter()
                    .map(|e| {
                        let stamp = Stamp::of_path(&e.path);
                        match e.dir {
                            true => (None, stamp.entry(e.rel, None, Some(true))),
                            false => (Some(e.path), stamp.entry(e.rel, Some(e.size), None)),
                        }
                    })
                    .collect();
                publish(generation, listing, &target, &outbox, drop)
                    .then(|| format!("{} items ready to paste on the other device", paths.len()))
            }
            Err(status) => Some(status),
        };
        if let Some(status) = status.filter(|_| GENERATION.load(Ordering::SeqCst) == generation) {
            ui.status(status);
        }
    });
}

/// Records and sends one offer unless a newer copy already superseded it.
fn publish(
    generation: u64,
    entries: Vec<(Option<PathBuf>, OfferEntry)>,
    target: &str,
    outbox: &Sender<Outgoing>,
    drop: Option<(u32, u32)>,
) -> bool {
    if entries.is_empty() {
        return false;
    }
    // Held while sending so offers leave in copy order even when a folder walk finishes late.
    let mut offers = OFFERS.lock();
    if GENERATION.load(Ordering::SeqCst) != generation {
        return false;
    }
    let id = format!("of_{}", now_micros());
    let (paths, listing): (Vec<Option<PathBuf>>, Vec<OfferEntry>) = entries.into_iter().unzip();
    logger::info("file_transfer", &format!("Offering {} entries as {id}.", listing.len()));
    offers.push(Offer { id: id.clone(), partner: target.to_string(), paths });
    let excess = offers.len().saturating_sub(KEPT_OFFERS);
    offers.drain(..excess);
    let mut payload = FilePayload::offer(&id, listing);
    if let Some((x, y)) = drop {
        payload.drop_x = Some(x.min(10_000));
        payload.drop_y = Some(y.min(10_000));
    }
    let _ = outbox.send(Outgoing::Message(ClientMessage::file(target, payload)));
    true
}

/// The partner pasted one of our offered files: send it under the transfer ID it chose.
pub fn serve_pull(ctx: &AppContext, from: &str, payload: &FilePayload) {
    let index = payload.index.unwrap_or(u64::MAX) as usize;
    let Some(tid) = payload.tid.as_deref().filter(|t| is_safe_id(t)) else { return };
    let path = OFFERS
        .lock()
        .iter()
        .find(|o| o.id == payload.id && o.partner == from)
        .and_then(|o| o.paths.get(index).cloned().flatten()); // folder entries are never pulled
    match path {
        Some(path) => {
            logger::info("file_transfer", &format!("Pull for offer {} file {index} ({tid}).", payload.id));
            send_pulled(ctx, path, tid.to_string(), from.to_string());
        }
        None => logger::warn("file_transfer", &format!("Rejected pull for unknown offer {} file {index}.", payload.id)),
    }
}
