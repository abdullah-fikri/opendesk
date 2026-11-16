//! Sending files and folders to a session partner.
//!
//! Each file is `start`, `chunk`s and `end` with its SHA-256. Chunks are binary messages when the receiver and
//! the relay both list `file-binary`, else base64 inside JSON (Android, older peers and relays). Pulled
//! clipboard offers go binary only to receivers that also list `file-pull-bin`. Receivers that advertise `file-ack`
//! answer `end` with an ACK/NACK; for them "Sent" means verified on the other side. Folders go only to
//! `file-folder` receivers, as folder entries plus files carrying their relative path.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use core_engine::codec::to_base64;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use super::{
    BINARY_CHUNK_SIZE, CHUNK_SIZE, Delivery, IN_FLIGHT_CHUNKS, Resumes, caps_of, chunk_units, epoch, expect_ack,
    forget_ack, hex, now_micros, tree,
};
use crate::domain::file_squeeze::{OFFSET_DEFLATED, Squeeze};
use crate::domain::transfer::{MAX_FILE_BYTES, MAX_TREE_BYTES, MAX_TREE_ENTRIES, percent, safe_rel_path};
use crate::infra::logger;
use crate::protocol::{ClientMessage, FileMark, FilePayload, Outgoing, binary};
use crate::service::context::AppContext;
use crate::service::ui_port::{TransferState, TransferUpdate, UiPort};

/// The sender waits this long for the receiver's verdict after queueing `end`.
const ACK_TIMEOUT: Duration = Duration::from_secs(120);
/// How often a sender waiting for the verdict refreshes its progress.
const PROGRESS_POLL: Duration = Duration::from_millis(250);
/// Most file messages allowed in the connection queue (~1.5 MB), in [`chunk_units`], before the sender waits.
const MAX_QUEUED_CHUNKS: usize = 48;
/// Status shown when a selection contained folders the partner cannot receive.
pub(super) const SKIPPED_FOLDERS: &str = "Folders skipped: the other device needs a newer OpenDesk to receive folders";

/// Selections being sent right now, per target, so a repeated Ctrl+V does not send the same files again.
static ACTIVE_SENDS: Mutex<Vec<(String, Vec<PathBuf>)>> = Mutex::new(Vec::new());

/// True while `paths` are still being sent to `target`.
pub fn is_sending(target: &str, paths: &[PathBuf]) -> bool {
    ACTIVE_SENDS.lock().iter().any(|(t, p)| t == target && p.as_slice() == paths)
}

/// Removes its selection from [`ACTIVE_SENDS`] when the send thread ends, however it ends.
struct ActiveSend((String, Vec<PathBuf>));

