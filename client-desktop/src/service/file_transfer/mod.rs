//! File transfer between session partners over the relay (see `docs/features/file-transfer.md`).
//!
//! - [`sender`]: sends dropped, picked or pasted files and folders in 128 KiB binary chunks (`file-binary`),
//!   else in 32 KiB base64 chunks.
//! - [`receiver`]: stages incoming files, verifies SHA-256 and publishes them to the clipboard.
//! - [`offer`]: clipboard file offers (copy here, paste there) and serving pulls.
//! - [`tree`]: folder walking and collision-free file creation.
//!
//! Transfers are authorized by the relay (a grant per transfer) and outlive their screen session only as long
//! as the relay connection; a lost connection fails every transfer on it.

pub mod offer;
pub mod receiver;
pub mod sender;
pub mod tree;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::domain::capability::PeerCaps;
use crate::domain::received_files::ReceivedFiles;
use crate::infra::{autostart, logger};
use crate::protocol::FileMark;

pub use receiver::FileReceiver;

/// Chunk size of JSON (base64) chunks; also the most a receiver accepts per JSON chunk.
pub const CHUNK_SIZE: usize = 32 * 1024;
/// Chunk size of binary chunks (`file-binary`); also the most a receiver accepts per binary chunk. Large
/// enough to make per-message costs negligible, small enough that one chunk holds input on the shared socket
/// for only a moment.
pub const BINARY_CHUNK_SIZE: usize = 128 * 1024;
/// Largest base64 chunk a receiver accepts (32 KiB encodes to 43 692 characters).
pub const MAX_CHUNK_B64: usize = 48_000;

/// File messages queued in the relay connection but not yet written, in units of [`CHUNK_SIZE`] (see
/// [`chunk_units`]). Senders wait while it is high, so a large file applies backpressure and progress reflects
/// the network, not the queue.
pub static IN_FLIGHT_CHUNKS: AtomicUsize = AtomicUsize::new(0);
/// When (epoch ms) the last file arrived; the viewer status line stays on the transfer for a moment.
pub static LAST_RECEIVED_MS: AtomicU64 = AtomicU64::new(0);
/// Bumped when the relay connection drops; senders abort transfers started on an older connection.
static TRANSFER_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Items received from each partner, so copy-paste never sends them back to it.
static RECEIVED: Mutex<ReceivedFiles> = Mutex::new(ReceivedFiles::new());
/// File bytes of each transfer being sent that the relay has read, keyed `target:id` (see [`set_delivered`]).
static DELIVERED: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());
/// Senders waiting for a receiver ACK, keyed by transfer ID.
static PENDING_ACKS: Mutex<Vec<(String, Sender<bool>)>> = Mutex::new(Vec::new());
/// File capabilities of each viewed host (a viewer of several hosts must not mix them up).
static SESSION_CAPS: Mutex<Vec<(String, PeerCaps)>> = Mutex::new(Vec::new());
/// Capabilities of the single partner (hosted viewer, or the only session of a single-session relay).
static PARTNER_CAPS: Mutex<PeerCaps> = Mutex::new(PeerCaps {
    file_ack: false,
    file_offer: false,
    file_folder: false,
    file_group: false,
    file_binary: false,
    cursor_shape: false,
    video_delta: false,
    video_h264: false,
    frame_stamp: false,
    audio_opus: false,
    audio_system: false,
    p2p: false,
    e2e: false,
    privacy_screen: false,
    block_input: false,
    remote_mic: false,
    max_fps: 0,
    performance: crate::domain::performance::Performance::Medium,
});

/// Weight of one queued file message of `len` bytes in [`IN_FLIGHT_CHUNKS`]: a JSON message counts 1, a binary
/// chunk one per 32 KiB it carries, so the queue limits keep meaning the same number of bytes.
pub fn chunk_units(len: usize) -> usize {
    (len / CHUNK_SIZE).max(1)
}

/// Records the capabilities of viewed host `id`.
pub fn set_session_caps(id: &str, caps: PeerCaps) {
    let mut all = SESSION_CAPS.lock();
    all.retain(|(k, _)| k != id);
    all.push((id.to_string(), caps));
}

/// Forgets viewed host `id`.
pub fn remove_session_caps(id: &str) {
    SESSION_CAPS.lock().retain(|(k, _)| k != id);
}

