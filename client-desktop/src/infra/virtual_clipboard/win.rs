//! AnyDesk-style clipboard file transfer: the partner's copied files are put on the local clipboard as
//! virtual files (FileGroupDescriptorW + FileContents). Nothing is transferred until an application pastes;
//! then each FileContents request pulls that file and streams it straight into the pasting application,
//! so Explorer shows its own copy progress. Hand-written COM objects, same raw-FFI style as the rest of the app.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use core_engine::codec::from_base64;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use crate::domain::transfer::{MAX_FILE_BYTES, MAX_TREE_BYTES, MAX_TREE_ENTRIES, is_safe_id, safe_rel_path};
use crate::infra::clipboard_helper::{ToAgent, ToHelper};
use crate::infra::logger;
use crate::protocol::{ClientMessage, FileAction, FilePayload, OfferEntry, Outgoing};

type HRESULT = i32;

const S_OK: HRESULT = 0;
const E_NOTIMPL: HRESULT = 0x8000_4001u32 as i32;
const E_NOINTERFACE: HRESULT = 0x8000_4002u32 as i32;
const E_FAIL: HRESULT = 0x8000_4005u32 as i32;
const E_OUTOFMEMORY: HRESULT = 0x8007_000Eu32 as i32;
const STG_E_INVALIDFUNCTION: HRESULT = 0x8003_0001u32 as i32;
const DV_E_FORMATETC: HRESULT = 0x8004_0064u32 as i32;
const DV_E_LINDEX: HRESULT = 0x8004_0068u32 as i32;
const DV_E_TYMED: HRESULT = 0x8004_0069u32 as i32;
const OLE_E_ADVISENOTSUPPORTED: HRESULT = 0x8004_0003u32 as i32;

const TYMED_HGLOBAL: u32 = 1;
const TYMED_ISTREAM: u32 = 4;
const DVASPECT_CONTENT: u32 = 1;
const DATADIR_GET: u32 = 1;
const GMEM_MOVEABLE: u32 = 0x0002;
const GMEM_ZEROINIT: u32 = 0x0040;
const FD_ATTRIBUTES: u32 = 0x0004;
const FD_CREATETIME: u32 = 0x0008;
const FD_ACCESSTIME: u32 = 0x0010;
const FD_WRITETIME: u32 = 0x0020;
const FD_FILESIZE: u32 = 0x0040;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FD_PROGRESSUI: u32 = 0x4000;
const DROPEFFECT_COPY: u32 = 1;
const STREAM_SEEK_SET: u32 = 0;
const STREAM_SEEK_CUR: u32 = 1;
const STGTY_STREAM: u32 = 2;
const WM_APP: u32 = 0x8000;
const PM_NOREMOVE: u32 = 0;

/// How long a paste waits for the next chunk before Explorer is told the copy failed.
const PULL_IDLE: Duration = Duration::from_secs(60);

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const fn ole_guid(data1: u32) -> GUID {
    GUID { data1, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] }
}
const IID_IUNKNOWN: GUID = ole_guid(0x0000_0000);
const IID_IDATAOBJECT: GUID = ole_guid(0x0000_010E);
const IID_ISTREAM: GUID = ole_guid(0x0000_000C);
const IID_ISEQUENTIALSTREAM: GUID =
    GUID { data1: 0x0c73_3a30, data2: 0x2a1c, data3: 0x11ce, data4: [0xad, 0xe5, 0x00, 0xaa, 0x00, 0x44, 0x77, 0x3d] };

#[repr(C)]
#[derive(Clone, Copy)]
struct FORMATETC {
    cf_format: u16,
    ptd: *mut c_void,
    dw_aspect: u32,
    lindex: i32,
    tymed: u32,
}

#[repr(C)]
struct STGMEDIUM {
    tymed: u32,
    data: *mut c_void, // HGLOBAL or IStream*
    p_unk_for_release: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FILETIME {
    low: u32,
    high: u32,
}

#[repr(C)]
struct FILEDESCRIPTORW {
    dw_flags: u32,
    clsid: GUID,
    sizel: [i32; 2],
    pointl: [i32; 2],
    dw_file_attributes: u32,
    ft_creation_time: FILETIME,
    ft_last_access_time: FILETIME,
    ft_last_write_time: FILETIME,
    n_file_size_high: u32,
    n_file_size_low: u32,
    c_file_name: [u16; 260],
}

#[repr(C)]
struct STATSTG {
    pwcs_name: *mut u16,
    type_: u32,
    cb_size: u64,
    mtime: FILETIME,
    ctime: FILETIME,
    atime: FILETIME,
    grf_mode: u32,
    grf_locks_supported: u32,
    clsid: GUID,
    grf_state_bits: u32,
    reserved: u32,
}

#[repr(C)]
struct MSG {
    hwnd: *mut c_void,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt: [i32; 2],
    l_private: u32,
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn OleInitialize(pv_reserved: *mut c_void) -> HRESULT;
    fn OleSetClipboard(p_data_obj: *mut c_void) -> HRESULT;
    fn DoDragDrop(p_data_obj: *mut c_void, p_drop_source: *mut c_void, ok_effects: u32, effect: *mut u32) -> HRESULT;
    fn OleIsCurrentClipboard(p_data_obj: *mut c_void) -> HRESULT;
    fn ReleaseStgMedium(p_medium: *mut STGMEDIUM);
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHCreateStdEnumFmtEtc(c_formats: u32, afmt: *const FORMATETC, ppenum_formatetc: *mut *mut c_void) -> HRESULT;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(u_flags: u32, dw_bytes: usize) -> *mut c_void;
    fn GlobalLock(h_mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(h_mem: *mut c_void) -> i32;
    fn GlobalSize(h_mem: *mut c_void) -> usize;
    fn GetCurrentThreadId() -> u32;
}

// tray.rs declares the message-loop functions with its own MSG type; the ABI is identical.
#[allow(clashing_extern_declarations)]
#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClipboardFormatW(lpsz_format: *const u16) -> u32;
    fn GetMessageW(lp_msg: *mut MSG, h_wnd: *mut c_void, min: u32, max: u32) -> i32;
    fn PeekMessageW(lp_msg: *mut MSG, h_wnd: *mut c_void, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(lp_msg: *const MSG) -> i32;
    fn DispatchMessageW(lp_msg: *const MSG) -> isize;
    fn PostThreadMessageW(id_thread: u32, msg: u32, w_param: usize, l_param: isize) -> i32;
}

struct Formats {
    descriptor: u16,
    contents: u16,
    drop_effect: u16,
    /// Shell notifications a paste target sends back through `SetData`.
    performed: u16,
    logical_performed: u16,
    paste_succeeded: u16,
    target_clsid: u16,
}

fn formats() -> &'static Formats {
    static F: OnceLock<Formats> = OnceLock::new();
    F.get_or_init(|| {
        let reg = |name: &str| {
            let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe { RegisterClipboardFormatW(w.as_ptr()) as u16 }
        };
        Formats {
            descriptor: reg("FileGroupDescriptorW"),
            contents: reg("FileContents"),
            drop_effect: reg("Preferred DropEffect"),
            performed: reg("Performed DropEffect"),
            logical_performed: reg("Logical Performed DropEffect"),
            paste_succeeded: reg("Paste Succeeded"),
            target_clsid: reg("TargetCLSID"),
        }
    })
}

/// One entry of the partner's offer. `name` is a validated relative path with backslash separators.
struct OfferFile {
    name: String,
    size: u64,
    dir: bool,
    /// Last write time from the partner (Unix ms), shown as "Date Modified" after the paste.
    mtime: Option<u64>,
    /// Creation and last access time (Unix ms) and the carried attributes (read-only, hidden, archive).
    ctime: Option<u64>,
    atime: Option<u64>,
    attrs: u32,
    /// Index in the partner's list (what a pull asks for); differs from ours when invalid entries were dropped.
    remote: usize,
}

/// Where a paste stream's pull request goes.
enum Route {
    /// This process talks to the relay: send the pull message here.
    Local(Sender<Outgoing>),
    /// This is the clipboard helper: the SYSTEM agent pulls and passes the bytes on.
    Agent,
}

/// One file list offered by the partner.
struct Offer {
    from: String,
    offer_id: String,
    files: Vec<OfferFile>,
    route: Route,
}

// ---------------------------------------------------------------------------------------------
// Pull sinks: transfer IDs we asked the partner to send, routed to the paste stream reading them.

enum SinkMsg {
    Data(Vec<u8>),
    End(bool),
}

struct Sink {
    from: String,
    name: String,
    size: u64,
    received: u64,
    hash: Sha256,
    out: SinkOut,
}

/// Where a pull's verified bytes go.
enum SinkOut {
    /// A paste stream in this process.
    Local(Sender<SinkMsg>),
    /// A paste stream in the clipboard helper, under this transfer ID.
    Helper(String),
}

impl SinkOut {
    /// Delivers one message; a closed stream means the paste was cancelled and later bytes are dropped.
    fn send(&self, msg: SinkMsg) {
        use crate::infra::clipboard_helper;
        match (self, msg) {
            (SinkOut::Local(tx), msg) => {
                let _ = tx.send(msg);
            }
            (SinkOut::Helper(tid), SinkMsg::Data(bytes)) => {
                clipboard_helper::send_data(tid, &bytes);
            }
            (SinkOut::Helper(tid), SinkMsg::End(ok)) => {
                clipboard_helper::send(&ToHelper::End { tid: tid.clone(), ok });
            }
        }
    }
}

static SINKS: Mutex<Option<HashMap<String, Sink>>> = Mutex::new(None);

fn with_sinks<R>(f: impl FnOnce(&mut HashMap<String, Sink>) -> R) -> R {
    f(SINKS.lock().get_or_insert_with(HashMap::new))
}

/// Handles a `file-transfer` payload if it belongs to one of our pulls. Returns `(status, ack)` like
/// `FileReceiver::handle_payload`, or None when the transfer is not ours.
pub fn on_transfer(
    from: &str,
    action: FileAction,
    id: &str,
    payload: &FilePayload,
) -> Option<(Option<String>, Option<bool>)> {
    with_sinks(|sinks| {
        if sinks.get(id).is_none_or(|s| s.from != from) {
            return None;
        }
        Some(match action {
            FileAction::Start => {
                let size = payload.size;
                if size == Some(sinks[id].size) {
                    (None, None)
                } else {
                    let sink = sinks.remove(id).unwrap();
                    sink.out.send(SinkMsg::End(false));
                    (Some(format!("Paste failed for {}: the file size changed", sink.name)), Some(false))
                }
            }
            FileAction::Chunk => {
                let bytes = payload.chunk.as_deref().filter(|b64| b64.len() <= 48_000).and_then(from_base64);
                sink_data(sinks, id, bytes)
            }
            FileAction::End => {
                let sink = sinks.remove(id).unwrap();
                let digest: String = sink.hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
                let ok = sink.received == sink.size && payload.sha256.as_deref() == Some(digest.as_str());
                sink.out.send(SinkMsg::End(ok));
                let status = if ok {
                    format!("Pasted: {}", sink.name)
                } else {
                    format!("Paste failed for {}: size or SHA-256 mismatch", sink.name)
                };
                (Some(status), Some(ok))
            }
            _ => (None, None),
        })
    })
}

/// Largest binary chunk a paste stream accepts (the sender's `BINARY_CHUNK_SIZE`).
const MAX_BINARY_CHUNK: usize = 128 * 1024;

/// A binary chunk (`file-pull-bin`) for a paste stream we requested; None when it belongs to no paste. The
/// relay keeps the order, so `offset` must be exactly where the stream stands.
pub fn on_chunk(from: &str, id: &str, offset: u64, data: &[u8]) -> Option<(Option<String>, Option<bool>)> {
    with_sinks(|sinks| {
        let sink = sinks.get(id).filter(|s| s.from == from)?;
        let ok = offset == sink.received && data.len() <= MAX_BINARY_CHUNK;
        Some(sink_data(sinks, id, ok.then(|| data.to_vec())))
    })
}

/// Hands one chunk to the paste stream `id`, or fails the paste when `bytes` is None or overruns the size.
fn sink_data(sinks: &mut HashMap<String, Sink>, id: &str, bytes: Option<Vec<u8>>) -> (Option<String>, Option<bool>) {
    let sink = sinks.get_mut(id).unwrap();
    match bytes.filter(|b| sink.received + b.len() as u64 <= sink.size) {
        Some(bytes) => {
            sink.hash.update(&bytes);
            sink.received += bytes.len() as u64;
            let pct = crate::domain::transfer::percent(sink.received, sink.size);
            let status = format!("Pasting {} ({}%)...", sink.name, pct);
            sink.out.send(SinkMsg::Data(bytes));
            (Some(status), None)
        }
        None => {
            let sink = sinks.remove(id).unwrap();
            sink.out.send(SinkMsg::End(false));
            (Some(format!("Paste failed for {}", sink.name)), Some(false))
        }
    }
}

/// Fails every paste in progress (relay connection lost).
pub fn abort_pulls() {
    with_sinks(|sinks| {
        for (_, sink) in sinks.drain() {
            sink.out.send(SinkMsg::End(false));
        }
    });
}

// ---------------------------------------------------------------------------------------------
// IStream that blocks on the pull sink.

#[repr(C)]
struct IStreamVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    read: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut u32) -> HRESULT,
    write: unsafe extern "system" fn(*mut c_void, *const c_void, u32, *mut u32) -> HRESULT,
    seek: unsafe extern "system" fn(*mut c_void, i64, u32, *mut u64) -> HRESULT,
    set_size: unsafe extern "system" fn(*mut c_void, u64) -> HRESULT,
    copy_to: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, *mut u64, *mut u64) -> HRESULT,
    commit: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    revert: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    lock_region: unsafe extern "system" fn(*mut c_void, u64, u64, u32) -> HRESULT,
    unlock_region: unsafe extern "system" fn(*mut c_void, u64, u64, u32) -> HRESULT,
    stat: unsafe extern "system" fn(*mut c_void, *mut STATSTG, u32) -> HRESULT,
    clone: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

