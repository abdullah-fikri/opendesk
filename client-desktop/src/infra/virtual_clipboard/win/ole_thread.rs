//! OLE clipboard thread (STA with a message loop, as OleSetClipboard requires) and placing offers.

use super::*;

pub(super) enum Cmd {
    Set(Offer),
    Drop(Offer),
    Clear(String),
}

pub(super) struct OleThread {
    pub(super) tx: Sender<Cmd>,
    pub(super) tid: u32,
}

pub(super) fn ole_thread() -> &'static OleThread {
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

pub(super) fn post(cmd: Cmd) {
    let t = ole_thread();
    let _ = t.tx.send(cmd);
    unsafe {
        PostThreadMessageW(t.tid, WM_APP, 0, 0);
    }
}

/// Validates the partner's file list. Entries with unsafe names (absolute, `..`, devices, ...) are dropped;
/// a list over the entry or size limit is refused with a status line.
pub(super) fn parse_offer_files(list: &[OfferEntry]) -> Result<Vec<OfferFile>, String> {
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

pub(super) fn place_offer(from: &str, payload: &FilePayload, tx_out: Sender<Outgoing>, drop: bool) -> Option<String> {
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
        offers.push(Offer {
            from: from.to_string(),
            offer_id,
            files,
            route: Route::Local(tx_out),
            prefetch: Default::default(),
        });
        let excess = offers.len().saturating_sub(KEPT_AGENT_OFFERS);
        offers.drain(..excess);
        std::mem::drop(offers);
        if !drop {
            *LAST_SET.lock() =
                Some((from.to_string(), payload.id.clone(), payload.files.clone()?, std::time::Instant::now()));
        }
        if crate::infra::clipboard_helper::send(&msg) {
            return Some(status);
        }
        logger::warn("clipbrd.capture", "Clipboard helper unavailable; offer not placed.");
        return None;
    }
    let offer =
        Offer { from: from.to_string(), offer_id, files, route: Route::Local(tx_out), prefetch: Default::default() };
    post(if drop { Cmd::Drop(offer) } else { Cmd::Set(offer) });
    Some(if drop { "Dropping files...".to_string() } else { status })
}

/// Session with `partner` ended (empty = any): its offers can no longer be pulled.
pub fn clear_offer(partner: &str) {
    post(Cmd::Clear(partner.to_string()));
    AGENT_OFFERS.lock().retain(|o| !partner.is_empty() && o.from != partner);
    LAST_SET.lock().take_if(|(from, _, _, _)| partner.is_empty() || from == partner);
    crate::infra::clipboard_helper::send(&ToHelper::Clear { partner: partner.to_string() });
}
