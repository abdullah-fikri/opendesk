//! Sending files and folders to a session partner.
//!
//! Each file is `start`, `chunk`s and `end` with its SHA-256. Chunks are binary messages when the receiver and
//! the relay both list `file-binary`, else base64 inside JSON (Android, older peers and relays). Pulled
//! clipboard offers stay JSON: the paste stream reads base64 chunks only. Receivers that advertise `file-ack`
//! answer `end` with an ACK/NACK; for them "Sent" means verified on the other side. Folders go only to
//! `file-folder` receivers, as folder entries plus files carrying their relative path.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use core_engine::codec::to_base64;
use sha2::{Digest, Sha256};

use super::{
    BINARY_CHUNK_SIZE, CHUNK_SIZE, IN_FLIGHT_CHUNKS, caps_of, chunk_units, epoch, expect_ack, forget_ack, hex,
    now_micros, tree,
};
use crate::domain::transfer::{MAX_FILE_BYTES, MAX_TREE_BYTES, MAX_TREE_ENTRIES, percent};
use crate::infra::logger;
use crate::protocol::{ClientMessage, FilePayload, Outgoing, binary};
use crate::service::context::AppContext;
use crate::service::ui_port::{TransferState, TransferUpdate, UiPort};

/// The sender waits this long for the receiver's verdict after queueing `end`.
const ACK_TIMEOUT: Duration = Duration::from_secs(120);
/// Most file messages allowed in the connection queue (~1.5 MB), in [`chunk_units`], before the sender waits.
const MAX_QUEUED_CHUNKS: usize = 48;
/// Status shown when a selection contained folders the partner cannot receive.
pub(super) const SKIPPED_FOLDERS: &str = "Folders skipped: the other device needs a newer OpenDesk to receive folders";

/// Selections being sent right now, per target, so a repeated Ctrl+V does not send the same files again.
static ACTIVE_SENDS: Mutex<Vec<(String, Vec<PathBuf>)>> = Mutex::new(Vec::new());

/// True while `paths` are still being sent to `target`.
pub fn is_sending(target: &str, paths: &[PathBuf]) -> bool {
    ACTIVE_SENDS.lock().unwrap().iter().any(|(t, p)| t == target && p.as_slice() == paths)
}

/// Removes its selection from [`ACTIVE_SENDS`] when the send thread ends, however it ends.
struct ActiveSend(Option<(String, Vec<PathBuf>)>);

impl Drop for ActiveSend {
    fn drop(&mut self) {
        if let Some(entry) = self.0.take() {
            let mut all = ACTIVE_SENDS.lock().unwrap();
            if let Some(pos) = all.iter().position(|e| *e == entry) {
                all.swap_remove(pos);
            }
        }
    }
}

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
    /// The relay forwards binary file chunks (sampled once: a new relay connection aborts the send anyway).
    relay_binary: bool,
}

impl Link {
    fn queue(&self, payload: FilePayload, status: Option<String>) {
        IN_FLIGHT_CHUNKS.fetch_add(1, Ordering::SeqCst);
        let _ = self.outbox.send(Outgoing::File { message: ClientMessage::file(&self.target, payload), status });
    }

    /// Queues `data` at `offset` of transfer `id`: binary when `binary`, else base64 JSON.
    fn queue_chunk(&self, binary: bool, id: &str, offset: u64, data: &[u8], status: Option<String>) {
        match binary.then(|| binary::file_chunk(id, offset, data)).flatten() {
            Some(frame) => {
                IN_FLIGHT_CHUNKS.fetch_add(chunk_units(frame.len()), Ordering::SeqCst);
                let _ = self.outbox.send(Outgoing::FileChunk { target: self.target.clone(), frame, status });
            }
            None => self.queue(FilePayload::chunk(id, to_base64(data)), status),
        }
    }

    fn status(&self, text: String) {
        self.ui.status(text);
    }

