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

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::domain::capability::PeerCaps;

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

/// Paths this app put on the clipboard after receiving them, so copy-paste never sends them back.
static RECEIVED_CLIPBOARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Senders waiting for a receiver ACK, keyed by transfer ID.
static PENDING_ACKS: Mutex<Vec<(String, Sender<bool>)>> = Mutex::new(Vec::new());
/// File capabilities of each viewed host (a viewer of several hosts must not mix them up).
static SESSION_CAPS: Mutex<Vec<(String, PeerCaps)>> = Mutex::new(Vec::new());
/// Capabilities of the single partner (hosted viewer, or the only session of a single-session relay).
static PARTNER_CAPS: Mutex<PeerCaps> = Mutex::new(PeerCaps {
    file_ack: false,
    file_offer: false,
    file_folder: false,
    file_binary: false,
    cursor_shape: false,
    video_delta: false,
    video_h264: false,
    audio_opus: false,
    p2p: false,
    e2e: false,
    privacy_screen: false,
    block_input: false,
    max_fps: 0,
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

/// True if the clipboard holds exactly the files we just received (provenance, not timing).
pub fn is_received_clipboard(paths: &[PathBuf]) -> bool {
    let ours = RECEIVED_CLIPBOARD.lock();
    !paths.is_empty()
        && paths.len() == ours.len()
        && paths.iter().all(|p| ours.iter().any(|o| o.eq_ignore_ascii_case(&p.to_string_lossy())))
}

fn remember_received(paths: Vec<String>) {
    *RECEIVED_CLIPBOARD.lock() = paths;
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
    fn received_clipboard_matches_exact_file_set_only() {
        remember_received(vec![r"C:\Users\a\Downloads\OpenDesk\x.txt".into()]);
        assert!(is_received_clipboard(&[PathBuf::from(r"c:\users\a\downloads\opendesk\X.TXT")]));
        assert!(!is_received_clipboard(&[PathBuf::from(r"C:\other.txt")]));
        assert!(!is_received_clipboard(&[]));
        assert!(!is_received_clipboard(&[
            PathBuf::from(r"C:\Users\a\Downloads\OpenDesk\x.txt"),
            PathBuf::from(r"C:\other.txt")
        ]));
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
