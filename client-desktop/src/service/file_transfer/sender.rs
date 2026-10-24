//! Sending files and folders to a session partner.
//!
//! Each file is `start`, base64 `chunk`s and `end` with its SHA-256. Receivers that advertise `file-ack`
//! answer `end` with an ACK/NACK; for them "Sent" means verified on the other side. Folders go only to
//! `file-folder` receivers, as folder entries plus files carrying their relative path.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use core_engine::codec::to_base64;
use sha2::{Digest, Sha256};

use super::{CHUNK_SIZE, IN_FLIGHT_CHUNKS, caps_of, epoch, expect_ack, forget_ack, hex, now_micros, tree};
use crate::domain::transfer::{MAX_FILE_BYTES, MAX_TREE_BYTES, MAX_TREE_ENTRIES, percent};
use crate::infra::logger;
use crate::protocol::{ClientMessage, FilePayload, Outgoing};
use crate::service::context::AppContext;
use crate::service::ui_port::UiPort;

/// The sender waits this long for the receiver's verdict after queueing `end`.
const ACK_TIMEOUT: Duration = Duration::from_secs(120);
/// Most file messages allowed in the connection queue (~1.5 MB) before the sender waits.
const MAX_QUEUED_CHUNKS: usize = 48;
/// Status shown when a selection contained folders the partner cannot receive.
pub(super) const SKIPPED_FOLDERS: &str = "Folders skipped: the other device needs a newer OpenDesk to receive folders";

/// Folder membership of one item of a folder transfer.
struct FolderTag {
    group: String,
    rel: String,
    dir: bool,
    /// Repeat of the first folder entry that tells the receiver the whole group was sent.
    end: bool,
}

struct SendItem {
    path: PathBuf,
    /// Transfer ID chosen by the receiver (pulled offers); generated otherwise.
    id: Option<String>,
    tag: Option<FolderTag>,
}

enum Job {
    /// Dropped, picked or pasted paths; may contain folders.
    Paths(Vec<PathBuf>),
    /// One offered file pulled under the receiver's transfer ID, so it can route the bytes to its paste stream.
    Pull(PathBuf, String),
}

/// Everything a send thread needs.
struct Link {
    outbox: Sender<Outgoing>,
    ui: Arc<dyn UiPort>,
    target: String,
}

impl Link {
    fn queue(&self, payload: FilePayload, status: Option<String>) {
        IN_FLIGHT_CHUNKS.fetch_add(1, Ordering::SeqCst);
        let _ = self.outbox.send(Outgoing::File { message: ClientMessage::file(&self.target, payload), status });
    }

    fn status(&self, text: String) {
        self.ui.status(text);
    }
}

/// Sends `paths` (files and folders) to `target` in the background. `auto_paste` asks the receiver to paste
/// them into its focused Explorer window.
pub fn send_files(ctx: &AppContext, paths: Vec<PathBuf>, target: String, auto_paste: bool) {
    if !paths.is_empty() {
        run(ctx, Job::Paths(paths), target, auto_paste);
    }
}

/// Sends one offered file the partner pulled, under the transfer ID it chose.
pub(super) fn send_pulled(ctx: &AppContext, path: PathBuf, tid: String, target: String) {
    run(ctx, Job::Pull(path, tid), target, false);
}

fn run(ctx: &AppContext, job: Job, target: String, auto_paste: bool) {
    if target.is_empty() {
        return;
    }
    let link = Link { outbox: ctx.outbox.clone(), ui: Arc::clone(&ctx.ui), target };
    thread::spawn(move || {
        // Folders are walked here, on the worker thread, so a big tree never stalls the UI.
        let (items, skipped_folders) = match job {
            Job::Pull(path, tid) => (vec![SendItem { path, id: Some(tid), tag: None }], false),
            Job::Paths(paths) => match expand(paths, &link.target) {
                Ok(expanded) => expanded,
                Err(status) => return link.status(status),
            },
        };
        let started = epoch();
        for item in items {
            if epoch() != started {
                return link.status("Could not send files: connection lost".into());
            }
            match &item.tag {
                Some(tag) if tag.dir => send_folder_entry(&link, tag, auto_paste),
                _ => send_file(&link, item, started, auto_paste),
            }
        }
        if skipped_folders {
            link.status(SKIPPED_FOLDERS.into());
        }
    });
}

