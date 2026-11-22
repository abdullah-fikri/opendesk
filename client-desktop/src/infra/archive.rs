//! Zip and tar archives for the file browser's "Extract" and "Compress to .zip" (`service::file_browser`), in
//! pure Rust on top of `miniz_oxide` (already used for `file-deflate`), so no new crate or C code. Reads .zip
//! (Store and Deflate), .tar, .tar.gz/.tgz; writes .zip (Deflate). ZIP64, encrypted zips, .7z and .rar are not
//! supported: their decoders would be a large new dependency for a rare case.
//!
//! SECURITY: an archive may come from anywhere, and on a host it is unpacked for a remote viewer.
//! - Zip-slip: every entry name must pass `safe_rel_path` (no absolute or drive paths, `..`, device names,
//!   streams) and is joined below the target folder; entries that fail are skipped and counted.
//! - Links: tar symlinks, hard links and devices are skipped; an existing link (or file) in the target where the
//!   archive wants a folder stops that entry, so nothing is written through a link out of the target.
//! - Nothing is overwritten: files are created with `create_new`, under a free name ("a (1).txt") when taken.
//! - Zip bombs: the item count and the unpacked bytes actually written (not what headers claim) are bounded by
//!   [`Limits`]; going over stops the extraction and removes everything it created.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

use crate::domain::transfer::safe_rel_path;

/// How much one extraction may unpack.
pub struct Limits {
    pub entries: usize,
    pub bytes: u64,
}

/// What the file browser allows: as many items as a folder transfer, and 16 GB.
pub const LIMITS: Limits = Limits { entries: 20_000, bytes: 16 << 30 };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zip,
    Tar,
    TarGz,
}

/// The archive kind a file name says, if it is one this module reads.
pub fn kind_of(name: &str) -> Option<Kind> {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".zip") {
        Some(Kind::Zip)
    } else if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
        Some(Kind::TarGz)
    } else if n.ends_with(".tar") {
        Some(Kind::Tar)
    } else {
        None
    }
}

/// "photos" for "photos.tar.gz": the folder "Extract to" creates.
pub fn stem_of(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    [".tar.gz", ".tgz", ".tar", ".zip"]
        .iter()
        .find(|ext| lower.ends_with(*ext) && name.len() > ext.len())
        .map_or(name, |ext| &name[..name.len() - ext.len()])
}

/// What an extraction did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub files: usize,
    pub folders: usize,
    /// Entries left out: unsafe names, links, devices, or a link in the way.
    pub skipped: usize,
}

impl Report {
    /// "Extracted 12 items" (+ ", 2 skipped").
    pub fn note(&self) -> String {
        let n = self.files + self.folders;
        let mut s = format!("Extracted {n} item{}", if n == 1 { "" } else { "s" });
        if self.skipped > 0 {
            s.push_str(&format!(", {} skipped (unsafe names or links)", self.skipped));
        }
        s
    }
}

/// Unpacks `archive` into the existing folder `into`. `progress(done, total)` is called as it goes (bytes; the
/// unit is the archive's own and only the ratio matters). On error everything created so far is removed.
pub fn extract(
    archive: &Path,
    into: &Path,
    limits: &Limits,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Report, String> {
    let name = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let kind = kind_of(&name).ok_or("This file is not an archive OpenDesk can extract (.zip, .tar, .tar.gz)")?;
    let file = File::open(archive).map_err(|e| format!("Cannot open {name}: {e}"))?;
    if !into.is_dir() {
        return Err(format!("{} is not a folder", into.display()));
    }
    let mut sink =
        Sink { root: into.to_path_buf(), limits, entries: 0, bytes: 0, report: Report::default(), made: vec![] };
    let result = match kind {
        Kind::Zip => extract_zip(file, &mut sink, progress),
        Kind::Tar | Kind::TarGz => {
            let total = file.metadata().map(|m| m.len()).unwrap_or(0).max(1);
            let counted = Counted { inner: BufReader::new(file), read: 0 };
            let mut tick = |read: u64| progress(read.min(total), total);
            if kind == Kind::Tar {
                extract_tar(counted, &mut sink, &mut tick)
            } else {
                extract_tar(Gunzip::new(counted)?, &mut sink, &mut tick)
            }
        }
    };
    match result {
        Ok(()) => Ok(sink.report),
        Err(e) => {
            sink.undo();
            Err(e)
        }
    }
}

/// One item to pack: `name` is its path inside the archive with `/` separators.
pub struct Item {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
}

/// Writes `items` into a new zip `out` (never replaces an existing file); returns how many it packed. On error
/// the partial file is removed.
pub fn compress(items: &[Item], out: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<usize, String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|e| format!("Cannot create {}: {e}", out.display()))?;
    let total: u64 = items.iter().filter(|i| !i.dir).filter_map(|i| fs::metadata(&i.path).ok()).map(|m| m.len()).sum();
    let result = write_zip(BufWriter::new(file), items, total.max(1), progress);
    if result.is_err() {
        let _ = fs::remove_file(out);
    }
    result
}