impl Drop for ActiveSend {
    fn drop(&mut self) {
        let mut all = ACTIVE_SENDS.lock();
        if let Some(pos) = all.iter().position(|e| *e == self.0) {
            all.swap_remove(pos);
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
        self.queue_marked(payload, status, None);
    }

    /// Queues `payload`; `mark` comes back through [`super::set_delivered`] once the relay has read it.
    fn queue_marked(&self, payload: FilePayload, status: Option<String>, mark: Option<FileMark>) {
        IN_FLIGHT_CHUNKS.fetch_add(1, Ordering::SeqCst);
        let message = ClientMessage::file(&self.target, payload);
        let _ = self.outbox.send(Outgoing::File { message, status, mark });
    }

    /// Queues `data` at `offset` of transfer `id`: binary when `binary`, else base64 JSON. `packed` is `data`
    /// deflated (binary only), sent instead with the offset flagged.
    fn queue_chunk(&self, binary: bool, id: &str, offset: u64, data: &[u8], packed: Option<Vec<u8>>, mark: FileMark) {
        let frame = match packed {
            Some(packed) if binary => binary::file_chunk(id, offset | OFFSET_DEFLATED, &packed),
            _ => binary.then(|| binary::file_chunk(id, offset, data)).flatten(),
        };
        match frame {
            Some(frame) => {
                IN_FLIGHT_CHUNKS.fetch_add(chunk_units(frame.len()), Ordering::SeqCst);
                let target = self.target.clone();
                let _ = self.outbox.send(Outgoing::FileChunk { target, frame, status: None, mark: Some(mark) });
            }
            None => self.queue_marked(FilePayload::chunk(id, to_base64(data)), None, Some(mark)),
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
        run(ctx, paths, target, auto_paste);
    }
}

/// Sends one offered file the partner pulled, under the transfer ID it chose. Pulls wait in [`PULLS`], so a
/// paste of many files never runs more than [`MAX_PARALLEL_PULLS`] sends (threads, relay grants) at once.
pub(super) fn send_pulled(ctx: &AppContext, path: PathBuf, tid: String, target: String) {
    if target.is_empty() {
        return;
    }
    let link = link_for(ctx, target);
    // A pull queued before the relay connection dropped belongs to a dead grant and paste stream.
    let started = epoch();
    PULLS.submit(Box::new(move || {
        if epoch() == started {
            send_file(&link, SendItem { path, id: Some(tid), tag: None }, &mut started.clone(), false);
        }
    }));
}

fn link_for(ctx: &AppContext, target: String) -> Link {
    let relay_binary = ctx.relay.caps.lock().file_binary;
    Link { outbox: ctx.outbox.clone(), ui: Arc::clone(&ctx.ui), target, relay_binary }
}

/// Most pulled files sent at the same time. Explorer reads a multi-file paste one stream after another, so a
/// second running send only hides the gap between files; more would cost one thread and one relay grant (the
/// relay allows 8 per peer) each, and a 50-file paste must not open 50 of them.
const MAX_PARALLEL_PULLS: usize = 2;
/// Pulled files waiting to be sent, in the order the partner asked for them.
static PULLS: TaskQueue = TaskQueue::new("files-pull", MAX_PARALLEL_PULLS);

type Task = Box<dyn FnOnce() + Send>;

/// Runs submitted tasks in submission order on at most `limit` threads, which exit when the queue is empty.
struct TaskQueue {
    name: &'static str,
    limit: usize,
    state: Mutex<QueueState>,
}

struct QueueState {
    workers: usize,
    waiting: VecDeque<Task>,
}

/// Replaces a worker whose task panicked, so the tasks queued behind it still run.
struct WorkerSlot(Option<&'static TaskQueue>);

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        if let Some(queue) = self.0 {
            let mut state = queue.state.lock();
            state.workers -= 1;
            if !state.waiting.is_empty() {
                state.workers += 1;
                drop(state);
                queue.spawn_worker();
            }
        }
    }
}

impl TaskQueue {
    const fn new(name: &'static str, limit: usize) -> Self {
        Self { name, limit, state: Mutex::new(QueueState { workers: 0, waiting: VecDeque::new() }) }
    }

    /// Queues `task`; starts a worker unless `limit` are already running.
    fn submit(&'static self, task: Task) {
        let mut state = self.state.lock();
        state.waiting.push_back(task);
        if state.workers < self.limit {
            state.workers += 1;
            drop(state);
            self.spawn_worker();
        }
    }

    /// Starts one worker; the caller has already counted it in `workers`.
    fn spawn_worker(&'static self) {
        crate::infra::thread::spawn_named(self.name, move || self.drain());
    }

