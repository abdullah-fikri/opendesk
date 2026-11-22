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
use crate::domain::file_route::Path as FilePath;
use crate::domain::received_files::ReceivedFiles;
use crate::domain::transfer::Limits;
use crate::infra::{autostart, logger};
use crate::protocol::{FileMark, FilePayload, OfferEntry};

pub use receiver::FileReceiver;
pub use sender::send_browsed;

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
    file_pull_bin: false,
    cursor_shape: false,
    mouse_relative: false,
    text_focus: false,
    clipboard_image: false,
    file_p2p: false,
    file_deflate: false,
    file_resume: false,
    file_drop: false,
    file_channel: false,
    video_delta: false,
    video_h264: false,
    h264_eager: false,
    tile_lossless: false,
    video_nack: false,
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

/// Sends a copied image (PNG) to `partner`, which listed `clipboard-image`; it goes on its clipboard as is.
pub fn send_clipboard_image(ctx: &crate::service::context::AppContext, partner: &str, png: &[u8]) {
    let id = format!("img-{}", SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis()));
    logger::info("clipboard", &format!("Sending copied image ({} KB PNG) to {partner}.", png.len() / 1024));
    ctx.send(crate::protocol::ClientMessage::file(partner, crate::protocol::FilePayload::clipboard_image(&id, png)));
}

/// Capabilities of `target`: its viewer session's when it has one, else the single partner's.
pub fn caps_of(target: &str) -> PeerCaps {
    SESSION_CAPS.lock().iter().find(|(k, _)| k == target).map(|(_, c)| *c).unwrap_or_else(|| *PARTNER_CAPS.lock())
}