struct StreamState {
    rx: Receiver<SinkMsg>,
    buf: Vec<u8>,
    off: usize,
    pos: u64,
    done: bool,
    failed: bool,
}

#[repr(C)]
struct PullStream {
    vtbl: *const IStreamVtbl,
    refs: AtomicU32,
    tid: String,
    size: u64,
    state: Mutex<StreamState>,
}

static STREAM_VTBL: IStreamVtbl = IStreamVtbl {
    query_interface: stream_qi,
    add_ref: stream_add_ref,
    release: stream_release,
    read: stream_read,
    write: stream_write,
    seek: stream_seek,
    set_size: stream_u64_notimpl,
    copy_to: stream_copy_to,
    commit: stream_commit,
    revert: stream_revert,
    lock_region: stream_region_notimpl,
    unlock_region: stream_region_notimpl,
    stat: stream_stat,
    clone: stream_clone,
};

unsafe extern "system" fn stream_qi(this: *mut c_void, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        if out.is_null() {
            return E_NOINTERFACE;
        }
        let iid = *riid;
        if iid == IID_IUNKNOWN || iid == IID_ISTREAM || iid == IID_ISEQUENTIALSTREAM {
            stream_add_ref(this);
            *out = this;
            S_OK
        } else {
            *out = ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn stream_add_ref(this: *mut c_void) -> u32 {
    unsafe { (*(this as *const PullStream)).refs.fetch_add(1, Ordering::SeqCst) + 1 }
}

unsafe extern "system" fn stream_release(this: *mut c_void) -> u32 {
    unsafe {
        let left = (*(this as *const PullStream)).refs.fetch_sub(1, Ordering::SeqCst) - 1;
        if left == 0 {
            let stream = Box::from_raw(this as *mut PullStream);
            // Paste finished or was cancelled: later bytes for this transfer are ignored.
            with_sinks(|sinks| sinks.remove(&stream.tid));
            if helper_streams(|m| m.remove(&stream.tid)).is_some() {
                crate::infra::clipboard_helper::to_agent(&ToAgent::Close { tid: stream.tid.clone() });
            }
        }
        left
    }
}

unsafe extern "system" fn stream_read(this: *mut c_void, pv: *mut c_void, cb: u32, pcb_read: *mut u32) -> HRESULT {
    unsafe {
        let stream = &*(this as *const PullStream);
        let mut st = stream.state.lock();
        let out = std::slice::from_raw_parts_mut(pv as *mut u8, cb as usize);
        let mut written = 0usize;
        // Fill the whole request (or reach EOF): a short read must not look like end of file.
        while written < out.len() && !st.failed {
            if st.off < st.buf.len() {
                let n = (st.buf.len() - st.off).min(out.len() - written);
                out[written..written + n].copy_from_slice(&st.buf[st.off..st.off + n]);
                st.off += n;
                written += n;
                continue;
            }
            if st.done {
                break;
            }
            match st.rx.recv_timeout(PULL_IDLE) {
                Ok(SinkMsg::Data(bytes)) => {
                    st.buf = bytes;
                    st.off = 0;
                }
                Ok(SinkMsg::End(true)) => st.done = true,
                Ok(SinkMsg::End(false)) | Err(_) => st.failed = true,
            }
        }
        st.pos += written as u64;
        if !pcb_read.is_null() {
            *pcb_read = written as u32;
        }
        if st.failed && written == 0 { E_FAIL } else { S_OK }
    }
}

unsafe extern "system" fn stream_write(_: *mut c_void, _: *const c_void, _: u32, _: *mut u32) -> HRESULT {
    STG_E_INVALIDFUNCTION
}

unsafe extern "system" fn stream_seek(this: *mut c_void, mv: i64, origin: u32, new_pos: *mut u64) -> HRESULT {
    unsafe {
        let stream = &*(this as *const PullStream);
        let pos = stream.state.lock().pos;
        // Forward-only network stream: only "where am I" queries are answerable.
        let target = match origin {
            STREAM_SEEK_SET => mv,
            STREAM_SEEK_CUR => pos as i64 + mv,
            _ => -1,
        };
        if target != pos as i64 {
            return STG_E_INVALIDFUNCTION;
        }
        if !new_pos.is_null() {
            *new_pos = pos;
        }
        S_OK
    }
}

unsafe extern "system" fn stream_u64_notimpl(_: *mut c_void, _: u64) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn stream_copy_to(_: *mut c_void, _: *mut c_void, _: u64, _: *mut u64, _: *mut u64) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn stream_commit(_: *mut c_void, _: u32) -> HRESULT {
    S_OK
}

unsafe extern "system" fn stream_revert(_: *mut c_void) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn stream_region_notimpl(_: *mut c_void, _: u64, _: u64, _: u32) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn stream_stat(this: *mut c_void, out: *mut STATSTG, _flags: u32) -> HRESULT {
    unsafe {
        if out.is_null() {
            return E_FAIL;
        }
        let stream = &*(this as *const PullStream);
        ptr::write_bytes(out, 0, 1);
        (*out).type_ = STGTY_STREAM;
        (*out).cb_size = stream.size;
        S_OK
    }
}

unsafe extern "system" fn stream_clone(_: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        if !out.is_null() {
            *out = ptr::null_mut();
        }
        E_NOTIMPL
    }
}

/// Starts pulling `index` from the offer and returns an owned IStream (refcount 1) that yields its bytes.
fn open_pull(offer: &Offer, index: usize) -> *mut c_void {
    let entry = &offer.files[index];
    let (name, size) = (entry.name.clone(), entry.size);
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros();
    let tid = format!("fp_{}_{}", ts, index);
    let (tx, rx) = mpsc::channel();
    logger::info("app.ft_sink_session", &format!("Pasting {} from offer {} ({}).", name, offer.offer_id, tid));
    match &offer.route {
        Route::Local(tx_out) => {
            let out = SinkOut::Local(tx);
            let sink = Sink { from: offer.from.clone(), name, size, received: 0, hash: Sha256::new(), out };
            with_sinks(|sinks| sinks.insert(tid.clone(), sink));
            let pull = FilePayload::pull(&offer.offer_id, entry.remote as u64, &tid);
            let _ = tx_out.send(Outgoing::Message(ClientMessage::file(&offer.from, pull)));
        }
        Route::Agent => {
            helper_streams(|m| m.insert(tid.clone(), tx));
            let (from, offer_id) = (offer.from.clone(), offer.offer_id.clone());
            let pull = ToAgent::Pull { from, offer_id, index, tid: tid.clone() };
            if !crate::infra::clipboard_helper::to_agent(&pull)
                && let Some(tx) = helper_streams(|m| m.remove(&tid))
            {
                let _ = tx.send(SinkMsg::End(false));
            }
        }
    }
    let stream = Box::new(PullStream {
        vtbl: &STREAM_VTBL,
        refs: AtomicU32::new(1),
        tid,
        size,
        state: Mutex::new(StreamState { rx, buf: Vec::new(), off: 0, pos: 0, done: false, failed: false }),
    });
    Box::into_raw(stream) as *mut c_void
}

// ---------------------------------------------------------------------------------------------
// IDataObject exposing the offer as virtual files.

#[repr(C)]
struct IDataObjectVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> HRESULT,
    get_data_here: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> HRESULT,
    query_get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC) -> HRESULT,
    get_canonical_format_etc: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut FORMATETC) -> HRESULT,
    set_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM, i32) -> HRESULT,
    enum_format_etc: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
    d_advise: unsafe extern "system" fn(*mut c_void, *const FORMATETC, u32, *mut c_void, *mut u32) -> HRESULT,
    d_unadvise: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    enum_d_advise: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct DataObject {
    vtbl: *const IDataObjectVtbl,
    refs: AtomicU32,
    offer: Arc<Offer>,
}