// ---- Extraction target ----

struct Sink<'a> {
    root: PathBuf,
    limits: &'a Limits,
    entries: usize,
    bytes: u64,
    report: Report,
    /// Files and folders this extraction created, to remove them again if it fails.
    made: Vec<PathBuf>,
}

impl Sink<'_> {
    fn count(&mut self) -> Result<(), String> {
        self.entries += 1;
        if self.entries > self.limits.entries {
            return Err(format!("The archive has more than {} items", self.limits.entries));
        }
        Ok(())
    }

    /// The folder below the root for `dirs`, created as needed; None when a part of it exists but is not a real
    /// folder (a file, or a link that could lead out of the target).
    fn folder(&mut self, dirs: &[&str]) -> Option<PathBuf> {
        let mut p = self.root.clone();
        for c in dirs {
            p.push(c);
            match fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return None,
                Err(_) => {
                    fs::create_dir(&p).ok()?;
                    self.made.push(p.clone());
                }
            }
        }
        Some(p)
    }

    fn dir(&mut self, name: &str) -> Result<(), String> {
        self.count()?;
        let made = safe_rel_path(name.trim_end_matches(['/', '\\'])).and_then(|c| self.folder(&c));
        if made.is_some() {
            self.report.folders += 1;
        } else {
            self.report.skipped += 1;
        }
        Ok(())
    }

    /// Writes one file from `data`; `check` = (CRC-32, size) the archive claims, verified when given. A skipped
    /// entry returns Ok without reading `data` (the caller skips what it holds).
    fn file(&mut self, name: &str, data: &mut dyn Read, check: Option<(u32, u64)>) -> Result<(), String> {
        self.count()?;
        let Some(comps) = safe_rel_path(name) else {
            self.report.skipped += 1;
            return Ok(());
        };
        let (leaf, dirs) = comps.split_last().expect("safe_rel_path never returns no components");
        let Some(dir) = self.folder(dirs) else {
            self.report.skipped += 1;
            return Ok(());
        };
        let (mut out, path) = create_free(&dir, leaf).map_err(|e| format!("Cannot create {name}: {e}"))?;
        self.made.push(path);
        let mut crc = Crc32::new();
        let mut written = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = data.read(&mut buf).map_err(|e| format!("Cannot read {name} from the archive: {e}"))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            self.bytes += n as u64;
            if self.bytes > self.limits.bytes {
                return Err(too_big(self.limits));
            }
            if check.is_some_and(|(_, size)| written > size) {
                return Err(format!("The archive is damaged ({name} is larger than it says)"));
            }
            crc.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| format!("Cannot write {name}: {e}"))?;
        }
        if let Some((want_crc, size)) = check
            && (written != size || crc.finish() != want_crc)
        {
            return Err(format!("The archive is damaged ({name} does not match its checksum)"));
        }
        self.report.files += 1;
        Ok(())
    }

    fn undo(&mut self) {
        for p in self.made.drain(..).rev() {
            let _ = if p.is_dir() { fs::remove_dir(&p) } else { fs::remove_file(&p) };
        }
    }
}

fn too_big(limits: &Limits) -> String {
    format!("The archive unpacks to more than {}", crate::domain::file_browser::size_text(limits.bytes))
}