/// Records the capabilities of the single partner.
pub fn set_partner_caps(caps: PeerCaps) {
    *PARTNER_CAPS.lock() = caps;
}

/// Capabilities of `target`: its viewer session's when it has one, else the single partner's.
pub fn caps_of(target: &str) -> PeerCaps {
    SESSION_CAPS.lock().iter().find(|(k, _)| k == target).map(|(_, c)| *c).unwrap_or_else(|| *PARTNER_CAPS.lock())
}

/// Whether this machine advertises `file-offer`, or why not. Offers need Explorer's virtual files: Windows only,
/// and not from the service's SYSTEM agent, whose clipboard objects the user's Explorer cannot pull from
/// reliably.
pub fn local_file_offer() -> Result<(), &'static str> {
    if !cfg!(windows) {
        Err("virtual clipboard files need Windows")
    } else if autostart::supervised_by_service() {
        Err("SYSTEM service agent; the user's Explorer cannot pull from its clipboard")
    } else {
        Ok(())
    }
}

/// `on`, or `off (reason)`, for logs.
fn on_off(state: Result<(), &str>) -> String {
    match state {
        Ok(()) => "on".into(),
        Err(reason) => format!("off ({reason})"),
    }
}

/// Logs once per session how copied files travel in each direction with `partner` and why, so a fallback is
/// never silent. `relay_offer` is whether the relay listed `file-offer`; `hosting` is our role.
pub fn log_session_path(partner: &str, caps: &PeerCaps, relay_offer: bool, hosting: bool) {
    let ours = local_file_offer();
    let out = match (caps.file_offer, hosting) {
        (true, _) => "offered (names only; pulled on paste)",
        (false, true) => "sent on copy (fallback: the viewer cannot take offers)",
        (false, false) => "sent on Ctrl+V in the session",
    };
    let incoming = match (ours.is_ok() && relay_offer, hosting) {
        (true, _) => "offered to us",
        (false, true) => "sent on Ctrl+V in the session",
        (false, false) => "sent on copy (fallback: we cannot take offers)",
    };
    logger::info(
        "file_transfer",
        &format!(
            "File path with {partner}: our copies are {out}; theirs are {incoming}. Partner file-offer={}, relay \
             file-offer={relay_offer}, this machine file-offer={}, folders={}, binary={}, ack={}.",
            caps.file_offer,
            on_off(ours),
            caps.file_folder,
            caps.file_binary,
            caps.file_ack
        ),
    );
}

/// Logs what happens to copied `files` for `partner` and why: one line per clipboard file event (copy seen by
/// the watcher, Ctrl+V in a session, focus change). `decision` is `offer`, `send` or `skip`.
pub fn log_clipboard_files(partner: &str, files: &[PathBuf], decision: &str, reason: &str, hosting: bool) {
    let (mut count, mut folders, mut bytes) = (0usize, 0usize, 0u64);
    for meta in files.iter().filter_map(|p| std::fs::metadata(p).ok()) {
        if meta.is_dir() {
            folders += 1;
        } else {
            count += 1;
            bytes += meta.len();
        }
    }
    logger::info(
        "clipboard",
        &format!(
            "Clipboard files for {partner}: {decision} ({reason}). {count} files ({bytes} B), {folders} folders; \
             partner file-offer={}, this machine file-offer={}, role={}, service agent={}.",
            caps_of(partner).file_offer,
            on_off(local_file_offer()),
            if hosting { "host" } else { "viewer" },
            autostart::supervised_by_service()
        ),
    );
}

/// Size of a file, `None` for a folder (or a path that is gone), as [`ReceivedFiles`] wants it.
fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len())
}

/// Copied `paths` for `partner` without the ones it sent us (see `domain::received_files`); each one left out
/// is logged with the reason. Only clipboard paths go through this: a drop or the file picker sends anything.
pub fn without_echoes(partner: &str, paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let now = Instant::now();
    let received = RECEIVED.lock();
    paths
        .into_iter()
        .filter(|path| match received.echo(partner, &path.to_string_lossy(), file_size(path), now) {
            Some(echo) => {
                logger::info(
                    "clipboard",
                    &format!("Not sending {} back to {partner}: {}.", path.display(), echo.reason()),
                );
                false
            }
            None => true,
        })
        .collect()
}