    fn drain(&'static self) {
        let mut slot = WorkerSlot(Some(self));
        loop {
            let task = {
                let mut state = self.state.lock();
                match state.waiting.pop_front() {
                    Some(task) => task,
                    None => {
                        // Under the same lock as the empty check: a task submitted right now sees one worker
                        // fewer and starts its own.
                        state.workers -= 1;
                        slot.0 = None;
                        return;
                    }
                }
            };
            task();
        }
    }
}

fn run(ctx: &AppContext, paths: Vec<PathBuf>, target: String, auto_paste: bool) {
    if target.is_empty() {
        return;
    }
    let entry = (target.clone(), paths.clone());
    ACTIVE_SENDS.lock().push(entry.clone());
    let active = ActiveSend(entry);
    let link = link_for(ctx, target);
    crate::infra::thread::spawn_named("files-send", move || {
        let _active = active;
        // Folders are walked here, on the worker thread, so a big tree never stalls the UI.
        let (items, skipped_folders) = match expand(paths, &link.target) {
            Ok(expanded) => expanded,
            Err(status) => return link.status(status),
        };
        let mut started = epoch();
        for item in items {
            if epoch() != started {
                return link.status("Could not send files: connection lost".into());
            }
            match &item.tag {
                Some(tag) if tag.dir => send_folder_entry(&link, tag, auto_paste),
                _ => send_file(&link, item, &mut started, auto_paste),
            }
        }
        if skipped_folders {
            link.status(SKIPPED_FOLDERS.into());
        }
    });
}

/// Turns selected paths into send items: `(items, folders_skipped)` or an error status. Without folders the
/// list is the selection: one group for `file-group` receivers (several files), else one transfer per file.
fn expand(paths: Vec<PathBuf>, target: &str) -> Result<(Vec<SendItem>, bool), String> {
    let plain = |path: PathBuf| SendItem { path, id: None, tag: None };
    if !paths.iter().any(|p| p.is_dir()) {
        return Ok((group_loose(paths, caps_of(target).file_group), false));
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

/// Loose files as send items. With `grouped` (the receiver lists `file-group`) several files travel as one
/// group whose items carry their plain name as `rel`, closed by a bare group end (no `rel`), so the receiver
/// publishes and pastes them once instead of each on its own. Names the receiver's folder rules would reject
/// keep the old one-by-one path, where they are only sanitized.
fn group_loose(paths: Vec<PathBuf>, grouped: bool) -> Vec<SendItem> {
    let name = |p: &PathBuf| p.file_name().and_then(|n| n.to_str()).map(str::to_string);
    let groupable = grouped
        && paths.len() > 1
        && paths.iter().all(|p| name(p).is_some_and(|n| safe_rel_path(&n).is_some_and(|c| c.len() == 1)));
    if !groupable {
        return paths.into_iter().map(|path| SendItem { path, id: None, tag: None }).collect();
    }
    let group = format!("fg_{}", now_micros());
    let marker = paths[0].clone();
    let mut items: Vec<SendItem> = paths
        .into_iter()
        .map(|path| {
            let rel = name(&path).unwrap_or_default();
            SendItem { path, id: None, tag: Some(FolderTag { group: group.clone(), rel, dir: false, end: false }) }
        })
        .collect();
    items.push(SendItem {
        path: marker,
        id: None,
        tag: Some(FolderTag { group, rel: String::new(), dir: true, end: true }),
    });
    logger::info("file_transfer", &format!("Sending {} loose files as one group.", items.len() - 1));
    items
}

/// Sends a folder entry: one `start` with `dir`, answered by an ACK; no bytes follow. An end marker with an
/// empty `rel` is the bare group end of loose files.
fn send_folder_entry(link: &Link, tag: &FolderTag, auto_paste: bool) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let id = format!("fd_{}_{}", now_micros(), SEQ.fetch_add(1, Ordering::Relaxed));
    let bare = tag.rel.is_empty();
    let name = if bare { "files" } else { tag.rel.rsplit('\\').next().unwrap_or(&tag.rel) };
    let wants_ack = caps_of(&link.target).file_ack;
    let (ack_tx, ack_rx) = mpsc::channel();
    if wants_ack {
        expect_ack(&id, ack_tx);
    }
    let payload = FilePayload {
        dir: Some(true),
        group: Some(tag.group.clone()),
        rel: (!bare).then(|| tag.rel.clone()),
        group_end: Some(tag.end),
        auto_paste: Some(auto_paste),
        ..FilePayload::start(&id, name, 0)
    };
    link.queue(payload, (!tag.end).then(|| format!("Sending folder {}...", tag.rel)));
    if !wants_ack {
        return;
    }
    match ack_rx.recv_timeout(ACK_TIMEOUT) {
        Ok(true) if tag.end && bare => link.status("Sent: all files".into()),
        Ok(true) if tag.end => link.status(format!("Sent: {}", tag.rel)),
        Ok(true) => {}
        Ok(false) => link.status(format!("Could not send folder {}", tag.rel)),
        Err(_) => {
            forget_ack(&id);
            link.status(format!("Could not send folder {}: no confirmation from the receiver", tag.rel));
        }
    }
}

/// What the transfer list last showed for one sent file, so it is updated only when something changed.
#[derive(Default, PartialEq)]
struct Shown {
    percent: u64,
    finishing: bool,
}

impl Shown {
    /// Shows `delivered` of `size` bytes: active with a percentage, or finishing once every byte reached the
    /// relay (the receiver still writes and verifies it).
    fn update(&mut self, link: &Link, id: &str, label: &str, delivered: u64, size: u64) {
        let delivered = delivered.min(size);
        let now = Shown { percent: percent(delivered, size), finishing: delivered == size };
        if now == *self {
            return;
        }
        if now.finishing {
            link.progress(id, label, size, size, TransferState::Verifying);
            link.status(format!("Finishing: {label}"));
        } else {
            link.progress(id, label, delivered, size, TransferState::Active);
            link.status(format!("Sending {label} ({}%)...", now.percent));
        }
        *self = now;
    }
}

/// Kill switch: `OPENDESK_FILE_DEFLATE=0` sends every chunk raw.
fn deflate_enabled() -> bool {
    std::env::var("OPENDESK_FILE_DEFLATE").map_or(true, |v| v != "0")
}

/// How long a sender whose transfer was cut waits for the receiver to take it up again (`file-resume`).
const RESUME_WAIT: Duration = Duration::from_secs(60);
/// A resume `start` is repeated this often until the receiver answers (the relay may still be reconnecting).
const RESUME_RETRY: Duration = Duration::from_secs(3);
/// Most resumes of one file; a link that keeps dropping fails the transfer after that.
const MAX_RESUMES: u32 = 8;

/// Kill switch: `OPENDESK_FILE_RESUME=0` fails cut transfers as before.
fn resume_enabled() -> bool {
    std::env::var("OPENDESK_FILE_RESUME").map_or(true, |v| v != "0")
}

/// How one pass over a file's bytes, or the wait for its verdict, ended.
enum Pass {
    /// Every byte and `end` are queued / the receiver confirmed the file.
    Done,
    /// The connection or the P2P path dropped, or the receiver lost the transfer and asked to resume it.
    Cut,
    /// Failed for good; the string is the status shown.
    Failed(String),
}

/// The bytes of one file in flight under its current transfer ID (it changes on every resume).
struct Leg {
    id: String,
    sent: u64,
    hash: Sha256,
    delivery: Delivery,
}

impl Leg {
    fn mark(&self, done: u64) -> FileMark {
        FileMark { key: self.delivery.0.clone(), done }
    }
}

/// Sends one file and waits for its verdict. With a receiver listing `file-resume`, a transfer cut by a lost
/// relay connection or P2P path continues from the receiver's last byte under a new transfer ID (see
/// [`resume`]); `started` follows the connection it continues on.
fn send_file(link: &Link, item: SendItem, started: &mut u64, auto_paste: bool) {
    let SendItem { path, id, tag } = item;
    // Only pulled offers come with the receiver's transfer ID; paste streams up to 1.3.x read JSON chunks only.
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
    let base = id.unwrap_or_else(|| format!("ft_{}", now_micros()));
    logger::info("file_transfer", &format!("Sending id={base} name={label} bytes={size} auto_paste={auto_paste}"));

    // Sampled per file: the partner (and its caps) may change while a background transfer runs.
    let caps = caps_of(&link.target);
    let wants_ack = caps.file_ack;
    let binary = caps.file_binary && link.relay_binary && (!pulled || caps.file_pull_bin);
    let chunk_size = if binary { BINARY_CHUNK_SIZE } else { CHUNK_SIZE };
    // A paste stream belongs to the pasting application, which gives up when the connection drops.
    let resumable = !pulled && wants_ack && caps.file_resume && resume_enabled();
    let mut resumes = Resumes::listen(&link.target, &base, resumable);
    let (ack_tx, ack_rx) = mpsc::channel();
    expect_ack(&base, ack_tx.clone());

    let mut start = FilePayload::start(&base, &wire_name, size);
    super::Stamp::of_path(&path).put_on(&mut start);
    if let Some(t) = &tag {
        start.group = Some(t.group.clone());
        start.rel = Some(t.rel.clone());
    }
    // Progress counts what reached the relay, not what was queued: up to the whole queue may still be on its
    // way, which showed as "stuck at 99%".
    let mut leg = Leg {
        id: base.clone(),
        sent: 0,
        hash: Sha256::new(),
        delivery: Delivery::track(format!("{}:{base}", link.target), 0),
    };
    link.queue(start.clone(), Some(format!("Sending {label} (0%)...")));
    link.progress(&base, &label, 0, size, TransferState::Active);

    let fast = super::fast_path(&link.target);
    let mut squeeze = Squeeze::new(&wire_name, binary && caps.file_deflate && deflate_enabled(), Instant::now());
    let (mut raw_bytes, mut wire_bytes) = (0u64, 0u64);
    let mut buf = vec![0u8; chunk_size];
    let mut shown = Shown::default();
    let mut resumed = 0;
    let verdict = loop {
        // The bytes, then the verdict; either may be cut.
        let mut pass = Pass::Done;
        while leg.sent < size {
            // An early NACK (receiver rejected start) stops sending chunks the relay would drop anyway.
            if ack_rx.try_recv() == Ok(false) {
                pass = Pass::Failed(format!("Could not send {label}: rejected by the receiver"));
                break;
            }
            if epoch() != *started || resumes.asked(&leg.id) {
                pass = Pass::Cut;
                break;
            }
            let mut waited = false;
            while IN_FLIGHT_CHUNKS.load(Ordering::Relaxed) > MAX_QUEUED_CHUNKS && epoch() == *started {
                waited = true;
                thread::sleep(Duration::from_millis(8));
                shown.update(link, &base, &label, leg.delivery.bytes(), size);
            }
            if epoch() != *started {
                pass = Pass::Cut;
                break;
            }
            // Never past the announced size: a file still being written (a log) is sent as it was at the start.
            let want = (size - leg.sent).min(chunk_size as u64) as usize;
            match file.read(&mut buf[..want]) {
                Ok(0) => {
                    pass = Pass::Failed(format!("Could not send {label}: the file got shorter"));
                    break;
                }
                Ok(n) => {
                    leg.hash.update(&buf[..n]);
                    let offset = leg.sent;
                    leg.sent += n as u64;
                    let packed = squeeze.pack(&buf[..n], waited, Instant::now());
                    raw_bytes += n as u64;
                    wire_bytes += packed.as_ref().map_or(n, Vec::len) as u64;
                    link.queue_chunk(binary, &leg.id, offset, &buf[..n], packed, leg.mark(leg.sent));
                    shown.update(link, &base, &label, leg.delivery.bytes(), size);
                    // The relay path spaces chunks out behind video; a direct path paces itself (`domain::bulk`).
                    if !fast {
                        thread::sleep(Duration::from_millis(3));
                    }
                }
                Err(e) => {
                    logger::error("file_transfer", &format!("Read failed for {path:?}: {e}"));
                    pass = Pass::Failed(format!("Could not send {label}: read error"));
                    break;
                }
            }
        }
        if let Pass::Done = pass {
            let sha = hex(&leg.hash.clone().finalize());
            link.queue_marked(
                FilePayload::end(&leg.id, &wire_name, sha, auto_paste),
                Some(format!("Waiting for confirmation: {label}")),
                Some(leg.mark(size)),
            );
            if !wants_ack {
                break Pass::Done;
            }
            pass = await_verdict(link, &ack_rx, &mut resumes, &leg, *started, &mut shown, &base, &label, size);
        }
        match pass {
            Pass::Cut if resumable && resumed < MAX_RESUMES => {
                resumed += 1;
                forget_ack(&leg.id);
                let Some((id, offset)) = resume(link, &mut resumes, &start, &leg.id, &base, resumed, &label) else {
                    break Pass::Failed(format!("Could not send {label}: connection lost"));
                };
                let Some(hash) = rewind(&mut file, offset, size) else {
                    break Pass::Failed(format!("Could not send {label}: the file changed"));
                };
                logger::info("file_transfer", &format!("Resuming {label} as id={id} at byte {offset} of {size}."));
                expect_ack(&id, ack_tx.clone());
                leg =
                    Leg { delivery: Delivery::track(format!("{}:{id}", link.target), offset), id, sent: offset, hash };
                *started = epoch();
            }
            Pass::Cut => break Pass::Failed(format!("Could not send {label}: connection lost")),
            done => break done,
        }
    };
    if wire_bytes < raw_bytes {
        logger::info(
            "file_transfer",
            &format!(
                "Deflated id={base}: {raw_bytes} -> {wire_bytes} bytes ({}%)",
                wire_bytes * 100 / raw_bytes.max(1)
            ),
        );
    }
    match verdict {
        Pass::Done if wants_ack => {
            logger::info("file_transfer", &format!("Sent and confirmed: {label}"));
            link.progress(&base, &label, size, size, TransferState::Done);
            link.status(format!("Sent: {label}"));
        }
        Pass::Done => {
            // Receivers without `file-ack` (Android) send no verdict: "sent" only means every byte left this socket.
            forget_ack(&leg.id);
            while IN_FLIGHT_CHUNKS.load(Ordering::Relaxed) > 0 {
                if epoch() != *started {
                    return link.fail(&base, &label, size, format!("Could not send {label}: connection lost"));
                }
                thread::sleep(Duration::from_millis(50));
            }
            link.progress(&base, &label, size, size, TransferState::Done);
            link.status(format!("Sent (not confirmed by the receiver): {label}"));
        }
        Pass::Failed(status) => {
            logger::warn("file_transfer", &format!("Transfer failed for {path:?}: {status}"));
            forget_ack(&leg.id);
            link.fail(&base, &label, size, status);
        }
        Pass::Cut => unreachable!("cut transfers resume or fail inside the loop"),
    }
}

/// Waits for the receiver's verdict on `leg`; bytes still on their way keep counting, and once all reached
/// the relay the row says "Finishing" while the receiver writes, syncs and verifies the file.
#[allow(clippy::too_many_arguments)]
fn await_verdict(
    link: &Link,
    ack_rx: &mpsc::Receiver<bool>,
    resumes: &mut Resumes,
    leg: &Leg,
    started: u64,
    shown: &mut Shown,
    row: &str,
    label: &str,
    size: u64,
) -> Pass {
    let deadline = Instant::now() + ACK_TIMEOUT;
    loop {
        match ack_rx.recv_timeout(PROGRESS_POLL) {
            Ok(true) => return Pass::Done,
            Ok(false) => {
                logger::warn("file_transfer", &format!("Transfer rejected: {label}"));
                return Pass::Failed(format!("Could not send {label}: rejected or checksum mismatch"));
            }
            Err(RecvTimeoutError::Timeout) if epoch() != started || resumes.asked(&leg.id) => return Pass::Cut,
            Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {
                shown.update(link, row, label, leg.delivery.bytes(), size);
            }
            Err(_) => {
                logger::warn("file_transfer", &format!("Transfer confirmation timed out: {label}"));
                return Pass::Failed(format!("Could not send {label}: no confirmation from the receiver"));
            }
        }
    }
}

/// Takes a cut transfer up again: sends `start` again under a new ID naming the cut one (`resumeOf`) until the
/// receiver answers where it stands (`pull` with `resumeAt`). `None` when it never does within [`RESUME_WAIT`].
fn resume(
    link: &Link,
    resumes: &mut Resumes,
    start: &FilePayload,
    cut: &str,
    base: &str,
    n: u32,
    label: &str,
) -> Option<(String, u64)> {
    // A new ID: the relay keeps the cut transfer's grant until it expires and refuses a second `start` for it.
    let id = format!("{base}_r{n}");
    let payload = FilePayload { id: id.clone(), resume_of: Some(cut.to_string()), ..start.clone() };
    logger::info("file_transfer", &format!("Transfer id={cut} was cut; asking to resume it as id={id}."));
    link.status(format!("Connection lost; resuming {label}..."));
    let deadline = Instant::now() + RESUME_WAIT;
    let mut retry = Instant::now();
    while Instant::now() < deadline {
        if Instant::now() >= retry {
            link.queue(payload.clone(), None);
            retry = Instant::now() + RESUME_RETRY;
        }
        if let Some(offset) = resumes.answer(&id, Duration::from_millis(250)) {
            return Some((id, offset));
        }
    }
    logger::warn("file_transfer", &format!("No answer to resuming id={cut}; giving up."));
    None
}

/// Re-reads the first `offset` bytes of `file` into a fresh hash and leaves the file there. `None` when the
/// file is shorter now or `offset` lies past the announced `size`.
fn rewind(file: &mut File, offset: u64, size: u64) -> Option<Sha256> {
    if offset > size {
        return None;
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut hash = Sha256::new();
    let copied = std::io::copy(&mut Read::by_ref(file).take(offset), &mut HashWriter(&mut hash)).ok()?;
    (copied == offset).then_some(hash)
}

/// Feeds written bytes into a hash.
struct HashWriter<'a>(&'a mut Sha256);

impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::capability::PeerCaps;
    use crate::domain::file_squeeze;
    use crate::protocol::FileAction;
    use crate::service::file_transfer::receiver::{FileReceiver, tests::TEST_DOWNLOADS};
    use crate::service::file_transfer::tree::tests::temp_tree;
    use crate::service::file_transfer::{now_ms, without_echoes};
    use crate::service::test_support;
    use std::{env, fs};

    /// Drag-and-drop path end to end: sender messages fed straight into a receiver. Sets the clipboard, so run
    /// it by hand: `cargo test --release folder_send -- --ignored`
    #[test]
    #[ignore]
    fn folder_send_round_trip_recreates_tree() {
        let src = temp_tree("send");
        let downloads = env::temp_dir().join(format!("opendesk-recv-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(downloads.join("Project")).unwrap(); // existing folder: the copy must become "Project (1)"
        *TEST_DOWNLOADS.lock() = Some(downloads.clone());
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
        assert_eq!(std::fs::read(copy.join("src/main.rs")).unwrap(), b"fn main() {}");
        assert_eq!(fs::read(copy.join("src/deep/x.txt")).unwrap(), b"x");
        assert!(copy.join("empty").is_dir());
        assert_eq!(fs::read(downloads.join("loose.txt")).unwrap(), b"12345");
        assert_eq!(fs::read_dir(downloads.join("Project")).unwrap().count(), 0, "existing folder untouched");
        let published = vec![copy.clone(), downloads.join("loose.txt")];
        assert!(without_echoes("222222222", published).is_empty(), "received items are never sent back");
        *TEST_DOWNLOADS.lock() = None;
        let _ = fs::remove_dir_all(src);
        let _ = fs::remove_dir_all(downloads);
    }

    /// FN8: the receiver loses a transfer halfway (its connection dropped) and asks to resume it; the file
    /// continues from the receiver's last byte under a new ID and arrives byte-identical. Bytes the sender
    /// queued for the cut ID meanwhile are dropped, as the relay would.
    #[test]
    fn a_cut_transfer_resumes_where_the_receiver_stands() {
        let peer = "710000009";
        let dir = env::temp_dir().join(format!("opendesk-resume-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.bin");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8).collect();
        fs::write(&path, &data).unwrap();
        let caps = PeerCaps { file_ack: true, file_binary: true, file_resume: true, ..PeerCaps::default() };
        super::super::set_session_caps(peer, caps);
        let (ctx, rx) = test_support::context_with_outbox();
        ctx.relay.caps.lock().file_binary = true;
        send_files(&ctx, vec![path.clone()], peer.into(), false);

        let mut receiver = FileReceiver::new();
        let (mut first, mut cut, mut current) = (None::<String>, false, String::new());
        let (mut chunks_seen, mut resumed_at) = (0, None);
        let key = loop {
            let msg = rx.recv_timeout(Duration::from_secs(10)).expect("sender stalled");
            match msg {
                Outgoing::File { message: ClientMessage::RelayFile { payload, .. }, .. } => {
                    IN_FLIGHT_CHUNKS.fetch_sub(1, Ordering::SeqCst);
                    if payload.action == FileAction::End {
                        assert_ne!(Some(&payload.id), first.as_ref(), "the cut ID must not end");
                        let key = format!("{peer}:{}", payload.id);
                        break (key, payload);
                    }
                    if payload.action == FileAction::Start {
                        first.get_or_insert(payload.id.clone());
                        current = payload.id.clone();
                    }
                    let (status, ack) = receiver.handle(&ctx, &payload, peer, false);
                    assert_ne!(ack, Some(false), "rejected: {status:?}");
                }
                Outgoing::FileChunk { frame, .. } => {
                    IN_FLIGHT_CHUNKS.fetch_sub(chunk_units(frame.len()), Ordering::SeqCst);
                    let chunk = binary::parse_file_chunk(&frame).unwrap();
                    if cut && Some(chunk.id) == first.as_deref() {
                        continue; // the relay drops chunks of the cut transfer
                    }
                    let (status, ack) = receiver.handle_chunk(&ctx, &chunk, peer);
                    assert_ne!(ack, Some(false), "chunk rejected: {status:?}");
                    chunks_seen += 1;
                    if chunks_seen == 8 && !cut {
                        // The receiver's connection drops: it keeps the file and asks to resume once back.
                        cut = true;
                        receiver.abort_all(ctx.ui.as_ref());
                        receiver.ask_resumes(&ctx, Instant::now());
                    }
                }
                Outgoing::Message(ClientMessage::RelayFile { payload, .. }) => {
                    // Receiver -> sender: what the relay would deliver to the sender's dispatcher.
                    let offset = payload.resume_at.expect("only resume pulls go this way");
                    if payload.id != *first.as_ref().unwrap() {
                        resumed_at = Some(offset);
                    }
                    super::super::deliver_resume(peer, &payload.id, offset);
                }
                _ => {}
            }
        };
        let (key, end) = key;
        assert!(current.ends_with("_r1"), "resumed under a new ID: {current}");
        assert_eq!(resumed_at, Some(8 * BINARY_CHUNK_SIZE as u64), "resumed at the receiver's last byte");
        assert_eq!(end.sha256.unwrap(), hex(&Sha256::digest(&data)), "sender hash covers the whole file");
        let entry = receiver.staged(&key).expect("resumed transfer is active");
        assert_eq!(fs::read(&entry).unwrap(), data, "staged bytes differ");
        super::super::deliver_ack(&end.id, true);
        let _ = fs::remove_file(entry);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn task_queue_runs_everything_in_order_on_at_most_limit_threads() {
        use std::sync::atomic::AtomicUsize;
        static QUEUE: TaskQueue = TaskQueue::new("test-queue", 2);
        static RUNNING: AtomicUsize = AtomicUsize::new(0);
        static PEAK: AtomicUsize = AtomicUsize::new(0);
        let (done_tx, done_rx) = mpsc::channel();
        for i in 0..12 {
            let done_tx = done_tx.clone();
            QUEUE.submit(Box::new(move || {
                let now = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
                PEAK.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(20));
                RUNNING.fetch_sub(1, Ordering::SeqCst);
                let _ = done_tx.send(i);
            }));
        }
        let mut started: Vec<i32> = (0..12).map(|_| done_rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert_eq!(PEAK.load(Ordering::SeqCst), 2, "ran more (or fewer) than 2 at once");
        // Two workers may finish neighbouring tasks in either order, but nothing jumps the queue.
        for (pos, i) in started.iter().enumerate() {
            assert!((*i - pos as i32).abs() <= 1, "task {i} finished at {pos}");
        }
        started.sort();
        assert_eq!(started, (0..12).collect::<Vec<_>>());
        // Idle workers exit; a later task starts a new one.
        thread::sleep(Duration::from_millis(50));
        assert_eq!(QUEUE.state.lock().workers, 0);
        QUEUE.submit(Box::new(move || {
            let _ = done_tx.send(99);
        }));
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 99);
    }

    /// Several loose files go to a `file-group` receiver as one group closed by a bare end; one file, older
    /// receivers and names the folder rules reject keep the one-by-one path.
    #[test]
    fn loose_files_travel_as_one_group_only_to_receivers_that_know_it() {
        let paths = vec![PathBuf::from("x").join("a.txt"), PathBuf::from("y").join("b.pdf")];
        let items = group_loose(paths.clone(), true);
        let tags: Vec<(&str, bool, bool)> =
            items.iter().map(|i| i.tag.as_ref().map(|t| (t.rel.as_str(), t.dir, t.end)).unwrap()).collect();
        assert_eq!(tags, vec![("a.txt", false, false), ("b.pdf", false, false), ("", true, true)]);
        let group = &items[0].tag.as_ref().unwrap().group;
        assert!(items.iter().all(|i| &i.tag.as_ref().unwrap().group == group));
        assert!(group_loose(paths.clone(), false).iter().all(|i| i.tag.is_none()), "1.3.x receivers");
        assert!(group_loose(paths[..1].to_vec(), true).iter().all(|i| i.tag.is_none()), "a single file");
        let odd = vec![PathBuf::from("x").join("CON"), PathBuf::from("x").join("b.pdf")];
        assert!(group_loose(odd, true).iter().all(|i| i.tag.is_none()), "a device name stays sanitized");
    }

    #[test]
    fn task_queue_survives_a_panicking_task() {
        static QUEUE: TaskQueue = TaskQueue::new("test-queue-panic", 1);
        QUEUE.submit(Box::new(|| panic!("deliberate")));
        let (tx, rx) = mpsc::channel();
        QUEUE.submit(Box::new(move || {
            let _ = tx.send(());
        }));
        rx.recv_timeout(Duration::from_secs(5)).expect("queue stalled after a panic");
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
                    let (offset, bytes) = if chunk.offset & OFFSET_DEFLATED != 0 {
                        (chunk.offset & !OFFSET_DEFLATED, file_squeeze::unpack(chunk.data, BINARY_CHUNK_SIZE).unwrap())
                    } else {
                        (chunk.offset, chunk.data.to_vec())
                    };
                    assert_eq!((to.as_str(), offset), (target, got.len() as u64));
                    assert!(bytes.len() <= BINARY_CHUNK_SIZE);
                    got.extend_from_slice(&bytes);
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
        // Compressible data: much smaller toward a `file-deflate` receiver, the same bytes after inflating.
        let text: Vec<u8> = (0..400_000u32)
            .flat_map(|i| {
                format!(
                    "line {} of a log file
",
                    i % 977
                )
                .into_bytes()
            })
            .collect();
        super::super::set_session_caps(
            "710000003",
            PeerCaps { file_binary: true, file_deflate: true, ..PeerCaps::default() },
        );
        let (plain, _) = send_and_collect("710000001", &text, true);
        let (deflated, _) = send_and_collect("710000003", &text, true);
        println!("text {} B: binary {plain} B, deflated {deflated} B", text.len());
        assert!(deflated * 4 < plain, "{deflated} vs {plain}");
        assert!(binary_wire < data.len() + data.len() / 100 && json_wire > data.len() * 4 / 3);
        super::super::remove_session_caps("710000001");
        super::super::remove_session_caps("710000002");
    }
}