/// Creates `dir/name`, or `stem (1).ext`, ... when taken; never opens an existing file.
fn create_free(dir: &Path, name: &str) -> io::Result<(File, PathBuf)> {
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    for n in 0..1000 {
        let candidate = if n == 0 { dir.join(name) } else { dir.join(format!("{stem} ({n}){ext}")) };
        match OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(f) => return Ok((f, candidate)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

// ---- Zip ----

const ZIP_LOCAL: u32 = 0x0403_4b50;
const ZIP_CENTRAL: u32 = 0x0201_4b50;
const ZIP_END: u32 = 0x0605_4b50;
/// General purpose flag bit 11: names are UTF-8.
const ZIP_UTF8: u16 = 0x0800;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

struct ZipEntry {
    name: String,
    method: u16,
    encrypted: bool,
    crc: u32,
    packed: u64,
    size: u64,
    local: u64,
}

fn extract_zip(file: File, sink: &mut Sink, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let damaged = || "The zip archive is damaged".to_string();
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut f = BufReader::new(file);
    // End of central directory: the last record, within the last 64 KiB (its comment) + 22 bytes.
    let tail_len = len.min(22 + 65_535);
    let mut tail = vec![0u8; tail_len as usize];
    f.seek(SeekFrom::Start(len - tail_len)).and_then(|_| f.read_exact(&mut tail)).map_err(|_| damaged())?;
    let at = (0..=tail.len().saturating_sub(22))
        .rev()
        .find(|&i| tail.len() >= i + 22 && u32_at(&tail, i) == ZIP_END)
        .ok_or("This is not a zip archive")?;
    let end = &tail[at..];
    let (count, cd_size, cd_at) = (u16_at(end, 10), u32_at(end, 12), u32_at(end, 16));
    if count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_at == 0xFFFF_FFFF {
        return Err("ZIP64 archives (over 4 GB or 65,535 items) are not supported".into());
    }
    if count as usize > sink.limits.entries {
        return Err(format!("The archive has more than {} items", sink.limits.entries));
    }
    if u64::from(cd_at) + u64::from(cd_size) > len {
        return Err(damaged());
    }
    let mut cd = vec![0u8; cd_size as usize];
    f.seek(SeekFrom::Start(cd_at.into())).and_then(|_| f.read_exact(&mut cd)).map_err(|_| damaged())?;
    let mut entries = Vec::with_capacity(count as usize);
    let mut pos = 0usize;
    for _ in 0..count {
        if pos + 46 > cd.len() || u32_at(&cd, pos) != ZIP_CENTRAL {
            return Err(damaged());
        }
        let flags = u16_at(&cd, pos + 8);
        let (nlen, xlen, clen) =
            (u16_at(&cd, pos + 28) as usize, u16_at(&cd, pos + 30) as usize, u16_at(&cd, pos + 32) as usize);
        let raw = cd.get(pos + 46..pos + 46 + nlen).ok_or_else(damaged)?;
        entries.push(ZipEntry {
            name: zip_name(raw, flags & ZIP_UTF8 != 0),
            method: u16_at(&cd, pos + 10),
            encrypted: flags & 1 != 0,
            crc: u32_at(&cd, pos + 16),
            packed: u32_at(&cd, pos + 20).into(),
            size: u32_at(&cd, pos + 24).into(),
            local: u32_at(&cd, pos + 42).into(),
        });
        pos += 46 + nlen + xlen + clen;
    }
    // What the headers claim already over the limit: stop before writing anything.
    let total = entries.iter().map(|e| e.size).fold(0u64, u64::saturating_add);
    if total > sink.limits.bytes {
        return Err(too_big(sink.limits));
    }
    for e in &entries {
        if e.name.ends_with(['/', '\\']) {
            sink.dir(&e.name)?;
            continue;
        }
        if e.encrypted {
            return Err("Password-protected zip archives are not supported".into());
        }
        if e.method != 0 && e.method != 8 {
            return Err(format!("{} uses a compression method OpenDesk cannot unpack (only Deflate)", e.name));
        }
        let mut head = [0u8; 30];
        f.seek(SeekFrom::Start(e.local)).and_then(|_| f.read_exact(&mut head)).map_err(|_| damaged())?;
        if u32_at(&head, 0) != ZIP_LOCAL {
            return Err(damaged());
        }
        let data_at = e.local + 30 + u64::from(u16_at(&head, 26)) + u64::from(u16_at(&head, 28));
        if data_at + e.packed > len {
            return Err(damaged());
        }
        f.seek(SeekFrom::Start(data_at)).map_err(|_| damaged())?;
        let mut raw = (&mut f).take(e.packed);
        let check = Some((e.crc, e.size));
        if e.method == 0 {
            sink.file(&e.name, &mut raw, check)?;
        } else {
            sink.file(&e.name, &mut Inflater::new(raw), check)?;
        }
        progress(sink.bytes, total.max(1));
    }
    Ok(())
}

/// A zip entry name: UTF-8 when flagged (or valid anyway), else the old DOS code page read as Latin-1.
fn zip_name(raw: &[u8], utf8: bool) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) if utf8 => String::from_utf8_lossy(raw).to_string(),
        Err(_) => raw.iter().map(|b| *b as char).collect(),
    }
}

