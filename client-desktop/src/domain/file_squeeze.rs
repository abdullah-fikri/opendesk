//! Adaptive compression of binary file chunks (`file-deflate`, FN9).
//!
//! A chunk travels deflated only when that saves at least an eighth of it; the receiver sees the
//! [`OFFSET_DEFLATED`] bit in the chunk's offset and inflates it before anything else reads it. The flag lives in
//! the offset so the relay, the E2E routing header and the P2P bulk stream carry such chunks unchanged.
//!
//! The sender stops trying when it does not pay:
//! - names that are already compressed (zip, mp4, jpg, ...) never try;
//! - three incompressible chunks in a row pause it for [`PAUSE_INCOMPRESSIBLE`] chunks, then one chunk probes
//!   again (a file can change its nature: an installer with a text header and a packed payload);
//! - when the link outruns the compressor (the sender never waited for the queue in a window while compression
//!   took most of its time) it pauses for [`PAUSE_CPU`] chunks: on a fast LAN raw bytes arrive sooner.

use std::time::{Duration, Instant};

use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::decompress_to_vec_with_limit;

/// Offset bit of a deflated chunk; real offsets stay below 2 GB ([`super::transfer::MAX_FILE_BYTES`]).
pub const OFFSET_DEFLATED: u64 = 1 << 63;
/// Deflate level: the fastest, which still halves text, logs, source code and office XML.
const LEVEL: u8 = 1;
/// Consecutive chunks saving less than an eighth before the sender stops trying.
const STRIKES: u32 = 3;
/// Chunks sent raw after [`STRIKES`] (8 MB of 128 KiB chunks).
const PAUSE_INCOMPRESSIBLE: u32 = 64;
/// Chunks per window of the CPU rule.
const WINDOW: u32 = 32;
/// Chunks sent raw when the compressor held the link back (32 MB).
const PAUSE_CPU: u32 = 256;

/// Extensions of formats that are compressed already; deflate would only burn CPU on them.
const PACKED: &[&str] = &[
    "7z", "aac", "apk", "avi", "avif", "br", "bz2", "cab", "deb", "docx", "flac", "gif", "gz", "heic", "iso", "jar",
    "jpeg", "jpg", "lz", "lz4", "lzma", "m4a", "m4v", "mkv", "mov", "mp3", "mp4", "msi", "ogg", "opus", "png", "pptx",
    "rar", "rpm", "tgz", "txz", "webm", "webp", "whl", "wmv", "xlsx", "xz", "zip", "zst",
];

/// Whether a file called `name` is worth trying to compress.
pub fn worth_trying(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((_, ext)) => !PACKED.iter().any(|p| p.eq_ignore_ascii_case(ext)),
        None => true,
    }
}

/// Per-file compression state of a sender.
pub struct Squeeze {
    on: bool,
    strikes: u32,
    /// Chunks still sent raw before the next probe.
    paused: u32,
    /// CPU rule: chunks, time spent compressing and whether the sender waited, in the current window.
    window: u32,
    busy: Duration,
    waited: bool,
    since: Instant,
}

impl Squeeze {
    /// `partner` is whether the receiver lists `file-deflate`.
    pub fn new(name: &str, partner: bool, now: Instant) -> Self {
        Self {
            on: partner && worth_trying(name),
            strikes: 0,
            paused: 0,
            window: 0,
            busy: Duration::ZERO,
            waited: false,
            since: now,
        }
    }

    /// The bytes to send for `data`: `Some(deflated)` when compression paid, `None` to send `data` raw.
    /// `waited` is whether the sender had to wait for the connection queue before this chunk.
    pub fn pack(&mut self, data: &[u8], waited: bool, now: Instant) -> Option<Vec<u8>> {
        if !self.on || data.is_empty() {
            return None;
        }
        self.waited |= waited;
        self.window += 1;
        if self.window >= WINDOW {
            self.end_window(now);
        }
        if self.paused > 0 {
            self.paused -= 1;
            return None;
        }
        let t = Instant::now();
        let packed = compress_to_vec(data, LEVEL);
        self.busy += t.elapsed();
        if packed.len() <= data.len() - data.len() / 8 {
            self.strikes = 0;
            Some(packed)
        } else {
            self.strikes += 1;
            if self.strikes >= STRIKES {
                self.strikes = 0;
                self.paused = PAUSE_INCOMPRESSIBLE;
            }
            None
        }
    }