/// Whether this machine advertises `file-offer`, or why not. Offers need Explorer's virtual files: Windows only.
/// The service's SYSTEM agent places them through its clipboard helper (the user's Explorer does not pull from
/// SYSTEM's clipboard objects), so it needs the helper connected.
pub fn local_file_offer() -> Result<(), &'static str> {
    if !cfg!(windows) {
        Err("virtual clipboard files need Windows")
    } else if autostart::supervised_by_service() && !helper_ready() {
        Err("SYSTEM service agent without a connected clipboard helper")
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn helper_ready() -> bool {
    crate::infra::clipboard_helper::ready()
}

#[cfg(not(windows))]
fn helper_ready() -> bool {
    false
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

/// Size limits toward `target`: builds that list `file-channel` take files over 2 GiB (see
/// [`Limits::for_partner`]); Android and older desktops keep the old ones.
pub fn limits_to(target: &str) -> Limits {
    Limits::for_partner(caps_of(target).file_channel)
}

/// How new transfers to each partner travel right now; see [`set_route`].
static ROUTES: Mutex<Vec<(String, Route)>> = Mutex::new(Vec::new());

/// The relay connection's view of one partner's file path, refreshed every 250 ms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Route {
    /// The path a transfer starting now takes.
    pub path: FilePath,
    /// The file channel was lost and is being set up again (`infra::file_channel` redials): a cut transfer waits
    /// a moment for it instead of resuming on a slower path for the rest of the file.
    pub channel_redialing: bool,
}

/// Records how new transfers to `target` travel: a direct path paces itself (`domain::bulk`, TCP) instead of being
/// spaced out for the relay, and the file channel may keep far more in flight.
pub fn set_route(target: &str, route: Route) {
    let mut all = ROUTES.lock();
    all.retain(|(t, _)| t != target);
    if route != Route::default() {
        all.push((target.to_string(), route));
    }
}

fn route_of(target: &str) -> Route {
    ROUTES.lock().iter().find(|(t, _)| t == target).map(|(_, r)| *r).unwrap_or_default()
}

/// True while new transfers to `target` take a direct path (P2P or the file channel).
pub(super) fn fast_path(target: &str) -> bool {
    route_of(target).path != FilePath::Relay
}

/// Most file messages queued toward `target`, in [`chunk_units`], before its sender waits. The relay path keeps
/// ~1.5 MB so a file never queues far ahead of video and progress follows the network. The file channel is TCP of
/// its own with 8 MB socket buffers on each of its streams; 16 MB here keeps every stream busy on a long fat
/// path (1 Gbit/s at 100 ms holds 12.5 MB) instead of starving it between two sender wake-ups.
pub(super) fn in_flight_limit(target: &str) -> usize {
    match route_of(target).path {
        FilePath::Channel => MAX_IN_FLIGHT_CHANNEL,
        _ => MAX_IN_FLIGHT,
    }
}

/// See [`in_flight_limit`]: relay and P2P path (~1.5 MB).
const MAX_IN_FLIGHT: usize = 48;
/// See [`in_flight_limit`]: the file channel (16 MB).
const MAX_IN_FLIGHT_CHANNEL: usize = (16 << 20) / CHUNK_SIZE;

/// True while the file channel to `target` is being set up again after it was lost.
pub(super) fn channel_redialing(target: &str) -> bool {
    route_of(target).channel_redialing
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
    /// Starts tracking `key`, whose first `done` bytes already arrived (a resumed transfer).
    fn track(key: String, done: u64) -> Self {
        DELIVERED.lock().push((key.clone(), done));
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

/// Senders of resumable transfers (`file-resume`): (target, first transfer ID, channel for `(id, resumeAt)`).
static RESUMES: Mutex<Vec<(String, String, ResumeTx)>> = Mutex::new(Vec::new());
/// Carries `(transfer ID, resumeAt)` to a sender thread.
type ResumeTx = Sender<(String, u64)>;

/// A receiver's `pull` with `resumeAt`: it lost transfer `id` and asks for it again, or it answers a resume
/// `start` (`id` is then the new ID). Only the sender of that transfer to `from` hears it.
pub fn deliver_resume(from: &str, id: &str, offset: u64) {
    let resumes = RESUMES.lock();
    let ours = |base: &str| id == base || id.strip_prefix(base).is_some_and(|rest| rest.starts_with("_r"));
    match resumes.iter().find(|(target, base, _)| target == from && ours(base)) {
        Some((_, _, tx)) => {
            let _ = tx.send((id.to_string(), offset));
        }
        None => logger::info("file_transfer", &format!("Resume request from {from} for unknown id={id}; ignored.")),
    }
}

/// Resume messages for one file being sent, while it lives.
pub(super) struct Resumes {
    key: Option<(String, String)>,
    rx: std::sync::mpsc::Receiver<(String, u64)>,
}

impl Resumes {
    /// Listens for resume messages about the file sent as `base` to `target`; `on` false listens to nothing.
    pub(super) fn listen(target: &str, base: &str, on: bool) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let key = on.then(|| {
            RESUMES.lock().push((target.to_string(), base.to_string(), tx));
            (target.to_string(), base.to_string())
        });
        Self { key, rx }
    }

    /// True when the receiver asked to resume transfer `id` (it lost it). Messages about other IDs are stale.
    pub(super) fn asked(&mut self, id: &str) -> bool {
        self.rx.try_iter().any(|(got, _)| got == id)
    }

    /// The receiver's answer to resuming as `id`: where it stands. Waits up to `wait`.
    pub(super) fn answer(&mut self, id: &str, wait: std::time::Duration) -> Option<u64> {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok((got, offset)) if got == id => return Some(offset),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }
}

impl Drop for Resumes {
    fn drop(&mut self) {
        if let Some((target, base)) = &self.key {
            RESUMES.lock().retain(|(t, b, _)| !(t == target && b == base));
        }
    }
}

pub(crate) fn expect_ack(id: &str, tx: Sender<bool>) {
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

/// Last write time in Unix milliseconds (`mtime` on offers and transfer starts).
pub(super) fn mtime_ms(meta: &std::fs::Metadata) -> Option<u64> {
    unix_ms(meta.modified())
}

fn unix_ms(t: std::io::Result<std::time::SystemTime>) -> Option<u64> {
    let since = t.ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since.as_millis()).ok()
}

/// Windows attributes a transfer carries: read-only, hidden, archive. System and the rest stay behind.
pub(crate) const CARRIED_ATTRS: u32 = 0x1 | 0x2 | 0x20;

/// What a file keeps across a transfer besides its bytes: the three times and the carried attributes.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct Stamp {
    pub mtime: Option<u64>,
    pub ctime: Option<u64>,
    pub atime: Option<u64>,
    pub attrs: Option<u32>,
}

impl Stamp {
    pub(super) fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(windows)]
        let attrs = Some(std::os::windows::fs::MetadataExt::file_attributes(meta) & CARRIED_ATTRS).filter(|a| *a != 0);
        #[cfg(not(windows))]
        let attrs = meta.permissions().readonly().then_some(0x1);
        Self { mtime: mtime_ms(meta), ctime: unix_ms(meta.created()), atime: unix_ms(meta.accessed()), attrs }
    }

    pub(super) fn of_path(path: &std::path::Path) -> Self {
        std::fs::metadata(path).map(|m| Self::of(&m)).unwrap_or_default()
    }

    pub(super) fn entry(self, name: String, size: Option<u64>, dir: Option<bool>) -> OfferEntry {
        let Self { mtime, ctime, atime, attrs } = self;
        OfferEntry { name, size, dir, mtime, ctime, atime, attrs }
    }

    pub(super) fn put_on(self, start: &mut FilePayload) {
        (start.mtime, start.ctime, start.atime, start.attrs) = (self.mtime, self.ctime, self.atime, self.attrs);
    }

    pub(super) fn from_payload(p: &FilePayload) -> Self {
        Self { mtime: p.mtime, ctime: p.ctime, atime: p.atime, attrs: p.attrs.map(|a| a & CARRIED_ATTRS) }
    }

    /// Applies the times to an open file (failures are cosmetic and ignored).
    pub(super) fn apply_times(&self, file: &std::fs::File) {
        let at = |ms: u64| std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms);
        let mut times = std::fs::FileTimes::new();
        if let Some(ms) = self.mtime {
            times = times.set_modified(at(ms));
        }
        if let Some(ms) = self.atime {
            times = times.set_accessed(at(ms));
        }
        #[cfg(windows)]
        if let Some(ms) = self.ctime {
            times = std::os::windows::fs::FileTimesExt::set_created(times, at(ms));
        }
        let _ = file.set_times(times);
    }

    /// Applies the attributes to a saved file; read-only last, since nothing may be written after it.
    pub(super) fn apply_attrs(&self, path: &std::path::Path) {
        let Some(attrs) = self.attrs.map(|a| a & CARRIED_ATTRS).filter(|a| *a != 0) else { return };
        crate::infra::file_attrs::set(path, attrs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn received_file_keeps_sender_times_and_attributes() {
        let dir = std::env::temp_dir().join(format!("opendesk-stamp-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let (src, dst) = (dir.join("src.txt"), dir.join("dst.txt"));
        fs::write(&src, b"x").unwrap();
        fs::write(&dst, b"x").unwrap();
        let file = fs::File::options().write(true).open(&src).unwrap();
        let old = UNIX_EPOCH + std::time::Duration::from_millis(1_600_000_000_000);
        let times = fs::FileTimes::new().set_modified(old).set_accessed(old);
        #[cfg(windows)]
        let times = std::os::windows::fs::FileTimesExt::set_created(times, old);
        file.set_times(times).unwrap();
        drop(file);
        crate::infra::file_attrs::set(&src, 0x2); // hidden
        let mut start = FilePayload::default();
        Stamp::of_path(&src).put_on(&mut start);
        let wire: FilePayload = serde_json::from_str(&serde_json::to_string(&start).unwrap()).unwrap();
        let stamp = Stamp::from_payload(&wire);
        stamp.apply_times(&fs::File::options().write(true).open(&dst).unwrap());
        stamp.apply_attrs(&dst);
        let got = Stamp::of_path(&dst);
        assert_eq!(got.mtime, Some(1_600_000_000_000));
        #[cfg(windows)]
        {
            assert_eq!(got.ctime, Some(1_600_000_000_000));
            assert_eq!(got.attrs.map(|a| a & 0x2), Some(0x2));
        }
        let _ = fs::remove_dir_all(&dir);
    }

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
        let delivery = Delivery::track("710000007:ft_9".into(), 0);
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