/// Turns selected paths into send items: `(items, folders_skipped)` or an error status. Without folders the
/// list is exactly the selection.
fn expand(paths: Vec<PathBuf>, target: &str) -> Result<(Vec<SendItem>, bool), String> {
    let plain = |path: PathBuf| SendItem { path, id: None, tag: None };
    if !paths.iter().any(|p| p.is_dir()) {
        return Ok((paths.into_iter().map(plain).collect(), false));
    }
    if !caps_of(target).file_folder {
        logger::info("file_transfer", "Partner lacks file-folder; skipping folders in the selection.");
        return Ok((paths.into_iter().filter(|p| !p.is_dir()).map(plain).collect(), true));
    }
    let entries = tree::walk(&paths, MAX_TREE_ENTRIES, MAX_TREE_BYTES)?;
    let group = format!("fg_{}", now_micros());
    let marker = entries.iter().find(|e| e.dir).map(|e| (e.path.clone(), e.rel.clone()));
    let mut items: Vec<SendItem> = entries
        .into_iter()
        .map(|e| SendItem {
            path: e.path,
            id: None,
            tag: Some(FolderTag { group: group.clone(), rel: e.rel, dir: e.dir, end: false }),
        })
        .collect();
    // Always close the group with a marker, so the receiver fills its clipboard even if some file failed here.
    if let Some((path, rel)) = marker {
        items.push(SendItem { path, id: None, tag: Some(FolderTag { group, rel, dir: true, end: true }) });
    }
    logger::info("file_transfer", &format!("Sending folder selection: {} entries.", items.len() - 1));
    Ok((items, false))
}

/// Sends a folder entry: one `start` with `dir`, answered by an ACK; no bytes follow.
fn send_folder_entry(link: &Link, tag: &FolderTag, auto_paste: bool) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let id = format!("fd_{}_{}", now_micros(), SEQ.fetch_add(1, Ordering::Relaxed));
    let name = tag.rel.rsplit('\\').next().unwrap_or(&tag.rel);
    let wants_ack = caps_of(&link.target).file_ack;
    let (ack_tx, ack_rx) = mpsc::channel();
    if wants_ack {
        expect_ack(&id, ack_tx);
    }
    let payload = FilePayload {
        dir: Some(true),
        group: Some(tag.group.clone()),
        rel: Some(tag.rel.clone()),
        group_end: Some(tag.end),
        auto_paste: Some(auto_paste),
        ..FilePayload::start(&id, name, 0)
    };
    link.queue(payload, (!tag.end).then(|| format!("Sending folder {}...", tag.rel)));
    if !wants_ack {
        return;
    }
    match ack_rx.recv_timeout(ACK_TIMEOUT) {
        Ok(true) if tag.end => link.status(format!("Sent: {}", tag.rel)),
        Ok(true) => {}
        Ok(false) => link.status(format!("Could not send folder {}", tag.rel)),
        Err(_) => {
            forget_ack(&id);
            link.status(format!("Could not send folder {}: no confirmation from the receiver", tag.rel));
        }
    }
}