static DATA_VTBL: IDataObjectVtbl = IDataObjectVtbl {
    query_interface: data_qi,
    add_ref: data_add_ref,
    release: data_release,
    get_data: data_get_data,
    get_data_here: data_get_data_here,
    query_get_data: data_query_get_data,
    get_canonical_format_etc: data_canonical,
    set_data: data_set_data,
    enum_format_etc: data_enum_format_etc,
    d_advise: data_d_advise,
    d_unadvise: data_d_unadvise,
    enum_d_advise: data_enum_d_advise,
};

fn offered_formats() -> [FORMATETC; 3] {
    let f = formats();
    let fe = |cf: u16, tymed: u32| FORMATETC {
        cf_format: cf,
        ptd: ptr::null_mut(),
        dw_aspect: DVASPECT_CONTENT,
        lindex: -1,
        tymed,
    };
    [fe(f.descriptor, TYMED_HGLOBAL), fe(f.contents, TYMED_ISTREAM), fe(f.drop_effect, TYMED_HGLOBAL)]
}

/// Validates a request against our formats; Ok(cf) on match.
fn check_format(fmt: &FORMATETC) -> Result<u16, HRESULT> {
    let f = formats();
    let wanted = if fmt.cf_format == f.contents {
        TYMED_ISTREAM
    } else if fmt.cf_format == f.descriptor || fmt.cf_format == f.drop_effect {
        TYMED_HGLOBAL
    } else {
        return Err(DV_E_FORMATETC);
    };
    if fmt.dw_aspect != DVASPECT_CONTENT {
        return Err(DV_E_FORMATETC);
    }
    if fmt.tymed & wanted == 0 {
        return Err(DV_E_TYMED);
    }
    Ok(fmt.cf_format)
}

fn global_from_bytes(bytes: &[u8]) -> *mut c_void {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len().max(1));
        if h.is_null() {
            return h;
        }
        let p = GlobalLock(h) as *mut u8;
        if !p.is_null() {
            ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            GlobalUnlock(h);
        }
        h
    }
}

/// Unix milliseconds to FILETIME (100 ns ticks since 1601-01-01 UTC).
fn unix_ms_to_filetime(ms: u64) -> Option<u64> {
    ms.checked_mul(10_000)?.checked_add(116_444_736_000_000_000)
}

