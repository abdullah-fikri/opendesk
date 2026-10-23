//! AnyDesk-style clipboard file transfer: the partner's copied files are put on the local clipboard as
//! virtual files (FileGroupDescriptorW + FileContents). Nothing is transferred until an application pastes;
//! then each FileContents request pulls that file and streams it straight into the pasting application,
//! so Explorer shows its own copy progress. Hand-written COM objects, same raw-FFI style as the rest of the app.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use core_engine::codec::from_base64;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::logger;
use crate::protocol::OutgoingMsg;

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
const FD_FILESIZE: u32 = 0x0040;
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
const IID_ISEQUENTIALSTREAM: GUID = GUID {
    data1: 0x0c73_3a30,
    data2: 0x2a1c,
    data3: 0x11ce,
    data4: [0xad, 0xe5, 0x00, 0xaa, 0x00, 0x44, 0x77, 0x3d],
};

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
    fn OleIsCurrentClipboard(p_data_obj: *mut c_void) -> HRESULT;
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
        }
    })
}

/// One file list offered by the partner.
struct Offer {
    from: String,
    offer_id: String,
    files: Vec<(String, u64)>,
    tx_out: Sender<OutgoingMsg>,
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
    tx: Sender<SinkMsg>,
}

static SINKS: Mutex<Option<HashMap<String, Sink>>> = Mutex::new(None);

fn with_sinks<R>(f: impl FnOnce(&mut HashMap<String, Sink>) -> R) -> R {
    f(SINKS.lock().unwrap().get_or_insert_with(HashMap::new))
}

/// Handles a `file-transfer` payload if it belongs to one of our pulls. Returns `(status, ack)` like
/// `FileReceiver::handle_payload`, or None when the transfer is not ours.
pub fn on_transfer(from: &str, action: &str, id: &str, payload: &Value) -> Option<(Option<String>, Option<bool>)> {
    with_sinks(|sinks| {
        if sinks.get(id).is_none_or(|s| s.from != from) {
            return None;
        }
        Some(match action {
            "start" => {
                let size = payload.get("size").and_then(|v| v.as_u64());
                if size == Some(sinks[id].size) {
                    (None, None)
                } else {
                    let sink = sinks.remove(id).unwrap();
                    let _ = sink.tx.send(SinkMsg::End(false));
                    (Some(format!("Gagal menempel {}: ukuran berubah", sink.name)), Some(false))
                }
            }
            "chunk" => {
                let sink = sinks.get_mut(id).unwrap();
                let bytes = payload
                    .get("chunk")
                    .and_then(|v| v.as_str())
                    .filter(|b64| b64.len() <= 48_000)
                    .and_then(from_base64)
                    .filter(|b| sink.received + b.len() as u64 <= sink.size);
                match bytes {
                    Some(bytes) => {
                        sink.hash.update(&bytes);
                        sink.received += bytes.len() as u64;
                        let pct = if sink.size > 0 { (sink.received * 100 / sink.size).min(99) } else { 99 };
                        let status = format!("Menempel {} ({}%)...", sink.name, pct);
                        // A closed receiver means the paste was cancelled; the remaining bytes are dropped.
                        let _ = sink.tx.send(SinkMsg::Data(bytes));
                        (Some(status), None)
                    }
                    None => {
                        let sink = sinks.remove(id).unwrap();
                        let _ = sink.tx.send(SinkMsg::End(false));
                        (Some(format!("Gagal menempel {}", sink.name)), Some(false))
                    }
                }
            }
            "end" => {
                let sink = sinks.remove(id).unwrap();
                let digest: String = sink.hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
                let ok = sink.received == sink.size
                    && payload.get("sha256").and_then(|v| v.as_str()) == Some(digest.as_str());
                let _ = sink.tx.send(SinkMsg::End(ok));
                let status = if ok {
                    format!("Selesai ditempel: {}", sink.name)
                } else {
                    format!("Gagal menempel {}: ukuran atau SHA-256 tidak cocok", sink.name)
                };
                (Some(status), Some(ok))
            }
            _ => (None, None),
        })
    })
}

