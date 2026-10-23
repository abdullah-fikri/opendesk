use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use core_engine::codec::{from_base64, to_base64};
use core_engine::input::InputInjector;
use sha2::{Digest, Sha256};
use serde_json::{json, Value};
use slint::Weak;

use crate::logger;
use crate::protocol::OutgoingMsg;
use crate::MainWindow;

const CHUNK_SIZE: usize = 32768; // 32 KB per chunk for smooth non-blocking WebSocket flow
/// Incoming transfers with no chunk for this long are abandoned and their staging file deleted.
const RECEIVE_IDLE: Duration = Duration::from_secs(60);
/// Completed transfer IDs are remembered this long to reject replays.
const COMPLETED_TTL: Duration = Duration::from_secs(600);
/// Sender waits this long for the receiver's final ACK after queueing the last message.
const ACK_TIMEOUT: Duration = Duration::from_secs(120);

/// Tracks in-flight file transfer chunks in `tx_out` so large files apply backpressure
/// and UI progress reflects actual network transmission rather than in-memory queueing.
pub static IN_FLIGHT_FILE_CHUNKS: AtomicUsize = AtomicUsize::new(0);

/// Timestamp (epoch ms) of the last completed incoming file paste so Clipboard Watcher never echoes it back.
pub static LAST_FILE_RECV_MS: AtomicU64 = AtomicU64::new(0);

/// Paths this app itself put on the clipboard after receiving them, so copy-paste never sends them back.
static RECEIVED_CLIPBOARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// True if the clipboard holds exactly the files we just received (provenance, not timing).
pub fn is_received_clipboard(paths: &[PathBuf]) -> bool {
    let ours = RECEIVED_CLIPBOARD.lock().unwrap();
    !paths.is_empty()
        && paths.len() == ours.len()
        && paths.iter().all(|p| ours.iter().any(|o| o.eq_ignore_ascii_case(&p.to_string_lossy())))
}

/// Bumped when the relay connection drops; senders abort transfers started on an older connection.
pub static TRANSFER_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Whether the current session partner advertised `file-ack` (old clients never send ACKs).
pub static PEER_FILE_ACK: AtomicBool = AtomicBool::new(false);

/// Whether the current partner advertised `file-offer`: copied files are offered and pulled on paste (AnyDesk style).
pub static PEER_FILE_OFFER: AtomicBool = AtomicBool::new(false);

/// Files we offered, as (offer id, partner, paths); a pull may only ask for these. Newest last, a few kept.
static OUR_OFFERS: Mutex<Vec<(String, String, Vec<PathBuf>)>> = Mutex::new(Vec::new());
const MAX_OFFER_FILES: usize = 100;

/// Senders waiting for a receiver ACK, keyed by transfer ID.
static PENDING_ACKS: Mutex<Vec<(String, Sender<bool>)>> = Mutex::new(Vec::new());

/// Delivers a receiver ACK to the waiting sender thread, if any.
pub fn deliver_ack(id: &str, ok: bool) {
    let mut pending = PENDING_ACKS.lock().unwrap();
    if let Some(pos) = pending.iter().position(|(pid, _)| pid == id) {
        let (_, tx) = pending.swap_remove(pos);
        let _ = tx.send(ok);
    }
}

/// Fails every in-progress send after the relay connection is lost (grants die with the connection).
pub fn abort_pending_sends() {
    TRANSFER_EPOCH.fetch_add(1, Ordering::SeqCst);
    PENDING_ACKS.lock().unwrap().clear();
}