fn write_zip<W: Write + Seek>(
    mut w: W,
    items: &[Item],
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<usize, String> {
    let io = |e: io::Error| format!("Cannot write the archive: {e}");
    if items.len() >= 0xFFFF {
        return Err("Too many items for one .zip (at most 65,534)".into());
    }
    let mut central = Vec::new();
    let mut offset = 0u64;
    let mut done = 0u64;
    for item in items {
        let name = if item.dir { format!("{}/", item.name.trim_end_matches('/')) } else { item.name.clone() };
        let (time, date) = dos_time(fs::metadata(&item.path).and_then(|m| m.modified()).ok());
        let local = u32::try_from(offset).map_err(|_| "The .zip would be larger than 4 GB".to_string())?;
        let method: u16 = if item.dir { 0 } else { 8 };
        let header = |crc: u32, packed: u32, size: u32| {
            let mut h = Vec::with_capacity(30 + name.len());
            h.extend_from_slice(&ZIP_LOCAL.to_le_bytes());
            for v in [20u16, ZIP_UTF8, method, time, date] {
                h.extend_from_slice(&v.to_le_bytes());
            }
            for v in [crc, packed, size] {
                h.extend_from_slice(&v.to_le_bytes());
            }
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(name.as_bytes());
            h
        };
        w.write_all(&header(0, 0, 0)).map_err(io)?;
        let (crc, packed, size) = if item.dir {
            (0, 0, 0)
        } else {
            let src = File::open(&item.path).map_err(|e| format!("Cannot read {}: {e}", item.name))?;
            let mut on_bytes = |n: u64| {
                done += n;
                progress(done, total);
            };
            deflate_into(&mut w, src, &mut on_bytes).map_err(|e| format!("Cannot pack {}: {e}", item.name))?
        };
        let (packed32, size32) = match (u32::try_from(packed), u32::try_from(size)) {
            (Ok(p), Ok(s)) if p < u32::MAX && s < u32::MAX => (p, s),
            _ => return Err(format!("{} is too large for a .zip (4 GB)", item.name)),
        };
        if !item.dir {
            // Sizes are known only now: patch them into the local header.
            w.seek(SeekFrom::Start(offset + 14)).map_err(io)?;
            for v in [crc, packed32, size32] {
                w.write_all(&v.to_le_bytes()).map_err(io)?;
            }
            w.seek(SeekFrom::End(0)).map_err(io)?;
        }
        central.extend_from_slice(&ZIP_CENTRAL.to_le_bytes());
        for v in [0x031Eu16, 20, ZIP_UTF8, method, time, date] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, packed32, size32] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [name.len() as u16, 0, 0, 0, 0] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        // External attributes: MS-DOS directory bit for folders.
        central.extend_from_slice(&(if item.dir { 0x10u32 } else { 0 }).to_le_bytes());
        central.extend_from_slice(&local.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
        offset += 30 + name.len() as u64 + packed;
    }
    let cd_at = u32::try_from(offset).map_err(|_| "The .zip would be larger than 4 GB".to_string())?;
    w.write_all(&central).map_err(io)?;
    let mut end = Vec::with_capacity(22);
    end.extend_from_slice(&ZIP_END.to_le_bytes());
    for v in [0u16, 0, items.len() as u16, items.len() as u16] {
        end.extend_from_slice(&v.to_le_bytes());
    }
    end.extend_from_slice(&(central.len() as u32).to_le_bytes());
    end.extend_from_slice(&cd_at.to_le_bytes());
    end.extend_from_slice(&0u16.to_le_bytes());
    w.write_all(&end).and_then(|_| w.flush()).map_err(io)?;
    Ok(items.len())
}

/// Streams `src` through raw Deflate into `w`; returns (CRC-32, packed size, plain size).
fn deflate_into(w: &mut dyn Write, mut src: File, on_bytes: &mut dyn FnMut(u64)) -> io::Result<(u32, u64, u64)> {
    use miniz_oxide::deflate::core::{CompressorOxide, create_comp_flags_from_zip_params};
    use miniz_oxide::deflate::stream::deflate;
    let mut comp = CompressorOxide::new(create_comp_flags_from_zip_params(6, -15, 0));
    let (mut inbuf, mut outbuf) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    let (mut crc, mut packed, mut size) = (Crc32::new(), 0u64, 0u64);
    loop {
        let n = src.read(&mut inbuf)?;
        crc.update(&inbuf[..n]);
        size += n as u64;
        on_bytes(n as u64);
        let flush = if n == 0 { MZFlush::Finish } else { MZFlush::None };
        let mut input = &inbuf[..n];
        loop {
            let r = deflate(&mut comp, input, &mut outbuf, flush);
            input = &input[r.bytes_consumed..];
            w.write_all(&outbuf[..r.bytes_written])?;
            packed += r.bytes_written as u64;
            match r.status {
                Ok(MZStatus::StreamEnd) => return Ok((crc.finish(), packed, size)),
                Ok(_) | Err(MZError::Buf) => {}
                Err(e) => return Err(io::Error::other(format!("deflate failed: {e:?}"))),
            }
            if flush == MZFlush::None && input.is_empty() && r.bytes_written < outbuf.len() {
                break;
            }
            if flush == MZFlush::Finish && r.bytes_written == 0 && r.status.is_err() {
                return Err(io::Error::other("deflate made no progress"));
            }
        }
    }
}

/// MS-DOS (time, date) of `t` in UTC; 1980-01-01 when unknown or earlier.
fn dos_time(t: Option<SystemTime>) -> (u16, u16) {
    let secs = t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    if !(1980..=2107).contains(&year) {
        return (0, (1 << 5) | 1);
    }
    let time = ((rem / 3600) << 11) | (((rem % 3600) / 60) << 5) | ((rem % 60) / 2);
    let date = ((year - 1980) << 9) | (month << 5) | day;
    (time as u16, date as u16)
}

// ---- Tar ----

fn extract_tar<R: Read + Position>(mut r: R, sink: &mut Sink, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
    let damaged = || "The tar archive is damaged".to_string();
    let mut long_name: Option<String> = None;
    let mut pax_path: Option<String> = None;
    let mut zeros = 0;
    loop {
        let mut h = [0u8; 512];
        if !read_block(&mut r, &mut h).map_err(|_| damaged())? {
            return Ok(());
        }
        if h.iter().all(|b| *b == 0) {
            zeros += 1;
            if zeros == 2 {
                return Ok(());
            }
            continue;
        }
        zeros = 0;
        let sum: u64 =
            h.iter().enumerate().map(|(i, b)| if (148..156).contains(&i) { 32 } else { u64::from(*b) }).sum();
        if octal(&h[148..156]) != Some(sum) {
            return Err("This is not a tar archive".into());
        }
        let size = tar_size(&h[124..136]).ok_or_else(damaged)?;
        if size > sink.limits.bytes {
            return Err(too_big(sink.limits));
        }
        let pad = size.div_ceil(512) * 512 - size;
        let kind = h[156];
        match kind {
            // GNU long name / PAX extended header: the next entry's name.
            b'L' | b'x' => {
                if size > 1 << 20 {
                    return Err(damaged());
                }
                let mut data = vec![0u8; size as usize];
                r.read_exact(&mut data).map_err(|_| damaged())?;
                if kind == b'L' {
                    long_name = Some(cstr(&data));
                } else {
                    pax_path = pax_value(&data, "path");
                }
                skip(&mut r, pad).map_err(|_| damaged())?;
                continue;
            }
            _ => {}
        }
        let name = pax_path.take().or(long_name.take()).unwrap_or_else(|| {
            let (prefix, name) = (cstr(&h[345..500]), cstr(&h[0..100]));
            if &h[257..262] == b"ustar" && !prefix.is_empty() { format!("{prefix}/{name}") } else { name }
        });
        let mut data = (&mut r).take(size);
        match kind {
            b'0' | 0 | b'7' => sink.file(&name, &mut data, None)?,
            b'5' => sink.dir(&name)?,
            // Global PAX headers carry nothing we use.
            b'g' => {}
            // Symlinks, hard links, devices, FIFOs: never created.
            _ => {
                sink.count()?;
                sink.report.skipped += 1;
            }
        }
        let rest = data.limit();
        skip(&mut r, rest + pad).map_err(|_| damaged())?;
        progress(r.position());
    }
}

/// Reads one 512-byte block; false at a clean end of the input.
fn read_block(r: &mut dyn Read, block: &mut [u8; 512]) -> io::Result<bool> {
    let mut got = 0;
    while got < block.len() {
        match r.read(&mut block[got..])? {
            0 if got == 0 => return Ok(false),
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => got += n,
        }
    }
    Ok(true)
}

fn skip(r: &mut dyn Read, n: u64) -> io::Result<()> {
    if io::copy(&mut r.take(n), &mut io::sink())? == n { Ok(()) } else { Err(io::ErrorKind::UnexpectedEof.into()) }
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).to_string()
}

