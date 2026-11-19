//! IDataObject exposing the offer as virtual files (FileGroupDescriptorW + FileContents).

use super::*;

#[repr(C)]
pub(super) struct IDataObjectVtbl {
    pub(super) query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    pub(super) add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(super) release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(super) get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> HRESULT,
    pub(super) get_data_here: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> HRESULT,
    pub(super) query_get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC) -> HRESULT,
    pub(super) get_canonical_format_etc:
        unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut FORMATETC) -> HRESULT,
    pub(super) set_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM, i32) -> HRESULT,
    pub(super) enum_format_etc: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
    pub(super) d_advise:
        unsafe extern "system" fn(*mut c_void, *const FORMATETC, u32, *mut c_void, *mut u32) -> HRESULT,
    pub(super) d_unadvise: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    pub(super) enum_d_advise: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
pub(super) struct DataObject {
    pub(super) vtbl: *const IDataObjectVtbl,
    pub(super) refs: AtomicU32,
    pub(super) offer: Arc<Offer>,
}

pub(super) static DATA_VTBL: IDataObjectVtbl = IDataObjectVtbl {
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

pub(super) fn offered_formats() -> [FORMATETC; 3] {
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
pub(super) fn check_format(fmt: &FORMATETC) -> Result<u16, HRESULT> {
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

pub(super) fn global_from_bytes(bytes: &[u8]) -> *mut c_void {
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
pub(super) fn unix_ms_to_filetime(ms: u64) -> Option<u64> {
    ms.checked_mul(10_000)?.checked_add(116_444_736_000_000_000)
}

pub(super) fn descriptor_bytes(files: &[OfferFile]) -> Vec<u8> {
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

pub(super) unsafe extern "system" fn data_qi(this: *mut c_void, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
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

pub(super) unsafe extern "system" fn data_add_ref(this: *mut c_void) -> u32 {
    unsafe { (*(this as *const DataObject)).refs.fetch_add(1, Ordering::SeqCst) + 1 }
}

pub(super) unsafe extern "system" fn data_release(this: *mut c_void) -> u32 {
    unsafe {
        let left = (*(this as *const DataObject)).refs.fetch_sub(1, Ordering::SeqCst) - 1;
        if left == 0 {
            drop(Box::from_raw(this as *mut DataObject));
        }
        left
    }
}

pub(super) unsafe extern "system" fn data_get_data(
    this: *mut c_void,
    fmt: *const FORMATETC,
    medium: *mut STGMEDIUM,
) -> HRESULT {
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

pub(super) unsafe extern "system" fn data_get_data_here(
    _: *mut c_void,
    _: *const FORMATETC,
    _: *mut STGMEDIUM,
) -> HRESULT {
    E_NOTIMPL
}

pub(super) unsafe extern "system" fn data_query_get_data(_: *mut c_void, fmt: *const FORMATETC) -> HRESULT {
    if fmt.is_null() {
        return DV_E_FORMATETC;
    }
    match check_format(unsafe { &*fmt }) {
        Ok(_) => S_OK,
        Err(hr) => hr,
    }
}

pub(super) unsafe extern "system" fn data_canonical(
    _: *mut c_void,
    _: *const FORMATETC,
    out: *mut FORMATETC,
) -> HRESULT {
    if !out.is_null() {
        unsafe { (*out).ptd = ptr::null_mut() };
    }
    E_NOTIMPL
}

/// `CLSID_RecycleBin` {645FF040-5081-101B-9F08-00AA002F954E}: a drop target that only deletes.
pub(super) const CLSID_RECYCLE_BIN: [u8; 16] =
    [0x40, 0xF0, 0x5F, 0x64, 0x81, 0x50, 0x1B, 0x10, 0x9F, 0x08, 0x00, 0xAA, 0x00, 0x2F, 0x95, 0x4E];

/// Bytes of an HGLOBAL medium (at most `max`).
pub(super) fn global_bytes(h: *mut c_void, max: usize) -> Vec<u8> {
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
pub(super) unsafe extern "system" fn data_set_data(
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

pub(super) unsafe extern "system" fn data_enum_format_etc(
    _: *mut c_void,
    direction: u32,
    out: *mut *mut c_void,
) -> HRESULT {
    if direction != DATADIR_GET || out.is_null() {
        return E_NOTIMPL;
    }
    let fmts = offered_formats();
    unsafe { SHCreateStdEnumFmtEtc(fmts.len() as u32, fmts.as_ptr(), out) }
}

pub(super) unsafe extern "system" fn data_d_advise(
    _: *mut c_void,
    _: *const FORMATETC,
    _: u32,
    _: *mut c_void,
    _: *mut u32,
) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

pub(super) unsafe extern "system" fn data_d_unadvise(_: *mut c_void, _: u32) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

pub(super) unsafe extern "system" fn data_enum_d_advise(_: *mut c_void, _: *mut *mut c_void) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}
