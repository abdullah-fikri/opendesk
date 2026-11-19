//! IStream that blocks on the pull sink, with prefetch of the next files.

use super::*;

#[repr(C)]
pub(super) struct IStreamVtbl {
    pub(super) query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    pub(super) add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(super) release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(super) read: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut u32) -> HRESULT,
    pub(super) write: unsafe extern "system" fn(*mut c_void, *const c_void, u32, *mut u32) -> HRESULT,
    pub(super) seek: unsafe extern "system" fn(*mut c_void, i64, u32, *mut u64) -> HRESULT,
    pub(super) set_size: unsafe extern "system" fn(*mut c_void, u64) -> HRESULT,
    pub(super) copy_to: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, *mut u64, *mut u64) -> HRESULT,
    pub(super) commit: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    pub(super) revert: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub(super) lock_region: unsafe extern "system" fn(*mut c_void, u64, u64, u32) -> HRESULT,
    pub(super) unlock_region: unsafe extern "system" fn(*mut c_void, u64, u64, u32) -> HRESULT,
    pub(super) stat: unsafe extern "system" fn(*mut c_void, *mut STATSTG, u32) -> HRESULT,
    pub(super) clone: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

pub(super) struct StreamState {
    pub(super) rx: Receiver<SinkMsg>,
    pub(super) buf: Vec<u8>,
    pub(super) off: usize,
    pub(super) pos: u64,
    pub(super) done: bool,
    pub(super) failed: bool,
}

#[repr(C)]
pub(super) struct PullStream {
    pub(super) vtbl: *const IStreamVtbl,
    pub(super) refs: AtomicU32,
    pub(super) tid: String,
    pub(super) size: u64,
    pub(super) state: Mutex<StreamState>,
}

pub(super) static STREAM_VTBL: IStreamVtbl = IStreamVtbl {
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

pub(super) unsafe extern "system" fn stream_qi(this: *mut c_void, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
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

pub(super) unsafe extern "system" fn stream_add_ref(this: *mut c_void) -> u32 {
    unsafe { (*(this as *const PullStream)).refs.fetch_add(1, Ordering::SeqCst) + 1 }
}

pub(super) unsafe extern "system" fn stream_release(this: *mut c_void) -> u32 {
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

pub(super) unsafe extern "system" fn stream_read(
    this: *mut c_void,
    pv: *mut c_void,
    cb: u32,
    pcb_read: *mut u32,
) -> HRESULT {
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

pub(super) unsafe extern "system" fn stream_write(_: *mut c_void, _: *const c_void, _: u32, _: *mut u32) -> HRESULT {
    STG_E_INVALIDFUNCTION
}

pub(super) unsafe extern "system" fn stream_seek(
    this: *mut c_void,
    mv: i64,
    origin: u32,
    new_pos: *mut u64,
) -> HRESULT {
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

pub(super) unsafe extern "system" fn stream_u64_notimpl(_: *mut c_void, _: u64) -> HRESULT {
    E_NOTIMPL
}

pub(super) unsafe extern "system" fn stream_copy_to(
    _: *mut c_void,
    _: *mut c_void,
    _: u64,
    _: *mut u64,
    _: *mut u64,
) -> HRESULT {
    E_NOTIMPL
}

pub(super) unsafe extern "system" fn stream_commit(_: *mut c_void, _: u32) -> HRESULT {
    S_OK
}

pub(super) unsafe extern "system" fn stream_revert(_: *mut c_void) -> HRESULT {
    E_NOTIMPL
}

pub(super) unsafe extern "system" fn stream_region_notimpl(_: *mut c_void, _: u64, _: u64, _: u32) -> HRESULT {
    E_NOTIMPL
}

pub(super) unsafe extern "system" fn stream_stat(this: *mut c_void, out: *mut STATSTG, _flags: u32) -> HRESULT {
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

pub(super) unsafe extern "system" fn stream_clone(_: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        if !out.is_null() {
            *out = ptr::null_mut();
        }
        E_NOTIMPL
    }
}

/// Starts pulling entry `index` from the partner under a new transfer ID.
pub(super) fn start_pull(offer: &Offer, index: usize) -> Prefetched {
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
    Prefetched { tid, rx: Some(rx) }
}

/// Pulls the next few small files before Explorer asks for them: pasting a hundred small files otherwise costs
/// a round trip per file, one after the other.
pub(super) fn prefetch_after(offer: &Offer, index: usize) {
    let mut ahead = offer.prefetch.lock();
    let next = (index + 1..offer.files.len())
        .filter(|i| !offer.files[*i].dir && !ahead.contains_key(i))
        .take_while(|i| offer.files[*i].size <= PREFETCH_MAX_BYTES)
        .take(PREFETCH_AHEAD.saturating_sub(ahead.len()));
    for i in next.collect::<Vec<_>>() {
        ahead.insert(i, start_pull(offer, i));
    }
}

/// Returns an owned IStream (refcount 1) that yields the bytes of entry `index`, pulled on demand or ahead.
pub(super) fn open_pull(offer: &Offer, index: usize) -> *mut c_void {
    let size = offer.files[index].size;
    let pulled = offer.prefetch.lock().remove(&index).unwrap_or_else(|| start_pull(offer, index));
    let (tid, rx) = pulled.into_parts();
    prefetch_after(offer, index);
    let stream = Box::new(PullStream {
        vtbl: &STREAM_VTBL,
        refs: AtomicU32::new(1),
        tid,
        size,
        state: Mutex::new(StreamState { rx, buf: Vec::new(), off: 0, pos: 0, done: false, failed: false }),
    });
    Box::into_raw(stream) as *mut c_void
}