/// Remembers `paths` (just put on the clipboard) as received from `partner`.
fn remember_received(partner: &str, paths: &[PathBuf]) {
    let now = Instant::now();
    let mut received = RECEIVED.lock();
    for path in paths {
        received.record(partner, &path.to_string_lossy(), file_size(path), now);
    }
}

/// Records how far a sent transfer has reached the relay (from the relay connection); transfers no sender
/// tracks any more are ignored.
pub fn set_delivered(mark: &FileMark) {
    if let Some((_, done)) = DELIVERED.lock().iter_mut().find(|(key, _)| *key == mark.key) {
        *done = (*done).max(mark.done);
    }
}

/// File bytes of transfer `key` (`target:id`) the relay has read.
fn delivered(key: &str) -> u64 {
    DELIVERED.lock().iter().find(|(k, _)| k == key).map_or(0, |(_, done)| *done)
}

/// Tracks the delivery of one sent transfer while it lives.
struct Delivery(String);

impl Delivery {
    fn track(key: String) -> Self {
        DELIVERED.lock().push((key.clone(), 0));
        Self(key)
    }

    fn bytes(&self) -> u64 {
        delivered(&self.0)
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        DELIVERED.lock().retain(|(key, _)| *key != self.0);
    }
}

/// Delivers a receiver ACK to the waiting sender thread, if any.
pub fn deliver_ack(id: &str, ok: bool) {
    let mut pending = PENDING_ACKS.lock();
    if let Some(pos) = pending.iter().position(|(pid, _)| pid == id) {
        let (_, tx) = pending.swap_remove(pos);
        let _ = tx.send(ok);
    }
}

fn expect_ack(id: &str, tx: Sender<bool>) {
    PENDING_ACKS.lock().push((id.to_string(), tx));
}

fn forget_ack(id: &str) {
    PENDING_ACKS.lock().retain(|(pid, _)| pid != id);
}

/// Fails every send in progress after the relay connection is lost (grants die with the connection).
pub fn abort_pending_sends() {
    TRANSFER_EPOCH.fetch_add(1, Ordering::SeqCst);
    PENDING_ACKS.lock().clear();
}

fn epoch() -> u64 {
    TRANSFER_EPOCH.load(Ordering::SeqCst)
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn now_micros() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros()
}

/// Lowercase hex of a SHA-256 digest.
fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn received_files_are_left_out_only_for_their_sender() {
        let folder = PathBuf::from(r"C:\Users\a\Downloads\OpenDesk\Project-710000009");
        remember_received("710000009", std::slice::from_ref(&folder));
        let copied =
            vec![PathBuf::from(r"c:\users\a\downloads\opendesk\PROJECT-710000009\x.txt"), "C:\\other.txt".into()];
        assert_eq!(without_echoes("710000009", copied.clone()), vec![PathBuf::from(r"C:\other.txt")]);
        assert_eq!(without_echoes("710000008", copied.clone()), copied, "another partner may get them");
        assert!(without_echoes("710000009", vec![folder]).is_empty());
    }

    #[test]
    fn delivery_counts_only_tracked_transfers_and_never_goes_back() {
        let mark = |done| FileMark { key: "710000007:ft_9".into(), done };
        set_delivered(&mark(10));
        assert_eq!(delivered("710000007:ft_9"), 0, "nobody sends it");
        let delivery = Delivery::track("710000007:ft_9".into());
        set_delivered(&mark(300));
        set_delivered(&mark(200)); // an older mark confirmed late
        assert_eq!(delivery.bytes(), 300);
        drop(delivery);
        assert_eq!(delivered("710000007:ft_9"), 0, "forgotten once the send ends");
    }

    #[test]
    fn session_caps_override_the_partner_caps() {
        set_partner_caps(PeerCaps { file_ack: true, ..PeerCaps::default() });
        set_session_caps("123123123", PeerCaps { file_offer: true, ..PeerCaps::default() });
        assert!(caps_of("123123123").file_offer && !caps_of("123123123").file_ack);
        assert!(caps_of("999999999").file_ack);
        remove_session_caps("123123123");
        assert!(caps_of("123123123").file_ack);
    }
}