fn descriptor_bytes(files: &[OfferFile]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + files.len() * std::mem::size_of::<FILEDESCRIPTORW>());
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for f in files {
        let mut fd: FILEDESCRIPTORW = unsafe { std::mem::zeroed() };
        if f.dir {
            // Explorer recreates the tree from the relative names; folder entries also keep empty folders.
            fd.dw_flags = FD_ATTRIBUTES | FD_PROGRESSUI;
            fd.dw_file_attributes = FILE_ATTRIBUTE_DIRECTORY;
        } else {
            fd.dw_flags = FD_FILESIZE | FD_PROGRESSUI;
            fd.n_file_size_high = (f.size >> 32) as u32;
            fd.n_file_size_low = f.size as u32;
        }
        let filetime = |ms: Option<u64>| {
            ms.and_then(unix_ms_to_filetime).map(|ft| FILETIME { low: ft as u32, high: (ft >> 32) as u32 })
        };
        if let Some(ft) = filetime(f.mtime) {
            fd.dw_flags |= FD_WRITETIME;
            fd.ft_last_write_time = ft;
        }
        if let Some(ft) = filetime(f.ctime) {
            fd.dw_flags |= FD_CREATETIME;
            fd.ft_creation_time = ft;
        }
        if let Some(ft) = filetime(f.atime) {
            fd.dw_flags |= FD_ACCESSTIME;
            fd.ft_last_access_time = ft;
        }
        if f.attrs != 0 {
            // Explorer applies these to the pasted file, as it does for a local copy.
            fd.dw_flags |= FD_ATTRIBUTES;
            fd.dw_file_attributes |= f.attrs;
        }
        // Names were validated by set_offer (`\` separates folders); replace anything else Explorer cannot create.
        let clean: Vec<u16> = f
            .name
            .chars()
            .map(|c| if matches!(c, '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
            .collect::<String>()
            .encode_utf16()
            .take(259)
            .collect();
        fd.c_file_name[..clean.len()].copy_from_slice(&clean);
        let raw = unsafe {
            std::slice::from_raw_parts(
                &fd as *const FILEDESCRIPTORW as *const u8,
                std::mem::size_of::<FILEDESCRIPTORW>(),
            )
        };
        out.extend_from_slice(raw);
    }
    out
}

unsafe extern "system" fn data_qi(this: *mut c_void, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        if out.is_null() {
            return E_NOINTERFACE;
        }
        let iid = *riid;
        if iid == IID_IUNKNOWN || iid == IID_IDATAOBJECT {
            data_add_ref(this);
            *out = this;
            S_OK
        } else {
            *out = ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn data_add_ref(this: *mut c_void) -> u32 {
    unsafe { (*(this as *const DataObject)).refs.fetch_add(1, Ordering::SeqCst) + 1 }
}

unsafe extern "system" fn data_release(this: *mut c_void) -> u32 {
    unsafe {
        let left = (*(this as *const DataObject)).refs.fetch_sub(1, Ordering::SeqCst) - 1;
        if left == 0 {
            drop(Box::from_raw(this as *mut DataObject));
        }
        left
    }
}

unsafe extern "system" fn data_get_data(this: *mut c_void, fmt: *const FORMATETC, medium: *mut STGMEDIUM) -> HRESULT {
    unsafe {
        if fmt.is_null() || medium.is_null() {
            return E_FAIL;
        }
        let obj = &*(this as *const DataObject);
        let fmt = &*fmt;
        let cf = match check_format(fmt) {
            Ok(cf) => cf,
            Err(hr) => return hr,
        };
        let f = formats();
        (*medium).p_unk_for_release = ptr::null_mut();
        if cf == f.contents {
            let Ok(index) = usize::try_from(fmt.lindex) else { return DV_E_LINDEX };
            // Folder entries have no contents; Explorer creates them from the descriptor alone.
            if obj.offer.files.get(index).is_none_or(|f| f.dir) {
                return DV_E_LINDEX;
            }
            (*medium).tymed = TYMED_ISTREAM;
            (*medium).data = open_pull(&obj.offer, index);
            return S_OK;
        }
        let bytes = if cf == f.descriptor {
            descriptor_bytes(&obj.offer.files)
        } else {
            DROPEFFECT_COPY.to_le_bytes().to_vec()
        };
        let h = global_from_bytes(&bytes);
        if h.is_null() {
            return E_OUTOFMEMORY;
        }
        (*medium).tymed = TYMED_HGLOBAL;
        (*medium).data = h;
        S_OK
    }
}

unsafe extern "system" fn data_get_data_here(_: *mut c_void, _: *const FORMATETC, _: *mut STGMEDIUM) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn data_query_get_data(_: *mut c_void, fmt: *const FORMATETC) -> HRESULT {
    if fmt.is_null() {
        return DV_E_FORMATETC;
    }
    match check_format(unsafe { &*fmt }) {
        Ok(_) => S_OK,
        Err(hr) => hr,
    }
}

unsafe extern "system" fn data_canonical(_: *mut c_void, _: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
    if !out.is_null() {
        unsafe { (*out).ptd = ptr::null_mut() };
    }
    E_NOTIMPL
}

/// `CLSID_RecycleBin` {645FF040-5081-101B-9F08-00AA002F954E}: a drop target that only deletes.
const CLSID_RECYCLE_BIN: [u8; 16] =
    [0x40, 0xF0, 0x5F, 0x64, 0x81, 0x50, 0x1B, 0x10, 0x9F, 0x08, 0x00, 0xAA, 0x00, 0x2F, 0x95, 0x4E];

/// Bytes of an HGLOBAL medium (at most `max`).
fn global_bytes(h: *mut c_void, max: usize) -> Vec<u8> {
    unsafe {
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return Vec::new();
        }
        let out = std::slice::from_raw_parts(p, GlobalSize(h).min(max)).to_vec();
        GlobalUnlock(h);
        out
    }
}

/// The paste target reports back (AnyDesk's `ole.data` does the same): which effect it performed, that the
/// paste succeeded, or which target it was. Logged so a paste is never silent; other formats are refused.
unsafe extern "system" fn data_set_data(
    this: *mut c_void,
    fmt: *const FORMATETC,
    medium: *mut STGMEDIUM,
    release: i32,
) -> HRESULT {
    unsafe {
        if fmt.is_null() || medium.is_null() {
            return E_FAIL;
        }
        let f = formats();
        let cf = (*fmt).cf_format;
        let known = [f.performed, f.logical_performed, f.paste_succeeded, f.target_clsid];
        if !known.contains(&cf) || (*medium).tymed != TYMED_HGLOBAL || (*medium).data.is_null() {
            // Refused: the medium stays with the caller.
            return E_NOTIMPL;
        }
        let bytes = global_bytes((*medium).data, 16);
        let dword = bytes.get(..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
        let offer = &(*(this as *const DataObject)).offer;
        let what = if cf == f.paste_succeeded {
            Some(format!("Paste succeeded (effect {dword})"))
        } else if cf == f.performed {
            Some(format!("Paste target performed effect {dword}"))
        } else if cf == f.target_clsid && bytes.as_slice() == CLSID_RECYCLE_BIN {
            Some("Dropped on the Recycle Bin; nothing pulled".to_string())
        } else {
            None
        };
        if let Some(what) = what {
            logger::info("clipbrd.capture", &format!("{what} for offer {} from {}.", offer.offer_id, offer.from));
        }
        // Accepted: with `release` the medium is ours to free.
        if release != 0 {
            ReleaseStgMedium(medium);
        }
        S_OK
    }
}

unsafe extern "system" fn data_enum_format_etc(_: *mut c_void, direction: u32, out: *mut *mut c_void) -> HRESULT {
    if direction != DATADIR_GET || out.is_null() {
        return E_NOTIMPL;
    }
    let fmts = offered_formats();
    unsafe { SHCreateStdEnumFmtEtc(fmts.len() as u32, fmts.as_ptr(), out) }
}

unsafe extern "system" fn data_d_advise(
    _: *mut c_void,
    _: *const FORMATETC,
    _: u32,
    _: *mut c_void,
    _: *mut u32,
) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn data_d_unadvise(_: *mut c_void, _: u32) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn data_enum_d_advise(_: *mut c_void, _: *mut *mut c_void) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

// ---------------------------------------------------------------------------------------------
// Drag onto the pointer (`file-drop`).

const DRAGDROP_S_DROP: HRESULT = 0x0004_0100;
const DRAGDROP_S_CANCEL: HRESULT = 0x0004_0101;
const DRAGDROP_S_USEDEFAULTCURSORS: HRESULT = 0x0004_0102;
/// How long the drag hovers over the target before the drop, so it sees `DragEnter` and `DragOver` first.
const DROP_HOVER: std::time::Duration = std::time::Duration::from_millis(400);

#[repr(C)]
struct IDropSourceVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    query_continue_drag: unsafe extern "system" fn(*mut c_void, i32, u32) -> HRESULT,
    give_feedback: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
}

#[repr(C)]
struct DropSource {
    vtbl: *const IDropSourceVtbl,
    started: std::time::Instant,
    released: std::sync::atomic::AtomicBool,
}

static DROP_SOURCE_VTBL: IDropSourceVtbl = IDropSourceVtbl {
    query_interface: source_qi,
    add_ref: source_ref,
    release: source_ref,
    query_continue_drag: source_continue,
    give_feedback: source_feedback,
};

// `[i32; 2]` is the same layout as the POINT other modules declare.
#[allow(clashing_extern_declarations)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetCursorPos(pt: *mut [i32; 2]) -> i32;
    fn SetCursorPos(x: i32, y: i32) -> i32;
    fn mouse_event(flags: u32, dx: i32, dy: i32, data: u32, extra: usize);
}

const MK_LBUTTON: u32 = 1;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;

unsafe extern "system" fn source_qi(this: *mut c_void, _riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    // Only IUnknown and IDropSource are ever asked for, and both are this object.
    unsafe { *out = this };
    S_OK
}

/// The source lives on the stack of `drag_onto_pointer`; OLE's references never outlive `DoDragDrop`.
unsafe extern "system" fn source_ref(_: *mut c_void) -> u32 {
    1
}

unsafe extern "system" fn source_continue(this: *mut c_void, escape: i32, keys: u32) -> HRESULT {
    // SAFETY: `this` is the `DropSource` passed to `DoDragDrop`.
    let source = unsafe { &*(this as *const DropSource) };
    if escape != 0 {
        return DRAGDROP_S_CANCEL;
    }
    if source.released.load(Ordering::SeqCst) {
        // Our button press is over: this is the drop.
        return if keys & MK_LBUTTON == 0 { DRAGDROP_S_DROP } else { S_OK };
    }
    if source.started.elapsed() >= DROP_HOVER {
        // Release the held button; OLE has the mouse captured, so only the drag sees it.
        source.released.store(true, Ordering::SeqCst);
        unsafe { mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0) };
    }
    S_OK
}

unsafe extern "system" fn source_feedback(_: *mut c_void, _effect: u32) -> HRESULT {
    DRAGDROP_S_USEDEFAULTCURSORS
}

/// What the catcher window's button press starts: the data object to drag and where the pointer is.
static DRAG_JOB: Mutex<Option<(usize, [i32; 2])>> = Mutex::new(None);
/// Verdict of the last drag; `None` while it runs.
static DRAG_RESULT: Mutex<Option<HRESULT>> = Mutex::new(None);