fn octal(b: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(b).ok()?.trim_matches(|c: char| c == '\0' || c == ' ');
    if s.is_empty() { Some(0) } else { u64::from_str_radix(s, 8).ok() }
}

/// A tar size field: octal, or GNU base-256 when the top bit is set.
fn tar_size(b: &[u8]) -> Option<u64> {
    if b[0] & 0x80 == 0 {
        return octal(b);
    }
    b[1..].iter().try_fold(u64::from(b[0] & 0x7f), |acc, x| acc.checked_mul(256).map(|v| v | u64::from(*x)))
}

/// The value of `key` in PAX records ("<len> key=value\n" ...).
fn pax_value(data: &[u8], key: &str) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    let mut rest = text.as_ref();
    while let Some(space) = rest.find(' ') {
        let len: usize = rest[..space].parse().ok()?;
        let record = rest.get(space + 1..len.checked_sub(1)?)?;
        if let Some(value) = record.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
            return Some(value.to_string());
        }
        rest = rest.get(len..)?;
    }
    None
}

/// How far into the archive file a reader is (compressed bytes for .tar.gz), for progress.
trait Position {
    fn position(&self) -> u64;
}

struct Counted<R> {
    inner: R,
    read: u64,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}

impl<R> Position for Counted<R> {
    fn position(&self) -> u64 {
        self.read
    }
}