/// Fails every paste in progress (relay connection lost).
pub fn abort_pulls() {
    with_sinks(|sinks| {
        for (_, sink) in sinks.drain() {
            let _ = sink.tx.send(SinkMsg::End(false));
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
        }
        left
    }
}

unsafe extern "system" fn stream_read(this: *mut c_void, pv: *mut c_void, cb: u32, pcb_read: *mut u32) -> HRESULT {
    unsafe {
        let stream = &*(this as *const PullStream);
        let mut st = stream.state.lock().unwrap();
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
        let pos = stream.state.lock().unwrap().pos;
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
    let (name, size) = offer.files[index].clone();
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros();
    let tid = format!("fp_{}_{}", ts, index);
    let (tx, rx) = mpsc::channel();
    with_sinks(|sinks| {
        sinks.insert(
            tid.clone(),
            Sink { from: offer.from.clone(), name: name.clone(), size, received: 0, hash: Sha256::new(), tx },
        )
    });
    logger::info("app.ft_sink_session", &format!("Pasting {} from offer {} ({}).", name, offer.offer_id, tid));
    let _ = offer.tx_out.send(OutgoingMsg::Json(json!({
        "type": "relay-file",
        "targetClean": offer.from,
        "payload": { "action": "pull", "id": offer.offer_id, "index": index, "tid": tid }
    })));
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
    let fe = |cf: u16, tymed: u32| FORMATETC { cf_format: cf, ptd: ptr::null_mut(), dw_aspect: DVASPECT_CONTENT, lindex: -1, tymed };
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

fn descriptor_bytes(files: &[(String, u64)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + files.len() * std::mem::size_of::<FILEDESCRIPTORW>());
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for (name, size) in files {
        let mut fd: FILEDESCRIPTORW = unsafe { std::mem::zeroed() };
        fd.dw_flags = FD_FILESIZE | FD_PROGRESSUI;
        fd.n_file_size_high = (*size >> 32) as u32;
        fd.n_file_size_low = *size as u32;
        // Names come from the partner: keep only a plain file name Explorer can create.
        let clean: Vec<u16> = name
            .chars()
            .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
            .collect::<String>()
            .encode_utf16()
            .take(259)
            .collect();
        fd.c_file_name[..clean.len()].copy_from_slice(&clean);
        let raw = unsafe {
            std::slice::from_raw_parts(&fd as *const FILEDESCRIPTORW as *const u8, std::mem::size_of::<FILEDESCRIPTORW>())
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
            if index >= obj.offer.files.len() {
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

unsafe extern "system" fn data_set_data(_: *mut c_void, _: *const FORMATETC, _: *mut STGMEDIUM, _: i32) -> HRESULT {
    // Explorer reports "Paste Succeeded" etc. here; refusing leaves the medium with the caller.
    E_NOTIMPL
}

unsafe extern "system" fn data_enum_format_etc(_: *mut c_void, direction: u32, out: *mut *mut c_void) -> HRESULT {
    if direction != DATADIR_GET || out.is_null() {
        return E_NOTIMPL;
    }
    let fmts = offered_formats();
    unsafe { SHCreateStdEnumFmtEtc(fmts.len() as u32, fmts.as_ptr(), out) }
}

unsafe extern "system" fn data_d_advise(_: *mut c_void, _: *const FORMATETC, _: u32, _: *mut c_void, _: *mut u32) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn data_d_unadvise(_: *mut c_void, _: u32) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn data_enum_d_advise(_: *mut c_void, _: *mut *mut c_void) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

// ---------------------------------------------------------------------------------------------
// OLE clipboard thread (STA with a message loop, as OleSetClipboard requires).

enum Cmd {
    Set(Offer),
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
        thread::spawn(move || unsafe {
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
                            Cmd::Clear(partner) => {
                                if let Some((obj, _)) = current.take_if(|(_, from)| *from == partner || partner.is_empty()) {
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

/// Partner copied files: expose them on our clipboard as virtual files. Nothing is downloaded yet.
pub fn set_offer(from: &str, payload: &Value, tx_out: Sender<OutgoingMsg>) -> Option<String> {
    let offer_id = payload.get("id").and_then(|v| v.as_str())?.to_string();
    let files: Vec<(String, u64)> = payload
        .get("files")
        .and_then(|v| v.as_array())?
        .iter()
        .filter_map(|f| {
            let name = f.get("name")?.as_str()?;
            let size = f.get("size")?.as_u64()?;
            (!name.is_empty() && name.len() <= 200 && size <= 2 * 1024 * 1024 * 1024).then(|| (name.to_string(), size))
        })
        .take(100)
        .collect();
    if files.is_empty() {
        return None;
    }
    let status = if files.len() == 1 {
        format!("Siap tempel: {} (Ctrl+V)", files[0].0)
    } else {
        format!("Siap tempel: {} berkas (Ctrl+V)", files.len())
    };
    logger::info("clipbrd.capture", &format!("Received file offer {} ({} files).", offer_id, files.len()));
    post(Cmd::Set(Offer { from: from.to_string(), offer_id, files, tx_out }));
    Some(status)
}

/// Session with `partner` ended (empty = any): its offers can no longer be pulled.
pub fn clear_offer(partner: &str) {
    post(Cmd::Clear(partner.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_layout_matches_windows() {
        assert_eq!(std::mem::size_of::<FILEDESCRIPTORW>(), 592);
        assert_eq!(std::mem::size_of::<STATSTG>(), 80);
        let bytes = descriptor_bytes(&[("a:b.txt".into(), 5_000_000_000)]);
        assert_eq!(bytes.len(), 4 + 592);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 1);
        let fd = &bytes[4..];
        assert_eq!(u32::from_le_bytes(fd[0..4].try_into().unwrap()), FD_FILESIZE | FD_PROGRESSUI);
        assert_eq!(u32::from_le_bytes(fd[64..68].try_into().unwrap()), 1); // size high
        let name: Vec<u16> = fd[72..86].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16_lossy(&name), "a_b.txt");
    }

    /// Real Explorer paste across processes. Overwrites the clipboard, so run it by hand:
    /// `cargo test --release explorer_paste -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn explorer_paste_pulls_virtual_file() {
        let dir = std::env::temp_dir().join(format!("opendesk-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let (tx_out, rx_out) = mpsc::channel();
        let offer = json!({ "id": "of_t", "files": [{ "name": "pasted.bin", "size": data.len() }] });
        assert!(set_offer("111", &offer, tx_out).is_some());
        thread::sleep(Duration::from_millis(300));

        // A fresh process must hold the OLE clipboard object (as Explorer always does for its Paste menu state)
        // before pasting; otherwise Shell reads through Win32 clipboard emulation, which has no per-file lindex.
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $null=[System.Windows.Forms.Clipboard]::GetDataObject().GetFormats(); $f=(New-Object -ComObject Shell.Application).NameSpace('{}'); $f.Self.InvokeVerb('Paste'); Start-Sleep -Seconds 8",
            dir.display()
        );
        // OPENDESK_MANUAL_PASTE=1: paste by hand (Ctrl+V in any Explorer folder) instead of the scripted paste.
        let manual = std::env::var("OPENDESK_MANUAL_PASTE").is_ok();
        let mut ps = if manual {
            println!("Clipboard now holds virtual file pasted.bin: press Ctrl+V in an Explorer folder within 60 s.");
            None
        } else {
            Some(std::process::Command::new("powershell").args(["-NoProfile", "-STA", "-Command", &script]).spawn().unwrap())
        };

        // Play the partner: answer the pull with start/chunk/end.
        let OutgoingMsg::Json(pull) = rx_out.recv_timeout(Duration::from_secs(if manual { 60 } else { 40 })).expect("Explorer never pulled") else {
            panic!("unexpected message")
        };
        let tid = pull["payload"]["tid"].as_str().unwrap().to_string();
        assert_eq!(pull["payload"]["index"], 0);
        on_transfer("111", "start", &tid, &json!({ "size": data.len() }));
        for chunk in data.chunks(32768) {
            on_transfer("111", "chunk", &tid, &json!({ "chunk": core_engine::codec::to_base64(chunk) }));
        }
        let sha: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(on_transfer("111", "end", &tid, &json!({ "sha256": sha })).unwrap().1, Some(true));

        if let Some(ps) = ps.as_mut() {
            let _ = ps.wait();
            let pasted = std::fs::read(dir.join("pasted.bin")).expect("Explorer did not create the file");
            assert_eq!(pasted, data);
        } else {
            thread::sleep(Duration::from_secs(3)); // let Explorer finish writing; check the pasted file by hand
        }
        clear_offer("");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pulled_bytes_flow_to_the_stream_and_are_verified() {
        let (tx_out, _rx_out) = mpsc::channel();
        let offer = Offer { from: "111".into(), offer_id: "of_1".into(), files: vec![("x.bin".into(), 4)], tx_out };
        let stream = open_pull(&offer, 0);
        let tid = unsafe { (*(stream as *const PullStream)).tid.clone() };
        assert!(on_transfer("999", "chunk", &tid, &json!({})).is_none(), "other peer must not feed the stream");
        on_transfer("111", "start", &tid, &json!({ "size": 4 }));
        on_transfer("111", "chunk", &tid, &json!({ "chunk": core_engine::codec::to_base64(b"abcd") }));
        let sha: String = Sha256::digest(b"abcd").iter().map(|b| format!("{b:02x}")).collect();
        let (_, ack) = on_transfer("111", "end", &tid, &json!({ "sha256": sha })).unwrap();
        assert_eq!(ack, Some(true));
        let mut buf = [0u8; 8];
        let mut n = 0u32;
        unsafe {
            assert_eq!(stream_read(stream, buf.as_mut_ptr() as *mut c_void, 8, &mut n), S_OK);
            assert_eq!(&buf[..n as usize], b"abcd");
            stream_release(stream);
        }
    }
}