    fn end_window(&mut self, now: Instant) {
        let wall = now.saturating_duration_since(self.since);
        if self.cpu_bound(wall) {
            self.paused = self.paused.max(PAUSE_CPU);
        }
        self.window = 0;
        self.busy = Duration::ZERO;
        self.waited = false;
        self.since = now;
    }

    /// The link never made the sender wait and compressing took over half of the window: it is the bottleneck.
    fn cpu_bound(&self, wall: Duration) -> bool {
        !self.waited && !wall.is_zero() && self.busy * 2 > wall
    }
}

/// Inflates a received deflated chunk; `None` when it is corrupt or larger than `limit`.
pub fn unpack(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    decompress_to_vec_with_limit(data, limit).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(len: usize) -> Vec<u8> {
        b"fn main() { println!(\"hello, world\"); } // OpenDesk test line\r\n"
            .iter()
            .copied()
            .cycle()
            .take(len)
            .collect()
    }

    /// xorshift bytes: incompressible.
    fn noise(len: usize, mut s: u64) -> Vec<u8> {
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u8
            })
            .collect()
    }

    #[test]
    fn packed_formats_are_never_tried() {
        assert!(worth_trying("report.txt") && worth_trying("Makefile") && worth_trying("db.SQLITE"));
        assert!(!worth_trying("video.MP4") && !worth_trying("a.tar.gz") && !worth_trying("x.zip"));
        let now = Instant::now();
        assert!(Squeeze::new("x.zip", true, now).pack(&text(4096), true, now).is_none());
        assert!(Squeeze::new("x.txt", false, now).pack(&text(4096), true, now).is_none(), "old partner");
    }

    #[test]
    fn text_round_trips_smaller() {
        let now = Instant::now();
        let mut sq = Squeeze::new("main.rs", true, now);
        let data = text(128 * 1024);
        let packed = sq.pack(&data, true, now).expect("text compresses");
        assert!(packed.len() < data.len() / 4, "{}", packed.len());
        assert_eq!(unpack(&packed, data.len()).unwrap(), data);
        assert!(unpack(&packed, data.len() - 1).is_none(), "over the limit");
        assert!(unpack(&[0xFF, 0x00, 0x13], 1024).is_none(), "corrupt");
    }

    #[test]
    fn incompressible_data_pauses_then_probes_again() {
        let now = Instant::now();
        let mut sq = Squeeze::new("blob.bin", true, now);
        for i in 0..STRIKES {
            assert!(sq.pack(&noise(4096, i as u64 + 1), true, now).is_none());
        }
        // Paused: even text goes raw until the pause ends.
        for _ in 0..PAUSE_INCOMPRESSIBLE {
            assert!(sq.pack(&text(4096), true, now).is_none());
        }
        assert!(sq.pack(&text(4096), true, now).is_some(), "probes again");
    }

    #[test]
    fn a_link_faster_than_the_compressor_pauses_it() {
        let t0 = Instant::now();
        let mut sq = Squeeze::new("a.log", true, t0);
        // Never waited, compression took 80% of the window.
        sq.busy = Duration::from_millis(80);
        sq.window = WINDOW - 1;
        assert!(sq.pack(&text(4096), false, t0 + Duration::from_millis(100)).is_none());
        assert_eq!(sq.paused, PAUSE_CPU - 1);
        // A link that made the sender wait keeps compression on, however busy.
        let mut sq = Squeeze::new("a.log", true, t0);
        sq.busy = Duration::from_millis(80);
        sq.window = WINDOW - 1;
        assert!(sq.pack(&text(4096), true, t0 + Duration::from_millis(100)).is_some());
    }

    /// `cargo test --release deflate_throughput -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn deflate_throughput() {
        let data = text(128 * 1024);
        let mut src = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        src.truncate(8 << 20);
        for (name, input) in [("text", data.repeat(64)), ("exe", src)] {
            let t = Instant::now();
            let packed: Vec<Vec<u8>> = input.chunks(128 * 1024).map(|c| compress_to_vec(c, LEVEL)).collect();
            let c = t.elapsed();
            let t = Instant::now();
            for p in &packed {
                unpack(p, 128 * 1024).unwrap();
            }
            let d = t.elapsed();
            let out: usize = packed.iter().map(Vec::len).sum();
            let mbs = |dur: Duration| input.len() as f64 / dur.as_secs_f64() / 1e6;
            println!(
                "{name}: ratio {:.2}, deflate {:.0} MB/s, inflate {:.0} MB/s",
                out as f64 / input.len() as f64,
                mbs(c),
                mbs(d)
            );
        }
    }
}