// ---- Deflate / gzip readers ----

/// Raw Deflate decoder over a reader, streaming (a 32 KiB window, whatever the size).
struct Inflater<R> {
    inner: R,
    state: Box<InflateState>,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    eof: bool,
    done: bool,
}

impl<R: Read> Inflater<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            state: InflateState::new_boxed(DataFormat::Raw),
            buf: vec![0u8; 64 * 1024],
            pos: 0,
            len: 0,
            eof: false,
            done: false,
        }
    }
}

impl<R: Read> Read for Inflater<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.done {
                return Ok(0);
            }
            if self.pos == self.len && !self.eof {
                self.len = self.inner.read(&mut self.buf)?;
                self.pos = 0;
                self.eof = self.len == 0;
            }
            let r = inflate(&mut self.state, &self.buf[self.pos..self.len], out, MZFlush::None);
            self.pos += r.bytes_consumed;
            match r.status {
                Ok(MZStatus::StreamEnd) => self.done = true,
                Ok(_) | Err(MZError::Buf) => {}
                Err(_) => return Err(bad("corrupt compressed data")),
            }
            if r.bytes_written > 0 {
                return Ok(r.bytes_written);
            }
            if !self.done && self.eof {
                return Err(bad("compressed data ends too early"));
            }
        }
    }
}

/// A gzip stream (.tar.gz): the header, then raw Deflate. The trailer is not checked; the tar headers' own
/// checksums catch damage that matters.
struct Gunzip<R> {
    inflater: Inflater<R>,
}

impl<R: Read + Position> Gunzip<R> {
    fn new(mut inner: R) -> Result<Self, String> {
        let bad = || "This is not a gzip file".to_string();
        let mut head = [0u8; 10];
        inner.read_exact(&mut head).map_err(|_| bad())?;
        if head[0..3] != [0x1f, 0x8b, 8] {
            return Err(bad());
        }
        let flags = head[3];
        if flags & 4 != 0 {
            let mut xlen = [0u8; 2];
            inner.read_exact(&mut xlen).map_err(|_| bad())?;
            skip(&mut inner, u16::from_le_bytes(xlen).into()).map_err(|_| bad())?;
        }
        for bit in [8u8, 16] {
            if flags & bit != 0 {
                // Original name / comment: up to a NUL.
                let mut b = [0u8; 1];
                for _ in 0..65_536 {
                    inner.read_exact(&mut b).map_err(|_| bad())?;
                    if b[0] == 0 {
                        break;
                    }
                }
            }
        }
        if flags & 2 != 0 {
            skip(&mut inner, 2).map_err(|_| bad())?;
        }
        Ok(Self { inflater: Inflater::new(inner) })
    }
}

impl<R: Read> Read for Gunzip<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inflater.read(buf)
    }
}

impl<R: Position> Position for Gunzip<R> {
    fn position(&self) -> u64 {
        self.inflater.inner.position()
    }
}