    /// Reports one file's progress to the transfer list.
    fn progress(&self, id: &str, name: &str, done: u64, total: u64, state: TransferState) {
        let id = format!("out:{}:{id}", self.target);
        self.ui.transfer(TransferUpdate { id, name: name.to_string(), outgoing: true, done, total, state });
    }

    /// Reports a failed file to the transfer list and the status line.
    fn fail(&self, id: &str, name: &str, total: u64, status: String) {
        self.progress(id, name, 0, total, TransferState::Failed(status.clone()));
        self.status(status);
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
    let active = match &job {
        Job::Paths(paths) => {
            let entry = (target.clone(), paths.clone());
            ACTIVE_SENDS.lock().unwrap().push(entry.clone());
            ActiveSend(Some(entry))
        }
        Job::Pull(..) => ActiveSend(None),
    };
    let relay_binary = ctx.relay.caps.lock().file_binary;
    let link = Link { outbox: ctx.outbox.clone(), ui: Arc::clone(&ctx.ui), target, relay_binary };
    crate::infra::thread::spawn_named("files-send", move || {
        let _active = active;
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
    // Only pulled offers come with the receiver's transfer ID; their paste stream reads JSON chunks only.
    let pulled = id.is_some();
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
    let caps = caps_of(&link.target);
    let wants_ack = caps.file_ack;
    let binary = caps.file_binary && link.relay_binary && !pulled;
    let chunk_size = if binary { BINARY_CHUNK_SIZE } else { CHUNK_SIZE };
    let (ack_tx, ack_rx) = mpsc::channel();
    expect_ack(&id, ack_tx);

    let mut start = FilePayload::start(&id, &wire_name, size);
    if let Some(t) = &tag {
        start.group = Some(t.group.clone());
        start.rel = Some(t.rel.clone());
    }
    link.queue(start, Some(format!("Sending {label} (0%)...")));
    link.progress(&id, &label, 0, size, TransferState::Active);

    let mut buf = vec![0u8; chunk_size];
    let mut hash = Sha256::new();
    let mut sent = 0u64;
    let mut chunks = 0usize;
    let mut aborted = false;
    let mut shown_percent = 0;
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
        // Never past the announced size: a file still being written (a log) is sent as it was at the start.
        let want = (size - sent).min(chunk_size as u64) as usize;
        if want == 0 {
            break;
        }
        match file.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => {
                hash.update(&buf[..n]);
                let offset = sent;
                sent += n as u64;
                chunks += 1;
                let status = (size > 0 && (binary || chunks.is_multiple_of(4) || sent == size))
                    .then(|| format!("Sending {label} ({}%)...", percent(sent, size)));
                link.queue_chunk(binary, &id, offset, &buf[..n], status);
                if percent(sent, size) != shown_percent {
                    shown_percent = percent(sent, size);
                    link.progress(&id, &label, sent, size, TransferState::Active);
                }
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
        return link.fail(&id, &label, size, format!("Could not send {label}"));
    }

    link.queue(
        FilePayload::end(&id, &wire_name, hex(&hash.finalize()), auto_paste),
        Some(format!("Waiting for confirmation: {label}")),
    );
    if wants_ack {
        link.progress(&id, &label, size, size, TransferState::Verifying);
        match ack_rx.recv_timeout(ACK_TIMEOUT) {
            Ok(true) => {
                logger::info("file_transfer", &format!("Sent and confirmed: {label}"));
                link.progress(&id, &label, size, size, TransferState::Done);
                link.status(format!("Sent: {label}"));
            }
            Ok(false) => {
                logger::warn("file_transfer", &format!("Transfer rejected: {label}"));
                link.fail(&id, &label, size, format!("Could not send {label}: rejected or checksum mismatch"));
            }
            Err(_) => {
                forget_ack(&id);
                logger::warn("file_transfer", &format!("Transfer confirmation timed out: {label}"));
                link.fail(&id, &label, size, format!("Could not send {label}: no confirmation from the receiver"));
            }
        }
        return;
    }
    // Receivers without `file-ack` (Android) send no verdict: "sent" only means every byte left this socket.
    forget_ack(&id);
    while IN_FLIGHT_CHUNKS.load(Ordering::Relaxed) > 0 {
        if epoch() != started {
            return link.fail(&id, &label, size, format!("Could not send {label}: connection lost"));
        }
        thread::sleep(Duration::from_millis(50));
    }
    link.progress(&id, &label, size, size, TransferState::Done);
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

    /// Sends `data` to `target` and reassembles it from the outbox: (bytes on the wire, binary chunks seen).
    fn send_and_collect(target: &str, data: &[u8], relay_binary: bool) -> (usize, usize) {
        let dir = env::temp_dir().join(format!("opendesk-wire-{}-{target}-{relay_binary}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wire.bin");
        fs::write(&path, data).unwrap();
        let (ctx, rx) = test_support::context_with_outbox();
        ctx.relay.caps.lock().file_binary = relay_binary;
        send_files(&ctx, vec![path.clone()], target.into(), false);
        let (mut wire, mut binary_chunks, mut got) = (0, 0, Vec::new());
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("sender stalled") {
                Outgoing::File { message, .. } => {
                    IN_FLIGHT_CHUNKS.fetch_sub(1, Ordering::SeqCst);
                    wire += message.to_json().len();
                    let ClientMessage::RelayFile { payload, .. } = message else { panic!("not a file message") };
                    match payload.action {
                        FileAction::Chunk => {
                            got.extend(core_engine::codec::from_base64(&payload.chunk.unwrap()).unwrap())
                        }
                        FileAction::End => {
                            assert_eq!(payload.sha256.unwrap(), hex(&Sha256::digest(data)));
                            break;
                        }
                        _ => {}
                    }
                }
                Outgoing::FileChunk { target: to, frame, .. } => {
                    IN_FLIGHT_CHUNKS.fetch_sub(chunk_units(frame.len()), Ordering::SeqCst);
                    wire += frame.len();
                    binary_chunks += 1;
                    let chunk = binary::parse_file_chunk(&frame).unwrap();
                    assert_eq!((to.as_str(), chunk.offset), (target, got.len() as u64));
                    assert!(chunk.data.len() <= BINARY_CHUNK_SIZE);
                    got.extend_from_slice(chunk.data);
                }
                _ => {}
            }
        }
        assert!(got == data, "reassembled bytes differ");
        let _ = fs::remove_dir_all(dir);
        (wire, binary_chunks)
    }

    /// Binary chunks only when the receiver and the relay both list `file-binary`; the bytes arrive in order
    /// either way. Prints the bytes on the wire for a 10 MB file (`-- --nocapture`).
    #[test]
    fn chunks_are_binary_only_when_receiver_and_relay_agree() {
        let data: Vec<u8> = (0..10_000_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        super::super::set_session_caps("710000001", PeerCaps { file_binary: true, ..PeerCaps::default() });
        super::super::set_session_caps("710000002", PeerCaps::default());
        let (json_wire, n) = send_and_collect("710000002", &data, true);
        assert_eq!(n, 0, "receiver without file-binary got binary chunks");
        let (old_relay_wire, n) = send_and_collect("710000001", &data, false);
        assert_eq!(n, 0, "binary chunks through a relay without file-binary");
        let (binary_wire, n) = send_and_collect("710000001", &data, true);
        assert_eq!(n, data.len().div_ceil(BINARY_CHUNK_SIZE));
        println!(
            "10 MB file: JSON {json_wire} B ({:.3}x), binary {binary_wire} B ({:.4}x), messages {} -> {}",
            json_wire as f64 / data.len() as f64,
            binary_wire as f64 / data.len() as f64,
            data.len().div_ceil(CHUNK_SIZE) + 2,
            n + 2
        );
        assert_eq!(json_wire, old_relay_wire);
        assert!(binary_wire < data.len() + data.len() / 100 && json_wire > data.len() * 4 / 3);
        super::super::remove_session_caps("710000001");
        super::super::remove_session_caps("710000002");
    }
}