fn set_status(win: &Option<Weak<MainWindow>>, text: String) {
    if let Some(w) = win {
        let _ = w.upgrade_in_event_loop(move |win| win.set_status_text(text.into()));
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn matching_checksum(hash: Sha256, expected: Option<&str>) -> bool {
    let actual: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    expected == Some(actual.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_accepts_empty_file_and_rejects_corruption_or_missing_hash() {
        let empty = Sha256::new();
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(matching_checksum(empty.clone(), Some(digest)));
        assert!(!matching_checksum(empty.clone(), None));
        assert!(!matching_checksum(empty, Some(&"0".repeat(64))));
    }

    #[test]
    fn link_unique_never_overwrites_existing_file() {
        let dir = env::temp_dir().join(format!("opendesk-test-{}", now_epoch_ms()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), b"old").unwrap();
        let staging = dir.join(".part");
        fs::write(&staging, b"new").unwrap();
        let saved = link_unique(&staging, &dir, "a.txt").unwrap();
        assert_eq!(saved, dir.join("a (1).txt"));
        assert_eq!(fs::read(dir.join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(saved).unwrap(), b"new");
        let _ = fs::remove_dir_all(dir);
    }
}

struct Incoming {
    file: File,
    path: PathBuf,
    name: String,
    received: u64,
    total: u64,
    hash: Sha256,
    last_activity: Instant,
}

pub struct FileReceiver {
    /// Keyed by `sender:transfer_id` so two senders cannot collide or hijack each other's transfer.
    active_files: HashMap<String, Incoming>,
    completed_ids: HashMap<String, Instant>,
}

impl FileReceiver {
    pub fn new() -> Self {
        Self {
            active_files: HashMap::new(),
            completed_ids: HashMap::new(),
        }
    }

    pub fn get_staging_dir() -> PathBuf {
        if let Ok(profile) = env::var("USERPROFILE") {
            let dl = PathBuf::from(profile).join("Downloads").join("OpenDesk");
            if fs::create_dir_all(&dl).is_ok() {
                return dl;
            }
        }
        let fallback = crate::config::get_app_dir().join("Downloads");
        let _ = fs::create_dir_all(&fallback);
        fallback
    }

    fn discard(&mut self, key: &str) -> Option<String> {
        let entry = self.active_files.remove(key)?;
        drop(entry.file);
        let _ = fs::remove_file(&entry.path);
        Some(entry.name)
    }

    /// Drops transfers that stopped receiving chunks and forgets old completed IDs.
    /// Returns a status line for the last abandoned transfer.
    pub fn expire_stale(&mut self, now: Instant) -> Option<String> {
        self.completed_ids
            .retain(|_, done| now.duration_since(*done) < COMPLETED_TTL);
        let stale: Vec<String> = self
            .active_files
            .iter()
            .filter(|(_, f)| now.duration_since(f.last_activity) >= RECEIVE_IDLE)
            .map(|(k, _)| k.clone())
            .collect();
        let mut status = None;
        for key in stale {
            if let Some(name) = self.discard(&key) {
                logger::warn("app.tunnel_ft_comp", &format!("Incoming transfer timed out: {}", name));
                status = Some(format!("Gagal menerima {}: pengirim berhenti mengirim", name));
            }
        }
        status
    }

    /// Deletes every partial file; called when the relay connection drops because grants die with it.
    pub fn abort_all(&mut self) -> Option<String> {
        let keys: Vec<String> = self.active_files.keys().cloned().collect();
        let mut status = None;
        for key in keys {
            if let Some(name) = self.discard(&key) {
                status = Some(format!("Gagal menerima {}: koneksi relay terputus", name));
            }
        }
        status
    }

    /// Returns `(status_text, ack)`. `ack` is `Some(ok)` when the sender must be told the final result.
    pub fn handle_payload(
        &mut self,
        payload: &Value,
        from: &str,
        injector: &InputInjector,
        last_clip_seq: &Arc<AtomicU32>,
        allow_auto_paste: bool,
    ) -> (Option<String>, Option<bool>) {
        let (Some(action), Some(id)) = (
            payload.get("action").and_then(|v| v.as_str()),
            payload.get("id").and_then(|v| v.as_str()),
        ) else {
            return (None, None);
        };
        // Bytes for a paste stream we requested go straight to Explorer, never to Downloads.
        if let Some(result) = crate::virtual_clip::on_transfer(from, action, id, payload) {
            return result;
        }
        let key = format!("{}:{}", from, id);

        if self.completed_ids.contains_key(&key) {
            return (None, None);
        }

        match action {
            "start" => {
                if self.active_files.contains_key(&key) {
                    return (None, None);
                }
                if self.active_files.len() >= 8 {
                    return (Some("Gagal menerima berkas: terlalu banyak transfer aktif".into()), Some(false));
                }
                let raw_name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("opendesk_file.bin");
                let Some(total_size) = payload.get("size").and_then(|v| v.as_u64()) else {
                    return (None, None);
                };
                if id.len() > 80
                    || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                    || total_size > 2 * 1024 * 1024 * 1024
                    || raw_name.is_empty()
                    || raw_name == "."
                    || raw_name == ".."
                    || raw_name.len() > 200
                    || raw_name.chars().any(|c| c.is_control())
                {
                    return (None, Some(false));
                }
                let clean_name: String = raw_name
                    .chars()
                    .map(|c| match c {
                        '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                        _ => c,
                    })
                    .collect();

                let dir = Self::get_staging_dir();
                // Keep incomplete bytes away from the destination, and never truncate an existing file.
                let staging_path = dir.join(format!(".opendesk-{}-{}.part", id, now_epoch_ms()));
                return match OpenOptions::new().write(true).create_new(true).open(&staging_path) {
                    Ok(file) => {
                        logger::info(
                            "app.tunnel_ft_comp",
                            &format!(
                                "Receiving file transfer id={} name={} size={}",
                                id, clean_name, total_size
                            ),
                        );
                        self.active_files.insert(
                            key,
                            Incoming {
                                file,
                                path: staging_path,
                                name: clean_name.clone(),
                                received: 0,
                                total: total_size,
                                hash: Sha256::new(),
                                last_activity: Instant::now(),
                            },
                        );
                        (Some(format!("Menerima {}...", clean_name)), None)
                    }
                    Err(_) => (Some(format!("Gagal menerima {}: tidak bisa membuat file", clean_name)), Some(false)),
                };
            }
            "chunk" => {
                let Some(entry) = self.active_files.get_mut(&key) else {
                    return (None, None);
                };
                let bytes = payload
                    .get("chunk")
                    .and_then(|v| v.as_str())
                    .filter(|b64| b64.len() <= 48_000)
                    .and_then(from_base64);
                let ok = match bytes {
                    Some(bytes)
                        if bytes.len() <= CHUNK_SIZE
                            && entry.received.saturating_add(bytes.len() as u64) <= entry.total
                            && entry.file.write_all(&bytes).is_ok() =>
                    {
                        entry.hash.update(&bytes);
                        entry.received += bytes.len() as u64;
                        entry.last_activity = Instant::now();
                        true
                    }
                    _ => false,
                };
                if !ok {
                    let name = self.discard(&key).unwrap_or_default();
                    return (Some(format!("Gagal menerima {}", name)), Some(false));
                }
                if let Some(prog) = entry_progress(entry) {
                    return (Some(prog), None);
                }
            }
            "end" => {
                let auto_paste = payload
                    .get("autoPaste")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let Some(Incoming { mut file, path: staging_path, name: clean_name, received, total: total_size, hash, .. }) =
                    self.active_files.remove(&key)
                else {
                    // Unknown transfer (expired or rejected start): NACK so the sender does not wait for a timeout.
                    return (None, Some(false));
                };
                let expected = payload.get("sha256").and_then(|v| v.as_str());
                let written = received == total_size && matching_checksum(hash, expected) && file.flush().is_ok() && file.sync_all().is_ok();
                drop(file);
                if !written {
                    let _ = fs::remove_file(staging_path);
                    return (Some(format!("Gagal menerima {}: ukuran atau SHA-256 tidak cocok", clean_name)), Some(false));
                }
                // Conflict policy: never overwrite; save as "name (n).ext" instead.
                let Some(target_path) = link_unique(&staging_path, &Self::get_staging_dir(), &clean_name) else {
                    let _ = fs::remove_file(staging_path);
                    return (Some(format!("Gagal menerima {}: gagal menyimpan", clean_name)), Some(false));
                };
                let _ = fs::remove_file(staging_path);

                self.completed_ids.insert(key, Instant::now());
                let finished_ms = now_epoch_ms();
                LAST_FILE_RECV_MS.store(finished_ms, Ordering::SeqCst);

                let abs_path = target_path.to_string_lossy().to_string();
                *RECEIVED_CLIPBOARD.lock().unwrap() = vec![abs_path.clone()];
                injector.set_clipboard_files(&[abs_path.clone()]);
                last_clip_seq.store(injector.get_clipboard_sequence_number(), Ordering::SeqCst);

                if auto_paste && allow_auto_paste {
                    thread::sleep(Duration::from_millis(35));
                    injector.send_key_combo(true, false, false, 0x56); // Ctrl+V into open Explorer folder
                    thread::sleep(Duration::from_millis(25));
                    // Refresh clipboard sequence again after Explorer processes Ctrl+V so watcher never echoes back!
                    last_clip_seq.store(injector.get_clipboard_sequence_number(), Ordering::SeqCst);
                    LAST_FILE_RECV_MS.store(now_epoch_ms(), Ordering::SeqCst);
                }

                logger::info(
                    "app.tunnel_ft_comp",
                    &format!(
                        "File transfer complete: {} (CF_HDROP set, auto_paste={})",
                        abs_path,
                        auto_paste && allow_auto_paste
                    ),
                );
                return (Some(format!("Selesai: {} (Siap Paste / Downloads\\OpenDesk)", clean_name)), Some(true));
            }
            _ => {}
        }
        (None, None)
    }
}

/// Hard-links `staging` to the first free `name`, `stem (1).ext`, ... in `dir`; hard_link never replaces an existing file.
fn link_unique(staging: &Path, dir: &Path, name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    for n in 0..100 {
        let candidate = if n == 0 { dir.join(name) } else { dir.join(format!("{stem} ({n}){ext}")) };
        match fs::hard_link(staging, &candidate) {
            Ok(()) => return Some(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

fn entry_progress(entry: &Incoming) -> Option<String> {
    (entry.total > 0).then(|| {
        let pct = ((entry.received * 100) / entry.total).min(99);
        format!("Menerima {} ({}%)...", entry.name, pct)
    })
}

pub fn send_files_async(
    paths: Vec<PathBuf>,
    target_clean: String,
    tx_out: Sender<OutgoingMsg>,
    win_opt: Option<Weak<MainWindow>>,
    auto_paste: bool,
) {
    send_with_ids(paths.into_iter().map(|p| (p, None)).collect(), target_clean, tx_out, win_opt, auto_paste);
}

/// `Some(id)` reuses a transfer ID chosen by the receiver (clipboard pull), so it can route the bytes to its paste stream.
fn send_with_ids(
    items: Vec<(PathBuf, Option<String>)>,
    target_clean: String,
    tx_out: Sender<OutgoingMsg>,
    win_opt: Option<Weak<MainWindow>>,
    auto_paste: bool,
) {
    if target_clean.is_empty() || items.is_empty() {
        return;
    }

    thread::spawn(move || {
        let epoch = TRANSFER_EPOCH.load(Ordering::SeqCst);
        for (path, forced_id) in items {
            if TRANSFER_EPOCH.load(Ordering::SeqCst) != epoch {
                set_status(&win_opt, "Gagal mengirim berkas: koneksi terputus".into());
                return;
            }
            if !path.is_file() {
                continue;
            }

            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file.bin")
                .to_string();

            let mut file = match File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    logger::error(
                        "app.tunnel_ft_comp",
                        &format!("Failed to open file {:?}: {}", path, e),
                    );
                    set_status(&win_opt, format!("Gagal membuka {}", file_name));
                    continue;
                }
            };

            let file_size = match file.metadata() {
                Ok(m) if m.len() <= 2 * 1024 * 1024 * 1024 => m.len(),
                _ => {
                    set_status(&win_opt, format!("Gagal mengirim {}: ukuran melebihi 2GB", file_name));
                    continue;
                }
            };
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros();
            let transfer_id = forced_id.unwrap_or_else(|| format!("ft_{}", ts));

            logger::info(
                "app.tunnel_ft_comp",
                &format!(
                    "Starting file transfer id={} name={} bytes={} auto_paste={}",
                    transfer_id, file_name, file_size, auto_paste
                ),
            );

            // Sampled per file: the session partner (and its caps) may change while a background transfer runs.
            let expect_ack = PEER_FILE_ACK.load(Ordering::SeqCst);
            let (ack_tx, ack_rx) = mpsc::channel();
            PENDING_ACKS.lock().unwrap().push((transfer_id.clone(), ack_tx));

            let start_msg = json!({
                "type": "relay-file",
                "targetClean": target_clean,
                "payload": {
                    "action": "start",
                    "id": transfer_id,
                    "name": file_name,
                    "size": file_size
                }
            });
            IN_FLIGHT_FILE_CHUNKS.fetch_add(1, Ordering::SeqCst);
            let _ = tx_out.send(OutgoingMsg::FileTransfer {
                msg: start_msg,
                status_text: Some(format!("Mengirim {} (0%)...", file_name)),
            });

            let mut buf = vec![0u8; CHUNK_SIZE];
            let mut hash = Sha256::new();
            let mut sent_bytes = 0u64;
            let mut chunk_idx = 0usize;
            let mut aborted = false;

            loop {
                // An early NACK (receiver rejected start) stops sending chunks the relay would drop anyway.
                if TRANSFER_EPOCH.load(Ordering::SeqCst) != epoch || ack_rx.try_recv() == Ok(false) {
                    aborted = true;
                    break;
                }
                // Bounded backpressure: wait if >48 chunks (~1.5 MB) are still waiting in network queue
                while IN_FLIGHT_FILE_CHUNKS.load(Ordering::Relaxed) > 48 {
                    if TRANSFER_EPOCH.load(Ordering::SeqCst) != epoch {
                        aborted = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(8));
                }
                if aborted {
                    break;
                }

                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        hash.update(&buf[..n]);
                        sent_bytes += n as u64;
                        chunk_idx += 1;
                        let b64 = to_base64(&buf[..n]);
                        let chunk_msg = json!({
                            "type": "relay-file",
                            "targetClean": target_clean,
                            "payload": {
                                "action": "chunk",
                                "id": transfer_id,
                                "chunk": b64
                            }
                        });

                        let status_update = if file_size > 0 && (chunk_idx % 4 == 0 || sent_bytes == file_size) {
                            let pct = ((sent_bytes * 100) / file_size).min(99);
                            Some(format!("Mengirim {} ({}%)...", file_name, pct))
                        } else {
                            None
                        };

                        IN_FLIGHT_FILE_CHUNKS.fetch_add(1, Ordering::SeqCst);
                        let _ = tx_out.send(OutgoingMsg::FileTransfer {
                            msg: chunk_msg,
                            status_text: status_update,
                        });

                        thread::sleep(Duration::from_millis(3));
                    }
                    Err(e) => {
                        logger::error("app.tunnel_ft_comp", &format!("Read failed for {:?}: {}", path, e));
                        aborted = true;
                        break;
                    }
                }
            }

            if aborted || sent_bytes != file_size {
                logger::error("app.tunnel_ft_comp", &format!("Transfer incomplete for {:?}", path));
                let mut pending = PENDING_ACKS.lock().unwrap();
                if let Some(pos) = pending.iter().position(|(pid, _)| pid == &transfer_id) {
                    pending.swap_remove(pos);
                }
                set_status(&win_opt, format!("Gagal mengirim {}", file_name));
                continue;
            }

            let checksum: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
            let end_msg = json!({
                "type": "relay-file",
                "targetClean": target_clean,
                "payload": {
                    "action": "end",
                    "id": transfer_id,
                    "name": file_name,
                    "sha256": checksum,
                    "autoPaste": auto_paste
                }
            });
            IN_FLIGHT_FILE_CHUNKS.fetch_add(1, Ordering::SeqCst);
            let _ = tx_out.send(OutgoingMsg::FileTransfer {
                msg: end_msg,
                status_text: Some(format!("Menunggu konfirmasi: {}", file_name)),
            });

            logger::info(
                "app.tunnel_ft_comp",
                &format!("Queued final chunk for file transfer id={} name={}", transfer_id, file_name),
            );

            // Wait for receiver ACK (or queue drain fallback for legacy peers)
            if expect_ack {
                match ack_rx.recv_timeout(ACK_TIMEOUT) {
                    Ok(true) => {
                        logger::info("app.tunnel_ft_comp", &format!("Transfer ACK received: {}", file_name));
                        set_status(&win_opt, format!("Terkirim: {}", file_name));
                    }
                    Ok(false) => {
                        logger::warn("app.tunnel_ft_comp", &format!("Transfer NACK received: {}", file_name));
                        set_status(&win_opt, format!("Gagal mengirim {}: ditolak atau checksum tidak cocok", file_name));
                    }
                    Err(_) => {
                        let mut pending = PENDING_ACKS.lock().unwrap();
                        if let Some(pos) = pending.iter().position(|(pid, _)| pid == &transfer_id) {
                            pending.swap_remove(pos);
                        }
                        logger::warn("app.tunnel_ft_comp", &format!("Transfer ACK timeout: {}", file_name));
                        set_status(&win_opt, format!("Gagal mengirim {}: tidak ada konfirmasi penerima", file_name));
                    }
                }
            } else {
                let mut pending = PENDING_ACKS.lock().unwrap();
                if let Some(pos) = pending.iter().position(|(pid, _)| pid == &transfer_id) {
                    pending.swap_remove(pos);
                }
                let mut lost = false;
                while IN_FLIGHT_FILE_CHUNKS.load(Ordering::Relaxed) > 0 {
                    if TRANSFER_EPOCH.load(Ordering::SeqCst) != epoch {
                        lost = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                // Legacy receivers send no ACK: "Terkirim" here only means every byte left this socket.
                set_status(&win_opt, if lost {
                    format!("Gagal mengirim {}: koneksi terputus", file_name)
                } else {
                    format!("Terkirim (tanpa konfirmasi penerima): {}", file_name)
                });
            }
        }
    });
}

/// Offers copied files to the partner (names and sizes only). Returns false when nothing could be offered.
pub fn offer_files(paths: &[PathBuf], target_clean: &str, tx_out: &Sender<OutgoingMsg>) -> bool {
    let files: Vec<(PathBuf, String, u64)> = paths
        .iter()
        .filter_map(|p| {
            let size = fs::metadata(p).ok().filter(|m| m.is_file() && m.len() <= 2 * 1024 * 1024 * 1024)?.len();
            Some((p.clone(), p.file_name()?.to_str()?.to_string(), size))
        })
        .take(MAX_OFFER_FILES)
        .collect();
    if files.is_empty() || target_clean.is_empty() {
        return false;
    }
    let offer_id = format!("of_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros());
    let listing: Vec<Value> = files.iter().map(|(_, name, size)| json!({ "name": name, "size": size })).collect();
    {
        let mut offers = OUR_OFFERS.lock().unwrap();
        offers.push((offer_id.clone(), target_clean.to_string(), files.into_iter().map(|(p, _, _)| p).collect()));
        let excess = offers.len().saturating_sub(4);
        offers.drain(..excess);
    }
    logger::info("app.clip_comp", &format!("Relaying file offer {} ({} files).", offer_id, listing.len()));
    let _ = tx_out.send(OutgoingMsg::Json(json!({
        "type": "relay-file",
        "targetClean": target_clean,
        "payload": { "action": "offer", "id": offer_id, "files": listing }
    })));
    true
}

/// Partner pasted one of our offered files: send it under the transfer ID it chose.
pub fn serve_pull(from: &str, payload: &Value, tx_out: &Sender<OutgoingMsg>, win_opt: Option<Weak<MainWindow>>) {
    let offer_id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let index = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as usize;
    let Some(tid) = payload
        .get("tid")
        .and_then(|v| v.as_str())
        .filter(|t| t.len() <= 80 && t.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'))
    else {
        return;
    };
    let path = OUR_OFFERS
        .lock()
        .unwrap()
        .iter()
        .find(|(id, partner, _)| id == offer_id && partner == from)
        .and_then(|(_, _, paths)| paths.get(index).cloned());
    match path {
        Some(path) => {
            logger::info("app.ft_src_session", &format!("Pull for offer {} file {} ({}).", offer_id, index, tid));
            send_with_ids(vec![(path, Some(tid.to_string()))], from.to_string(), tx_out.clone(), win_opt, false);
        }
        None => logger::warn("app.ft_src_session", &format!("Rejected pull for unknown offer {} file {}.", offer_id, index)),
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    #[test]
    fn received_clipboard_matches_exact_file_set_only() {
        *RECEIVED_CLIPBOARD.lock().unwrap() = vec![r"C:\Users\a\Downloads\OpenDesk\x.txt".into()];
        assert!(is_received_clipboard(&[PathBuf::from(r"c:\users\a\downloads\opendesk\X.TXT")]));
        assert!(!is_received_clipboard(&[PathBuf::from(r"C:\other.txt")]));
        assert!(!is_received_clipboard(&[]));
        assert!(!is_received_clipboard(&[
            PathBuf::from(r"C:\Users\a\Downloads\OpenDesk\x.txt"),
            PathBuf::from(r"C:\other.txt"),
        ]));
    }
}