// ---- CRC-32 (zip) ----

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    fn update(&mut self, data: &[u8]) {
        for b in data {
            self.0 = CRC_TABLE[((self.0 ^ u32::from(*b)) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }

    fn finish(&self) -> u32 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opendesk-archive-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn crc(data: &[u8]) -> u32 {
        let mut c = Crc32::new();
        c.update(data);
        c.finish()
    }

    /// A zip of stored entries with exactly these names (no checks), like a hostile tool would write.
    fn raw_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data) in entries {
            let local = out.len() as u32;
            out.extend_from_slice(&ZIP_LOCAL.to_le_bytes());
            for v in [20u16, 0, 0, 0, 0] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in [crc(data), data.len() as u32, data.len() as u32] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(&ZIP_CENTRAL.to_le_bytes());
            for v in [20u16, 20, 0, 0, 0, 0] {
                central.extend_from_slice(&v.to_le_bytes());
            }
            for v in [crc(data), data.len() as u32, data.len() as u32] {
                central.extend_from_slice(&v.to_le_bytes());
            }
            for v in [name.len() as u16, 0, 0, 0, 0] {
                central.extend_from_slice(&v.to_le_bytes());
            }
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&local.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_at = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&ZIP_END.to_le_bytes());
        for v in [0u16, 0, entries.len() as u16, entries.len() as u16] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_at.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// A tar entry: header (+ data padded to 512).
    fn tar_entry(name: &str, kind: u8, data: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 255];
        out.extend_from_slice(b"x.tar\0");
        out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(data, 6));
        out.extend_from_slice(&crc(data).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out
    }

    fn walk_names(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                if e.path().is_dir() {
                    stack.push(e.path());
                    out.push(format!("{rel}/"));
                } else {
                    out.push(rel);
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn names_say_the_kind_and_the_folder() {
        assert_eq!(kind_of("A.ZIP"), Some(Kind::Zip));
        assert_eq!(kind_of("b.tar.gz"), Some(Kind::TarGz));
        assert_eq!(kind_of("b.tgz"), Some(Kind::TarGz));
        assert_eq!(kind_of("c.tar"), Some(Kind::Tar));
        assert_eq!(kind_of("d.7z"), None);
        assert_eq!(stem_of("photos.tar.gz"), "photos");
        assert_eq!(stem_of("Report.Zip"), "Report");
        assert_eq!(stem_of(".zip"), ".zip");
        assert_eq!(
            Report { files: 1, folders: 0, skipped: 2 }.note(),
            "Extracted 1 item, 2 skipped (unsafe names or links)"
        );
    }

    #[test]
    fn crc_matches_the_standard() {
        assert_eq!(crc(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn compress_then_extract_round_trips_and_never_overwrites() {
        let src = temp("rt-src");
        fs::create_dir_all(src.join("Project/src/empty")).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(src.join("Project/src/main.rs"), &big).unwrap();
        fs::write(src.join("Project/readme.txt"), b"hello").unwrap();
        fs::write(src.join("notes.txt"), b"").unwrap();
        let items = vec![
            Item { path: src.join("Project"), name: "Project".into(), dir: true },
            Item { path: src.join("Project/readme.txt"), name: "Project/readme.txt".into(), dir: false },
            Item { path: src.join("Project/src"), name: "Project/src".into(), dir: true },
            Item { path: src.join("Project/src/empty"), name: "Project/src/empty".into(), dir: true },
            Item { path: src.join("Project/src/main.rs"), name: "Project/src/main.rs".into(), dir: false },
            Item { path: src.join("notes.txt"), name: "notes.txt".into(), dir: false },
        ];
        let out = temp("rt-out");
        let zip = out.join("Archive.zip");
        let mut last = (0, 0);
        assert_eq!(compress(&items, &zip, &mut |d, t| last = (d, t)).unwrap(), 6);
        assert_eq!(last.0, last.1, "progress reaches the end");
        assert!(compress(&items, &zip, &mut |_, _| {}).is_err(), "an existing .zip is never replaced");

        let into = temp("rt-into");
        let report = extract(&zip, &into, &LIMITS, &mut |_, _| {}).unwrap();
        assert_eq!(report, Report { files: 3, folders: 3, skipped: 0 });
        assert_eq!(fs::read(into.join("Project/src/main.rs")).unwrap(), big);
        assert_eq!(fs::read(into.join("Project/readme.txt")).unwrap(), b"hello");
        assert!(into.join("Project/src/empty").is_dir());
        // Again into the same place: the existing files stay, the new ones get free names.
        fs::write(into.join("notes.txt"), b"mine").unwrap();
        extract(&zip, &into, &LIMITS, &mut |_, _| {}).unwrap();
        assert_eq!(fs::read(into.join("notes.txt")).unwrap(), b"mine");
        assert!(into.join("notes (1).txt").is_file() && into.join("Project/readme (1).txt").is_file());
        for d in [src, out, into] {
            let _ = fs::remove_dir_all(d);
        }
    }

    #[test]
    fn zip_slip_entries_are_skipped() {
        let dir = temp("slip");
        let into = dir.join("into");
        fs::create_dir(&into).unwrap();
        let zip = dir.join("evil.zip");
        fs::write(
            &zip,
            raw_zip(&[
                ("../evil.txt", b"x"),
                ("/abs.txt", b"x"),
                ("C:/drive.txt", b"x"),
                ("a/../../up.txt", b"x"),
                ("CON", b"x"),
                ("ok/good.txt", b"fine"),
            ]),
        )
        .unwrap();
        let report = extract(&zip, &into, &LIMITS, &mut |_, _| {}).unwrap();
        assert_eq!((report.files, report.skipped), (1, 5));
        assert_eq!(walk_names(&dir), ["evil.zip", "into/", "into/ok/", "into/ok/good.txt"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bombs_stop_at_the_limits_and_leave_nothing() {
        let dir = temp("bomb");
        let zeros = vec![0u8; 3 << 20];
        let src = dir.join("zeros.bin");
        fs::write(&src, &zeros).unwrap();
        let zip = dir.join("bomb.zip");
        compress(&[Item { path: src.clone(), name: "a/zeros.bin".into(), dir: false }], &zip, &mut |_, _| {}).unwrap();
        assert!(fs::metadata(&zip).unwrap().len() < 64 * 1024, "zeros compress well");
        let into = dir.join("into");
        fs::create_dir(&into).unwrap();
        let small = Limits { entries: 100, bytes: 1 << 20 };
        assert!(extract(&zip, &into, &small, &mut |_, _| {}).unwrap_err().contains("more than"));
        // Headers that lie about the size: the bytes actually written still count.
        let mut liar = raw_zip(&[("big.bin", &zeros)]);
        let cd = u32_at(&liar, liar.len() - 22 + 16) as usize;
        liar[cd + 24..cd + 28].copy_from_slice(&10u32.to_le_bytes());
        let liar_path = dir.join("liar.zip");
        fs::write(&liar_path, &liar).unwrap();
        let tiny = Limits { entries: 100, bytes: 1 << 20 };
        assert!(extract(&liar_path, &into, &tiny, &mut |_, _| {}).unwrap_err().contains("larger than it says"));
        let many = raw_zip(&[("1", b""), ("2", b""), ("3", b"")]);
        let many_path = dir.join("many.zip");
        fs::write(&many_path, many).unwrap();
        let two = Limits { entries: 2, bytes: 1 << 20 };
        assert!(extract(&many_path, &into, &two, &mut |_, _| {}).unwrap_err().contains("more than 2 items"));
        assert!(walk_names(&into).is_empty(), "a failed extraction removes what it made: {:?}", walk_names(&into));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn damaged_zips_fail_cleanly() {
        let dir = temp("damaged");
        let mut zip = raw_zip(&[("a.txt", b"hello world")]);
        let at = zip.windows(11).position(|w| w == b"hello world").unwrap();
        zip[at] = b'j';
        fs::write(dir.join("bad.zip"), &zip).unwrap();
        fs::write(dir.join("junk.zip"), b"not a zip at all").unwrap();
        let into = dir.join("into");
        fs::create_dir(&into).unwrap();
        assert!(extract(&dir.join("bad.zip"), &into, &LIMITS, &mut |_, _| {}).unwrap_err().contains("checksum"));
        assert!(extract(&dir.join("junk.zip"), &into, &LIMITS, &mut |_, _| {}).is_err());
        assert!(walk_names(&into).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn tar_and_tgz_unpack_files_and_skip_links_and_escapes() {
        let dir = temp("tar");
        let mut tar = Vec::new();
        tar.extend(tar_entry("docs/", b'5', b""));
        tar.extend(tar_entry("docs/a.txt", b'0', b"alpha"));
        tar.extend(tar_entry("link", b'2', b""));
        tar.extend(tar_entry("../escape.txt", b'0', b"bad"));
        let long = format!("{}/deep.txt", "d".repeat(120));
        tar.extend(tar_entry("././@LongLink", b'L', long.as_bytes()));
        tar.extend(tar_entry("ignored", b'0', b"deep"));
        tar.extend([0u8; 1024]);
        fs::write(dir.join("a.tar"), &tar).unwrap();
        fs::write(dir.join("a.tgz"), gzip(&tar)).unwrap();
        for (archive, into) in [("a.tar", "t"), ("a.tgz", "g")] {
            let into = dir.join(into);
            fs::create_dir(&into).unwrap();
            let mut seen = 0;
            let report = extract(&dir.join(archive), &into, &LIMITS, &mut |d, _| seen = d).unwrap();
            assert_eq!(report, Report { files: 2, folders: 1, skipped: 2 }, "{archive}");
            assert_eq!(fs::read(into.join("docs/a.txt")).unwrap(), b"alpha");
            assert_eq!(fs::read(into.join("d".repeat(120)).join("deep.txt")).unwrap(), b"deep");
            assert!(seen > 0);
        }
        assert!(!dir.join("escape.txt").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_link_in_the_target_is_not_followed() {
        let dir = temp("link");
        let outside = dir.join("outside");
        let into = dir.join("into");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&into).unwrap();
        std::os::unix::fs::symlink(&outside, into.join("sub")).unwrap();
        fs::write(dir.join("a.zip"), raw_zip(&[("sub/x.txt", b"x")])).unwrap();
        let report = extract(&dir.join("a.zip"), &into, &LIMITS, &mut |_, _| {}).unwrap();
        assert_eq!(report.skipped, 1);
        assert!(!outside.join("x.txt").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