#[repr(C)]
struct WNDCLASSW {
    style: u32,
    wnd_proc: unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize,
    cls_extra: i32,
    wnd_extra: i32,
    instance: *mut c_void,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: *const u16,
    class_name: *const u16,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(class: *const WNDCLASSW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: *mut c_void,
        menu: *mut c_void,
        instance: *mut c_void,
        param: *mut c_void,
    ) -> *mut c_void;
    fn DefWindowProcW(hwnd: *mut c_void, msg: u32, w: usize, l: isize) -> isize;
    fn DestroyWindow(hwnd: *mut c_void) -> i32;
    fn SetWindowPos(hwnd: *mut c_void, after: *mut c_void, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    fn ReleaseCapture() -> i32;
}

const WM_LBUTTONDOWN: u32 = 0x0201;
const WS_POPUP: u32 = 0x8000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_EX_TOPMOST: u32 = 0x8;
const WS_EX_TOOLWINDOW: u32 = 0x80;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const SWP_NOSIZE: u32 = 0x1;
const SWP_NOACTIVATE: u32 = 0x10;

/// Window under the pointer for one drag. A drag can only start from a button press inside a window of this
/// thread (Windows gives that window the mouse), so the press lands here, not in the application below, which
/// therefore never sees a click. The window then moves away and the drag runs from its message.
unsafe extern "system" fn catcher_proc(hwnd: *mut c_void, msg: u32, w: usize, l: isize) -> isize {
    if msg == WM_LBUTTONDOWN {
        let job = DRAG_JOB.lock().take();
        if let Some((obj, home)) = job {
            // SAFETY: plain Win32 calls; `obj` is the data object `drag_onto_pointer` keeps alive until it ends.
            let hr = unsafe {
                SetWindowPos(hwnd, ptr::null_mut(), -32000, -32000, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
                let mut source = DropSource {
                    vtbl: &DROP_SOURCE_VTBL,
                    started: std::time::Instant::now(),
                    released: std::sync::atomic::AtomicBool::new(false),
                };
                // OLE looks at the drag when the mouse moves, and nobody is moving it: nudge it one pixel and
                // back so the target gets its `DragEnter` / `DragOver` here.
                let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let nudger = {
                    let done = Arc::clone(&done);
                    std::thread::spawn(move || {
                        let mut step = 0;
                        while !done.load(Ordering::SeqCst) {
                            SetCursorPos(home[0] + step % 2, home[1]);
                            step += 1;
                            std::thread::sleep(Duration::from_millis(15));
                        }
                    })
                };
                let mut effect = 0u32;
                let hr = DoDragDrop(
                    obj as *mut c_void,
                    &mut source as *mut DropSource as *mut c_void,
                    DROPEFFECT_COPY,
                    &mut effect,
                );
                if !source.released.load(Ordering::SeqCst) {
                    mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
                }
                done.store(true, Ordering::SeqCst);
                let _ = nudger.join();
                SetCursorPos(home[0], home[1]);
                ReleaseCapture();
                hr
            };
            *DRAG_RESULT.lock() = Some(hr);
        }
        return 0;
    }
    // SAFETY: forwards a message to the default procedure.
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

/// OLE thread: drags `offer` (as virtual files) onto what is under the pointer and drops it there.
fn drag_onto_pointer(offer: Offer) {
    let name = offer.files.first().map(|f| f.name.clone()).unwrap_or_default();
    let obj = Box::into_raw(Box::new(DataObject { vtbl: &DATA_VTBL, refs: AtomicU32::new(1), offer: Arc::new(offer) }))
        as *mut c_void;
    let wide = |t: &str| t.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let class = wide("OpenDeskDropCatcher");
    let mut home = [0, 0];
    // SAFETY: plain Win32 calls; the window, the data object and the job outlive the message loop below.
    let hr = unsafe {
        GetCursorPos(&mut home);
        let wc = WNDCLASSW {
            style: 0,
            wnd_proc: catcher_proc,
            cls_extra: 0,
            wnd_extra: 0,
            instance: ptr::null_mut(),
            icon: ptr::null_mut(),
            cursor: ptr::null_mut(),
            background: ptr::null_mut(),
            menu_name: ptr::null(),
            class_name: class.as_ptr(),
        };
        RegisterClassW(&wc); // already registered on later drags: harmless
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP | WS_VISIBLE,
            home[0] - 2,
            home[1] - 2,
            5,
            5,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
        *DRAG_RESULT.lock() = None;
        *DRAG_JOB.lock() = Some((obj as usize, home));
        let mut result = None;
        if !hwnd.is_null() {
            mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
            // The press arrives as a message of this thread; the drag then runs inside it.
            let mut msg: MSG = std::mem::zeroed();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while result.is_none() && std::time::Instant::now() < deadline {
                while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, 1) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                result = *DRAG_RESULT.lock();
                std::thread::sleep(Duration::from_millis(5));
            }
            DestroyWindow(hwnd);
        }
        DRAG_JOB.lock().take();
        result
    };
    let verdict = if hr == Some(DRAGDROP_S_DROP) { "accepted" } else { "not accepted" };
    logger::info("clipbrd.capture", &format!("Dropped {name} at {},{}: {verdict}", home[0], home[1]));
    // SAFETY: `obj` is our own reference, created above.
    unsafe { data_release(obj) };
}

// ---------------------------------------------------------------------------------------------
// OLE clipboard thread (STA with a message loop, as OleSetClipboard requires).

enum Cmd {
    Set(Offer),
    Drop(Offer),
    Clear(String),
}

struct OleThread {
    tx: Sender<Cmd>,
    tid: u32,
}

fn ole_thread() -> &'static OleThread {
    static T: OnceLock<OleThread> = OnceLock::new();
    T.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::channel();
        crate::infra::thread::spawn_named("clipboard-ole", move || unsafe {
            OleInitialize(ptr::null_mut());
            let mut msg: MSG = std::mem::zeroed();
            PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_NOREMOVE); // create the queue before anyone posts
            let _ = ready_tx.send(GetCurrentThreadId());
            // Our own reference to the object we put on the clipboard, with the partner it came from.
            let mut current: Option<(*mut c_void, String)> = None;
            while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                if msg.hwnd.is_null() && msg.message == WM_APP {
                    while let Ok(cmd) = rx.try_recv() {
                        match cmd {
                            Cmd::Set(offer) => {
                                let from = offer.from.clone();
                                let obj = Box::into_raw(Box::new(DataObject {
                                    vtbl: &DATA_VTBL,
                                    refs: AtomicU32::new(1),
                                    offer: Arc::new(offer),
                                })) as *mut c_void;
                                let hr = OleSetClipboard(obj);
                                if hr != S_OK {
                                    logger::warn("clipbrd.capture", &format!("OleSetClipboard failed: 0x{:08x}", hr));
                                }
                                if let Some((old, _)) = current.replace((obj, from)) {
                                    data_release(old);
                                }
                            }
                            Cmd::Drop(offer) => drag_onto_pointer(offer),
                            Cmd::Clear(partner) => {
                                if let Some((obj, _)) =
                                    current.take_if(|(_, from)| *from == partner || partner.is_empty())
                                {
                                    // Only empty the clipboard if the user has not copied something else since.
                                    if OleIsCurrentClipboard(obj) == S_OK {
                                        OleSetClipboard(ptr::null_mut());
                                    }
                                    data_release(obj);
                                }
                            }
                        }
                    }
                    continue;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
        let tid = ready_rx.recv().unwrap_or(0);
        OleThread { tx, tid }
    })
}

fn post(cmd: Cmd) {
    let t = ole_thread();
    let _ = t.tx.send(cmd);
    unsafe {
        PostThreadMessageW(t.tid, WM_APP, 0, 0);
    }
}

/// Validates the partner's file list. Entries with unsafe names (absolute, `..`, devices, ...) are dropped;
/// a list over the entry or size limit is refused with a status line.
fn parse_offer_files(list: &[OfferEntry]) -> Result<Vec<OfferFile>, String> {
    if list.len() > MAX_TREE_ENTRIES {
        return Err(format!("File list refused: more than {} items", MAX_TREE_ENTRIES));
    }
    let files: Vec<OfferFile> = list
        .iter()
        .enumerate()
        .filter_map(|(remote, f)| {
            let comps = safe_rel_path(&f.name)?;
            let dir = f.dir == Some(true);
            let size = if dir { 0 } else { f.size.filter(|s| *s <= MAX_FILE_BYTES)? };
            let attrs = f.attrs.unwrap_or(0) & crate::service::file_transfer::CARRIED_ATTRS;
            Some(OfferFile {
                name: comps.join("\\"),
                size,
                dir,
                mtime: f.mtime,
                ctime: f.ctime,
                atime: f.atime,
                attrs,
                remote,
            })
        })
        .collect();
    if files.iter().map(|f| f.size).sum::<u64>() > MAX_TREE_BYTES {
        return Err(format!("File list refused: total exceeds {} GB", MAX_TREE_BYTES >> 30));
    }
    Ok(files)
}

/// Partner copied files: expose them on our clipboard as virtual files. Nothing is downloaded yet.
pub fn set_offer(from: &str, payload: &FilePayload, tx_out: Sender<Outgoing>) -> Option<String> {
    place_offer(from, payload, tx_out, false)
}

/// Partner dropped files on our screen: the pointer is already there. Drag them onto whatever is under it, as
/// if a local Explorer window had been dragged from; the clipboard stays as it was.
pub fn drop_offer(from: &str, payload: &FilePayload, tx_out: Sender<Outgoing>) -> Option<String> {
    place_offer(from, payload, tx_out, true)
}

fn place_offer(from: &str, payload: &FilePayload, tx_out: Sender<Outgoing>, drop: bool) -> Option<String> {
    let offer_id = payload.id.clone();
    let files = match parse_offer_files(payload.files.as_deref()?) {
        Ok(files) if files.is_empty() => return None,
        Ok(files) => files,
        Err(status) => return Some(status),
    };
    let tops: Vec<&OfferFile> = files.iter().filter(|f| !f.name.contains('\\')).collect();
    let status = match tops.as_slice() {
        [one] => format!("Ready to paste: {} (Ctrl+V)", one.name),
        _ if files.iter().all(|f| !f.dir) => format!("Ready to paste: {} files (Ctrl+V)", files.len()),
        _ => format!("Ready to paste: {} items (Ctrl+V)", tops.len()),
    };
    logger::info("clipbrd.capture", &format!("Received file offer {} ({} files).", offer_id, files.len()));
    if crate::infra::clipboard_helper::ready() {
        // SYSTEM agent: the helper puts it on the user's clipboard; we keep our copy to check its pulls.
        let msg =
            ToHelper::Set { from: from.to_string(), offer_id: offer_id.clone(), files: payload.files.clone()?, drop };
        let mut offers = AGENT_OFFERS.lock();
        offers.push(Offer { from: from.to_string(), offer_id, files, route: Route::Local(tx_out) });
        let excess = offers.len().saturating_sub(KEPT_AGENT_OFFERS);
        offers.drain(..excess);
        std::mem::drop(offers);
        if crate::infra::clipboard_helper::send(&msg) {
            return Some(status);
        }
        logger::warn("clipbrd.capture", "Clipboard helper unavailable; offer not placed.");
        return None;
    }
    let offer = Offer { from: from.to_string(), offer_id, files, route: Route::Local(tx_out) };
    post(if drop { Cmd::Drop(offer) } else { Cmd::Set(offer) });
    Some(if drop { "Dropping files...".to_string() } else { status })
}

/// Session with `partner` ended (empty = any): its offers can no longer be pulled.
pub fn clear_offer(partner: &str) {
    post(Cmd::Clear(partner.to_string()));
    AGENT_OFFERS.lock().retain(|o| !partner.is_empty() && o.from != partner);
    crate::infra::clipboard_helper::send(&ToHelper::Clear { partner: partner.to_string() });
}

// ---------------------------------------------------------------------------------------------
// Split between the SYSTEM agent and the clipboard helper (see `infra::clipboard_helper`).

/// Offers the agent passed to the helper, newest last; the helper may pull only from these.
static AGENT_OFFERS: Mutex<Vec<Offer>> = Mutex::new(Vec::new());
/// Offers kept for pulls (same as the sender's `KEPT_OFFERS`).
const KEPT_AGENT_OFFERS: usize = 4;
/// Helper process: paste streams waiting for bytes from the agent, by transfer ID.
static HELPER_STREAMS: Mutex<Option<HashMap<String, Sender<SinkMsg>>>> = Mutex::new(None);

fn helper_streams<R>(f: impl FnOnce(&mut HashMap<String, Sender<SinkMsg>>) -> R) -> R {
    f(HELPER_STREAMS.lock().get_or_insert_with(HashMap::new))
}

/// Agent: the helper asks to pull or closes a paste stream. Every pull is checked against our own offers.
pub fn helper_request(msg: ToAgent) {
    use crate::infra::clipboard_helper::send;
    match msg {
        ToAgent::Pull { from, offer_id, index, tid } => {
            if !is_safe_id(&tid) || !tid.starts_with("fp_") {
                return;
            }
            let offers = AGENT_OFFERS.lock();
            let offer = offers.iter().find(|o| o.from == from && o.offer_id == offer_id);
            let entry = offer.and_then(|o| o.files.get(index)).filter(|f| !f.dir);
            let (Some(offer), Some(entry)) = (offer, entry) else {
                logger::warn("clipbrd.capture", "Clipboard helper asked for an unknown offer entry; refused.");
                send(&ToHelper::End { tid, ok: false });
                return;
            };
            let Route::Local(tx_out) = &offer.route else { return };
            logger::info(
                "app.ft_sink_session",
                &format!("Helper pastes {} from offer {offer_id} ({tid}).", entry.name),
            );
            let (name, size, out) = (entry.name.clone(), entry.size, SinkOut::Helper(tid.clone()));
            let sink = Sink { from: from.clone(), name, size, received: 0, hash: Sha256::new(), out };
            with_sinks(|sinks| sinks.insert(tid.clone(), sink));
            let pull = FilePayload::pull(&offer_id, entry.remote as u64, &tid);
            let _ = tx_out.send(Outgoing::Message(ClientMessage::file(&from, pull)));
        }
        ToAgent::Close { tid } => {
            with_sinks(|sinks| sinks.remove(&tid));
        }
    }
}

/// Agent: the helper is gone; its paste streams and offers went with it.
pub fn helper_lost() {
    with_sinks(|sinks| sinks.retain(|_, s| matches!(s.out, SinkOut::Local(_))));
    AGENT_OFFERS.lock().clear();
}

/// Helper: a command from the agent.
pub fn helper_command(msg: ToHelper) {
    match msg {
        ToHelper::Set { from, offer_id, files, drop } => match parse_offer_files(&files) {
            Ok(files) if !files.is_empty() => {
                let offer = Offer { from, offer_id, files, route: Route::Agent };
                post(if drop { Cmd::Drop(offer) } else { Cmd::Set(offer) })
            }
            _ => logger::warn("clipbrd.capture", "Agent sent an unusable offer; ignored."),
        },
        ToHelper::Clear { partner } => post(Cmd::Clear(partner)),
        ToHelper::End { tid, ok } => {
            if let Some(tx) = helper_streams(|m| m.get(&tid).cloned()) {
                let _ = tx.send(SinkMsg::End(ok));
            }
        }
    }
}

/// Helper: verified bytes for paste stream `tid`.
pub fn helper_data(tid: &str, bytes: Vec<u8>) {
    if let Some(tx) = helper_streams(|m| m.get(tid).cloned()) {
        let _ = tx.send(SinkMsg::Data(bytes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::thread;

    fn payload(v: Value) -> FilePayload {
        serde_json::from_value(v).unwrap()
    }

    fn entries(v: Value) -> Vec<OfferEntry> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn descriptor_layout_matches_windows() {
        assert_eq!(std::mem::size_of::<FILEDESCRIPTORW>(), 592);
        assert_eq!(std::mem::size_of::<STATSTG>(), 80);
        let bytes = descriptor_bytes(&[OfferFile {
            name: "a:b.txt".into(),
            size: 5_000_000_000,
            dir: false,
            mtime: None,
            ctime: None,
            atime: None,
            attrs: 0,
            remote: 0,
        }]);
        assert_eq!(bytes.len(), 4 + 592);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 1);
        let fd = &bytes[4..];
        assert_eq!(u32::from_le_bytes(fd[0..4].try_into().unwrap()), FD_FILESIZE | FD_PROGRESSUI);
        assert_eq!(u32::from_le_bytes(fd[64..68].try_into().unwrap()), 1); // size high
        let name: Vec<u16> = fd[72..86].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16_lossy(&name), "a_b.txt");
    }

    #[test]
    fn descriptor_carries_write_time() {
        // 2024-01-01T00:00:00Z = 1_704_067_200_000 ms = FILETIME 133_485_408_000_000_000.
        let file = OfferFile {
            name: "a.txt".into(),
            size: 1,
            dir: false,
            mtime: Some(1_704_067_200_000),
            ctime: None,
            atime: None,
            attrs: 0,
            remote: 0,
        };
        let fd = &descriptor_bytes(&[file])[4..];
        assert_eq!(u32::from_le_bytes(fd[0..4].try_into().unwrap()), FD_FILESIZE | FD_PROGRESSUI | FD_WRITETIME);
        let low = u32::from_le_bytes(fd[56..60].try_into().unwrap()) as u64;
        let high = u32::from_le_bytes(fd[60..64].try_into().unwrap()) as u64;
        assert_eq!(high << 32 | low, 133_485_408_000_000_000);
        assert_eq!(unix_ms_to_filetime(u64::MAX), None);
    }

    /// Puts `offer` on the clipboard, pastes it with Explorer into a fresh temp folder and plays the partner,
    /// answering each pull with `contents[remote index]`. Returns the folder, or None in manual mode.
    fn paste_with_explorer(tag: &str, offer: Value, contents: &HashMap<u64, Vec<u8>>) -> Option<std::path::PathBuf> {
        // Both ignored tests use the one system clipboard.
        static CLIPBOARD: Mutex<()> = Mutex::new(());
        let _guard = CLIPBOARD.lock();
        let dir = std::env::temp_dir().join(format!("opendesk-paste-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (tx_out, rx_out) = mpsc::channel();
        assert!(set_offer("111", &payload(offer), tx_out).is_some());
        thread::sleep(Duration::from_millis(300));

        // A fresh process must hold the OLE clipboard object (as Explorer always does for its Paste menu state)
        // before pasting; otherwise Shell reads through Win32 clipboard emulation, which has no per-file lindex.
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $null=[System.Windows.Forms.Clipboard]::GetDataObject().GetFormats(); $f=(New-Object -ComObject Shell.Application).NameSpace('{}'); $f.Self.InvokeVerb('Paste'); Start-Sleep -Seconds 10",
            dir.display()
        );
        // OPENDESK_MANUAL_PASTE=1: paste by hand (Ctrl+V in any Explorer folder) instead of the scripted paste.
        let manual = std::env::var("OPENDESK_MANUAL_PASTE").is_ok();
        let mut ps = if manual {
            println!("Clipboard now holds the virtual {}: press Ctrl+V in an Explorer folder within 60 s.", tag);
            None
        } else {
            Some(
                std::process::Command::new("powershell")
                    .args(["-NoProfile", "-STA", "-Command", &script])
                    .spawn()
                    .unwrap(),
            )
        };

        // Play the partner: answer every pull with start/chunk/end.
        for _ in 0..contents.len() {
            let Outgoing::Message(ClientMessage::RelayFile { payload: pull, .. }) =
                rx_out.recv_timeout(Duration::from_secs(if manual { 60 } else { 40 })).expect("Explorer never pulled")
            else {
                panic!("unexpected message")
            };
            let tid = pull.tid.unwrap();
            let index = pull.index.unwrap();
            let data = contents.get(&index).expect("pull for a folder or unknown entry");
            on_transfer("111", FileAction::Start, &tid, &payload(json!({ "size": data.len() })));
            for chunk in data.chunks(32768) {
                on_transfer(
                    "111",
                    FileAction::Chunk,
                    &tid,
                    &payload(json!({ "chunk": core_engine::codec::to_base64(chunk) })),
                );
            }
            let sha: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(
                on_transfer("111", FileAction::End, &tid, &payload(json!({ "sha256": sha }))).unwrap().1,
                Some(true)
            );
        }

        let result = if let Some(ps) = ps.as_mut() {
            let _ = ps.wait();
            Some(dir)
        } else {
            thread::sleep(Duration::from_secs(3)); // let Explorer finish writing; check the pasted files by hand
            None
        };
        clear_offer("");
        result
    }

    /// A window of another process accepts a drag started with no mouse button held, at the pointer position.
    /// Moves the real pointer, so run it by hand: `cargo test file_drop_reaches_a_window -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn file_drop_reaches_a_window_under_the_pointer() {
        let dir = std::env::temp_dir().join(format!("opendesk-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (ready, got) = (dir.join("ready"), dir.join("formats"));
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $f=New-Object System.Windows.Forms.Form; \
             $f.StartPosition='Manual'; $f.Location=New-Object System.Drawing.Point(300,300); $f.Size=New-Object System.Drawing.Size(400,300); \
             $f.TopMost=$true; $f.AllowDrop=$true; \
             $f.Add_DragEnter({{ $_.Effect='Copy' }}); \
             $f.Add_DragDrop({{ [IO.File]::WriteAllText('{got}', ($_.Data.GetFormats() -join ',')); $f.Close() }}); \
             $f.Add_Shown({{ [IO.File]::WriteAllText('{ready}', '1') }}); \
             $t=New-Object System.Windows.Forms.Timer; $t.Interval=20000; $t.Add_Tick({{ $f.Close() }}); $t.Start(); \
             [void]$f.ShowDialog()",
            got = got.display(),
            ready = ready.display()
        );
        let mut ps =
            std::process::Command::new("powershell").args(["-NoProfile", "-STA", "-Command", &script]).spawn().unwrap();
        for _ in 0..100 {
            if ready.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(ready.exists(), "the target window never opened");
        let (tx_out, _rx) = mpsc::channel();
        let offer = json!({ "id": "of_d", "files": [{ "name": "dropped.bin", "size": 10 }] });
        // The host's dispatcher puts the pointer on the spot first.
        unsafe { SetCursorPos(500, 450) };
        thread::sleep(Duration::from_millis(200));
        assert!(drop_offer("111", &payload(offer), tx_out).is_some());
        for _ in 0..80 {
            if got.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let _ = ps.kill();
        let _ = ps.wait();
        let formats = std::fs::read_to_string(&got).expect("the window never received the drop");
        assert!(formats.contains("FileGroupDescriptorW"), "formats: {formats}");
        clear_offer("");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Real Explorer paste across processes. Overwrites the clipboard, so run it by hand:
    /// `cargo test --release explorer_paste -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn explorer_paste_pulls_virtual_file() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let offer = json!({ "id": "of_t", "files": [{ "name": "pasted.bin", "size": data.len() }] });
        if let Some(dir) = paste_with_explorer("file", offer, &HashMap::from([(0, data.clone())])) {
            let pasted = std::fs::read(dir.join("pasted.bin")).expect("Explorer did not create the file");
            assert_eq!(pasted, data);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// The service setup: a separate helper process holds the clipboard object and gets the bytes from this
    /// process (the agent) over its pipes. Needs the app built first:
    /// `cargo build --release && cargo test --release explorer_paste_through_helper -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn explorer_paste_through_helper_process() {
        let deps = std::env::current_exe().unwrap();
        let exe = deps.parent().unwrap().parent().unwrap().join("opendesk-client.exe");
        let mut helper = crate::infra::clipboard_helper::start_test_helper(&exe);
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 13 % 251) as u8).collect();
        let small = b"second file".to_vec();
        let offer = json!({ "id": "of_h", "files": [
            { "name": "big.bin", "size": data.len(), "mtime": 1_704_067_200_000u64 },
            { "name": "small.txt", "size": small.len() }
        ]});
        let contents = HashMap::from([(0, data.clone()), (1, small.clone())]);
        let dir = paste_with_explorer("helper", offer, &contents);
        let _ = helper.kill();
        let _ = helper.wait();
        if let Some(dir) = dir {
            assert_eq!(std::fs::read(dir.join("big.bin")).expect("big.bin not pasted"), data);
            assert_eq!(std::fs::read(dir.join("small.txt")).expect("small.txt not pasted"), small);
            let modified = std::fs::metadata(dir.join("big.bin")).unwrap().modified().unwrap();
            let ms = modified.duration_since(UNIX_EPOCH).unwrap().as_millis();
            assert_eq!(ms, 1_704_067_200_000, "Date Modified kept (FN1)");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Same, for several top-level files and no folder (a multi-file copy in Explorer): every file must be
    /// pulled and pasted, not only the first.
    #[test]
    #[ignore]
    fn explorer_paste_pulls_four_top_level_files() {
        let names = ["a.txt", "b.bin", "c.dat", "d.log"];
        let contents: HashMap<u64, Vec<u8>> = (0..names.len() as u64)
            .map(|i| (i, (0..40_000 + i as u32 * 30_000).map(|b| (b * (i as u32 + 3) % 251) as u8).collect()))
            .collect();
        let files: Vec<Value> =
            names.iter().enumerate().map(|(i, n)| json!({ "name": n, "size": contents[&(i as u64)].len() })).collect();
        let offer = json!({ "id": "of_four", "files": files });
        if let Some(dir) = paste_with_explorer("four", offer, &contents) {
            for (i, name) in names.iter().enumerate() {
                let pasted = std::fs::read(dir.join(name)).unwrap_or_else(|_| panic!("{name} was not pasted"));
                assert_eq!(pasted, contents[&(i as u64)], "{name}");
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Same, for a nested folder with an empty subfolder; an unsafe entry in the offer must be dropped and the
    /// remaining pulls must still name the partner's own indices.
    #[test]
    #[ignore]
    fn explorer_paste_recreates_nested_folder() {
        let main_rs = b"fn main() {}\n".to_vec();
        let big: Vec<u8> = (0..150_000u32).map(|i| (i * 13 % 251) as u8).collect();
        let offer = json!({ "id": "of_tree", "files": [
            { "name": "Proyek", "dir": true },
            { "name": "..\\escape.txt", "size": 3 },
            { "name": "Proyek\\src", "dir": true },
            { "name": "Proyek\\src\\main.rs", "size": main_rs.len() },
            { "name": "Proyek\\kosong", "dir": true },
            { "name": "Proyek\\src\\data.bin", "size": big.len() },
        ]});
        let contents = HashMap::from([(3, main_rs.clone()), (5, big.clone())]);
        if let Some(dir) = paste_with_explorer("tree", offer, &contents) {
            assert_eq!(std::fs::read(dir.join(r"Proyek\src\main.rs")).expect("main.rs missing"), main_rs);
            assert_eq!(std::fs::read(dir.join(r"Proyek\src\data.bin")).expect("data.bin missing"), big);
            assert!(dir.join(r"Proyek\kosong").is_dir(), "empty folder not recreated");
            assert!(!dir.join("escape.txt").exists() && !dir.parent().unwrap().join("escape.txt").exists());
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn descriptor_marks_nested_folders_and_relative_names() {
        let files = parse_offer_files(&entries(json!([
            { "name": "Proyek", "dir": true },
            { "name": "Proyek/src", "dir": true },
            { "name": "Proyek\\src\\main.rs", "size": 7 },
        ])))
        .unwrap();
        let bytes = descriptor_bytes(&files);
        assert_eq!(bytes.len(), 4 + 3 * 592);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 3);
        let fd = |i: usize| &bytes[4 + i * 592..4 + (i + 1) * 592];
        let name = |i: usize| {
            let raw: Vec<u16> = fd(i)[72..72 + 520].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&raw[..raw.iter().position(|&c| c == 0).unwrap()])
        };
        let u32_at = |i: usize, off: usize| u32::from_le_bytes(fd(i)[off..off + 4].try_into().unwrap());
        for i in 0..2 {
            assert_eq!(u32_at(i, 0), FD_ATTRIBUTES | FD_PROGRESSUI);
            assert_eq!(u32_at(i, 36), FILE_ATTRIBUTE_DIRECTORY); // dwFileAttributes
        }
        assert_eq!(u32_at(2, 0), FD_FILESIZE | FD_PROGRESSUI);
        assert_eq!(u32_at(2, 68), 7); // size low
        assert_eq!((name(0), name(1), name(2)), ("Proyek".into(), r"Proyek\src".into(), r"Proyek\src\main.rs".into()));
    }

    #[test]
    fn descriptor_carries_all_three_times_and_safe_attributes_only() {
        // System (0x4) must not cross; read-only, hidden, archive do.
        let files = parse_offer_files(&entries(json!([
            { "name": "a.txt", "size": 1, "mtime": 1_704_067_200_000u64, "ctime": 1_600_000_000_000u64,
              "atime": 1_710_000_000_000u64, "attrs": 0x1 | 0x2 | 0x4 | 0x20 },
        ])))
        .unwrap();
        let fd = &descriptor_bytes(&files)[4..];
        let u32_at = |off: usize| u32::from_le_bytes(fd[off..off + 4].try_into().unwrap());
        let ft_at = |off: usize| u32_at(off) as u64 | (u32_at(off + 4) as u64) << 32;
        assert_eq!(
            u32_at(0),
            FD_ATTRIBUTES | FD_CREATETIME | FD_ACCESSTIME | FD_WRITETIME | FD_FILESIZE | FD_PROGRESSUI
        );
        assert_eq!(u32_at(36), 0x1 | 0x2 | 0x20);
        assert_eq!(ft_at(40), unix_ms_to_filetime(1_600_000_000_000).unwrap()); // ftCreationTime
        assert_eq!(ft_at(48), unix_ms_to_filetime(1_710_000_000_000).unwrap()); // ftLastAccessTime
        assert_eq!(ft_at(56), unix_ms_to_filetime(1_704_067_200_000).unwrap()); // ftLastWriteTime
    }

    #[test]
    fn offer_parsing_drops_unsafe_names_and_keeps_partner_indices() {
        let files = parse_offer_files(&entries(json!([
            { "name": "..\\x", "size": 1 },
            { "name": "C:\\Windows\\x", "size": 1 },
            { "name": "\\\\server\\share\\x", "size": 1 },
            { "name": "a\\NUL.txt", "size": 1 },
            { "name": "ok.txt", "size": 1 },
            { "name": "big.bin", "size": 3u64 << 30 },
            { "name": "d", "dir": true },
        ])))
        .unwrap();
        let kept: Vec<(&str, usize, bool)> = files.iter().map(|f| (f.name.as_str(), f.remote, f.dir)).collect();
        assert_eq!(kept, vec![("ok.txt", 4, false), ("d", 6, true)]);
        let too_many: Vec<OfferEntry> =
            (0..10_001).map(|i| OfferEntry { name: format!("f{i}"), size: Some(0), ..Default::default() }).collect();
        assert!(parse_offer_files(&too_many).is_err());
    }

    #[test]
    fn folder_entries_are_never_pulled() {
        let (tx_out, rx_out) = mpsc::channel();
        let offer = Offer {
            from: "111".into(),
            offer_id: "of_d".into(),
            files: vec![OfferFile {
                name: "d".into(),
                size: 0,
                dir: true,
                mtime: None,
                ctime: None,
                atime: None,
                attrs: 0,
                remote: 0,
            }],
            route: Route::Local(tx_out),
        };
        let obj =
            Box::into_raw(Box::new(DataObject { vtbl: &DATA_VTBL, refs: AtomicU32::new(1), offer: Arc::new(offer) }))
                as *mut c_void;
        let fmt = FORMATETC {
            cf_format: formats().contents,
            ptd: ptr::null_mut(),
            dw_aspect: DVASPECT_CONTENT,
            lindex: 0,
            tymed: TYMED_ISTREAM,
        };
        let mut medium = STGMEDIUM { tymed: 0, data: ptr::null_mut(), p_unk_for_release: ptr::null_mut() };
        unsafe {
            assert_eq!(data_get_data(obj, &fmt, &mut medium), DV_E_LINDEX);
            data_release(obj);
        }
        assert!(rx_out.try_recv().is_err(), "no pull may be sent for a folder");
    }

    #[test]
    fn pulled_bytes_flow_to_the_stream_and_are_verified() {
        let (tx_out, _rx_out) = mpsc::channel();
        let offer = Offer {
            from: "111".into(),
            offer_id: "of_1".into(),
            files: vec![OfferFile {
                name: "x.bin".into(),
                size: 4,
                dir: false,
                mtime: None,
                ctime: None,
                atime: None,
                attrs: 0,
                remote: 0,
            }],
            route: Route::Local(tx_out),
        };
        let stream = open_pull(&offer, 0);
        let tid = unsafe { (*(stream as *const PullStream)).tid.clone() };
        assert!(
            on_transfer("999", FileAction::Chunk, &tid, &payload(json!({}))).is_none(),
            "other peer must not feed the stream"
        );
        on_transfer("111", FileAction::Start, &tid, &payload(json!({ "size": 4 })));
        on_transfer(
            "111",
            FileAction::Chunk,
            &tid,
            &payload(json!({ "chunk": core_engine::codec::to_base64(b"abcd") })),
        );
        let sha: String = Sha256::digest(b"abcd").iter().map(|b| format!("{b:02x}")).collect();
        let (_, ack) = on_transfer("111", FileAction::End, &tid, &payload(json!({ "sha256": sha }))).unwrap();
        assert_eq!(ack, Some(true));
        let mut buf = [0u8; 8];
        let mut n = 0u32;
        unsafe {
            assert_eq!(stream_read(stream, buf.as_mut_ptr() as *mut c_void, 8, &mut n), S_OK);
            assert_eq!(&buf[..n as usize], b"abcd");
            stream_release(stream);
        }
    }

    #[test]
    fn binary_chunks_feed_the_stream_in_order_only() {
        let (tx_out, _rx_out) = mpsc::channel();
        let offer = Offer {
            from: "111".into(),
            offer_id: "of_2".into(),
            files: vec![OfferFile {
                name: "y.bin".into(),
                size: 6,
                dir: false,
                mtime: None,
                ctime: None,
                atime: None,
                attrs: 0,
                remote: 0,
            }],
            route: Route::Local(tx_out),
        };
        let stream = open_pull(&offer, 0);
        let tid = unsafe { (*(stream as *const PullStream)).tid.clone() };
        assert!(on_chunk("999", &tid, 0, b"abc").is_none(), "other peer must not feed the stream");
        assert!(on_chunk("111", "unknown", 0, b"abc").is_none(), "not a paste stream: normal receiver");
        on_transfer("111", FileAction::Start, &tid, &payload(json!({ "size": 6 })));
        assert_eq!(on_chunk("111", &tid, 0, b"abc").unwrap().1, None);
        on_transfer("111", FileAction::Chunk, &tid, &payload(json!({ "chunk": core_engine::codec::to_base64(b"d") })));
        assert_eq!(on_chunk("111", &tid, 4, b"ef").unwrap().1, None);
        let sha: String = Sha256::digest(b"abcdef").iter().map(|b| format!("{b:02x}")).collect();
        let (_, ack) = on_transfer("111", FileAction::End, &tid, &payload(json!({ "sha256": sha }))).unwrap();
        assert_eq!(ack, Some(true));
        let mut buf = [0u8; 8];
        let mut n = 0u32;
        unsafe {
            assert_eq!(stream_read(stream, buf.as_mut_ptr() as *mut c_void, 8, &mut n), S_OK);
            assert_eq!(&buf[..n as usize], b"abcdef");
            stream_release(stream);
        }

        // A gap (wrong offset) fails the paste.
        let stream = open_pull(&offer, 0);
        let tid = unsafe { (*(stream as *const PullStream)).tid.clone() };
        assert_eq!(on_chunk("111", &tid, 2, b"cd").unwrap().1, Some(false));
        assert!(on_chunk("111", &tid, 0, b"ab").is_none(), "failed stream is gone");
        unsafe { stream_release(stream) };
    }
}