/// Sends one file and waits for its verdict.
fn send_file(link: &Link, item: SendItem, started: u64, auto_paste: bool) {
    let SendItem { path, id, tag } = item;
    if !path.is_file() {
        return;
    }
    let wire_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file.bin").to_string();
    // Inside a folder the status shows the relative path; the wire `name` stays a plain file name.
    let label = tag.as_ref().map_or_else(|| wire_name.clone(), |t| t.rel.clone());
    let mut file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            logger::error("file_transfer", &format!("Failed to open {path:?}: {e}"));
            return link.status(format!("Could not open {label}"));
        }
    };
    let size = match file.metadata() {
        Ok(m) if m.len() <= MAX_FILE_BYTES => m.len(),
        _ => return link.status(format!("Could not send {label}: larger than 2 GB")),
    };
    let id = id.unwrap_or_else(|| format!("ft_{}", now_micros()));
    logger::info("file_transfer", &format!("Sending id={id} name={label} bytes={size} auto_paste={auto_paste}"));

    // Sampled per file: the partner (and its caps) may change while a background transfer runs.
    let wants_ack = caps_of(&link.target).file_ack;
    let (ack_tx, ack_rx) = mpsc::channel();
    expect_ack(&id, ack_tx);

    let mut start = FilePayload::start(&id, &wire_name, size);
    if let Some(t) = &tag {
        start.group = Some(t.group.clone());
        start.rel = Some(t.rel.clone());
    }
    link.queue(start, Some(format!("Sending {label} (0%)...")));

    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut hash = Sha256::new();
    let mut sent = 0u64;
    let mut chunks = 0usize;
    let mut aborted = false;
    loop {
        // An early NACK (receiver rejected start) stops sending chunks the relay would drop anyway.
        if epoch() != started || ack_rx.try_recv() == Ok(false) {
            aborted = true;
            break;
        }
        while IN_FLIGHT_CHUNKS.load(Ordering::Relaxed) > MAX_QUEUED_CHUNKS && epoch() == started {
            thread::sleep(Duration::from_millis(8));
        }
        if epoch() != started {
            aborted = true;
            break;
        }
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hash.update(&buf[..n]);
                sent += n as u64;
                chunks += 1;
                let status = (size > 0 && (chunks.is_multiple_of(4) || sent == size))
                    .then(|| format!("Sending {label} ({}%)...", percent(sent, size)));
                link.queue(FilePayload::chunk(&id, to_base64(&buf[..n])), status);
                thread::sleep(Duration::from_millis(3));
            }
            Err(e) => {
                logger::error("file_transfer", &format!("Read failed for {path:?}: {e}"));
                aborted = true;
                break;
            }
        }
    }
    if aborted || sent != size {
        logger::error("file_transfer", &format!("Transfer incomplete for {path:?}"));
        forget_ack(&id);
        return link.status(format!("Could not send {label}"));
    }

    link.queue(
        FilePayload::end(&id, &wire_name, hex(&hash.finalize()), auto_paste),
        Some(format!("Waiting for confirmation: {label}")),
    );
    if wants_ack {
        match ack_rx.recv_timeout(ACK_TIMEOUT) {
            Ok(true) => link.status(format!("Sent: {label}")),
            Ok(false) => {
                logger::warn("file_transfer", &format!("Transfer rejected: {label}"));
                link.status(format!("Could not send {label}: rejected or checksum mismatch"));
            }
            Err(_) => {
                forget_ack(&id);
                logger::warn("file_transfer", &format!("Transfer confirmation timed out: {label}"));
                link.status(format!("Could not send {label}: no confirmation from the receiver"));
            }
        }
        return;
    }
    // Receivers without `file-ack` (Android) send no verdict: "sent" only means every byte left this socket.
    forget_ack(&id);
    while IN_FLIGHT_CHUNKS.load(Ordering::Relaxed) > 0 {
        if epoch() != started {
            return link.status(format!("Could not send {label}: connection lost"));
        }
        thread::sleep(Duration::from_millis(50));
    }
    link.status(format!("Sent (not confirmed by the receiver): {label}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::capability::PeerCaps;
    use crate::protocol::FileAction;
    use crate::service::file_transfer::receiver::{FileReceiver, tests::TEST_DOWNLOADS};
    use crate::service::file_transfer::tree::tests::temp_tree;
    use crate::service::file_transfer::{RECEIVED_CLIPBOARD, now_ms};
    use crate::service::test_support;
    use std::{env, fs};

    /// Drag-and-drop path end to end: sender messages fed straight into a receiver. Sets the clipboard, so run
    /// it by hand: `cargo test --release folder_send -- --ignored`
    #[test]
    #[ignore]
    fn folder_send_round_trip_recreates_tree() {
        let src = temp_tree("send");
        let downloads = env::temp_dir().join(format!("opendesk-recv-{}-{}", std::process::id(), now_ms()));
        fs::create_dir_all(downloads.join("Project")).unwrap(); // existing folder: the copy must become "Project (1)"
        *TEST_DOWNLOADS.lock().unwrap() = Some(downloads.clone());
        super::super::set_partner_caps(PeerCaps { file_folder: true, ..PeerCaps::default() });
        let (ctx, rx) = test_support::context_with_outbox();
        send_files(&ctx, vec![src.join("Project"), src.join("loose.txt")], "222222222".into(), false);

        let mut receiver = FileReceiver::new();
        let mut finished = false;
        while let Ok(Outgoing::File { message: ClientMessage::RelayFile { payload, .. }, .. }) =
            rx.recv_timeout(Duration::from_secs(10))
        {
            IN_FLIGHT_CHUNKS.fetch_sub(1, Ordering::SeqCst);
            assert!(payload.name.as_deref().is_none_or(|n| !n.contains('\\')), "wire name must stay plain");
            let (status, ack) = receiver.handle(&ctx, &payload, "222222222", false);
            assert_ne!(ack, Some(false), "rejected: {status:?}");
            if payload.action == FileAction::Start && payload.group_end == Some(true) {
                finished = true;
                break;
            }
        }
        assert!(finished, "group end marker never arrived");
        let copy = downloads.join("Project (1)");
        assert_eq!(fs::read(copy.join("src/main.rs")).unwrap(), b"fn main() {}");
        assert_eq!(fs::read(copy.join("src/deep/x.txt")).unwrap(), b"x");
        assert!(copy.join("empty").is_dir());
        assert_eq!(fs::read(downloads.join("loose.txt")).unwrap(), b"12345");
        assert_eq!(fs::read_dir(downloads.join("Project")).unwrap().count(), 0, "existing folder untouched");
        let ours = RECEIVED_CLIPBOARD.lock().unwrap().clone();
        assert_eq!(
            ours,
            vec![copy.to_string_lossy().to_string(), downloads.join("loose.txt").to_string_lossy().to_string()]
        );
        *TEST_DOWNLOADS.lock().unwrap() = None;
        let _ = fs::remove_dir_all(src);
        let _ = fs::remove_dir_all(downloads);
    }
}
