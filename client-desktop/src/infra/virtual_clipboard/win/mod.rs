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

mod agent;
mod data_object;
mod drag;
mod ole_thread;
mod sinks;
mod stream;
#[cfg(test)]
mod tests;

pub use agent::*;
use data_object::*;
use drag::*;
pub use ole_thread::*;
pub use sinks::*;
use stream::*;

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
    /// Small files already being pulled before Explorer asks for them, by entry index.
    prefetch: Mutex<HashMap<usize, Prefetched>>,
}

/// How many following small files are pulled ahead while Explorer pastes a list.
const PREFETCH_AHEAD: usize = 8;
/// Files up to this size are pulled ahead (they cost a round trip each, not bandwidth).
const PREFETCH_MAX_BYTES: u64 = 256 * 1024;

/// A pull started for an entry nobody has opened yet: its stream is built when Explorer asks for it.
struct Prefetched {
    tid: String,
    rx: Option<Receiver<SinkMsg>>,
}

impl Prefetched {
    /// Hands the stream over for use: nothing is forgotten when `self` is dropped afterwards.
    fn into_parts(mut self) -> (String, Receiver<SinkMsg>) {
        let rx = self.rx.take().expect("a prefetched stream is taken once");
        (std::mem::take(&mut self.tid), rx)
    }
}

impl Drop for Prefetched {
    /// Never opened (the offer was replaced): forget the stream so the bytes stop being kept.
    fn drop(&mut self) {
        if self.rx.is_none() {
            return;
        }
        with_sinks(|sinks| sinks.remove(&self.tid));
        if helper_streams(|m| m.remove(&self.tid)).is_some() {
            crate::infra::clipboard_helper::to_agent(&ToAgent::Close { tid: self.tid.clone() });
        }
    }
}
