//! Drag onto the pointer (`file-drop`): an invisible source window starts an OLE drag at the cursor.

use super::*;

pub(super) const DRAGDROP_S_DROP: HRESULT = 0x0004_0100;
pub(super) const DRAGDROP_S_CANCEL: HRESULT = 0x0004_0101;
pub(super) const DRAGDROP_S_USEDEFAULTCURSORS: HRESULT = 0x0004_0102;
/// How long the drag hovers over the target before the drop, so it sees `DragEnter` and `DragOver` first.
pub(super) const DROP_HOVER: std::time::Duration = std::time::Duration::from_millis(400);

#[repr(C)]
pub(super) struct IDropSourceVtbl {
    pub(super) query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    pub(super) add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(super) release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(super) query_continue_drag: unsafe extern "system" fn(*mut c_void, i32, u32) -> HRESULT,
    pub(super) give_feedback: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
}

#[repr(C)]
pub(super) struct DropSource {
    pub(super) vtbl: *const IDropSourceVtbl,
    pub(super) started: std::time::Instant,
    pub(super) released: std::sync::atomic::AtomicBool,
}

pub(super) static DROP_SOURCE_VTBL: IDropSourceVtbl = IDropSourceVtbl {
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
    pub(super) fn GetCursorPos(pt: *mut [i32; 2]) -> i32;
    pub(super) fn SetCursorPos(x: i32, y: i32) -> i32;
    pub(super) fn mouse_event(flags: u32, dx: i32, dy: i32, data: u32, extra: usize);
}

pub(super) const MK_LBUTTON: u32 = 1;
pub(super) const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
pub(super) const MOUSEEVENTF_LEFTUP: u32 = 0x0004;

pub(super) unsafe extern "system" fn source_qi(
    this: *mut c_void,
    _riid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    // Only IUnknown and IDropSource are ever asked for, and both are this object.
    unsafe { *out = this };
    S_OK
}

/// The source lives on the stack of `drag_onto_pointer`; OLE's references never outlive `DoDragDrop`.
pub(super) unsafe extern "system" fn source_ref(_: *mut c_void) -> u32 {
    1
}

pub(super) unsafe extern "system" fn source_continue(this: *mut c_void, escape: i32, keys: u32) -> HRESULT {
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

pub(super) unsafe extern "system" fn source_feedback(_: *mut c_void, _effect: u32) -> HRESULT {
    DRAGDROP_S_USEDEFAULTCURSORS
}

/// What the catcher window's button press starts: the data object to drag and where the pointer is.
pub(super) static DRAG_JOB: Mutex<Option<(usize, [i32; 2])>> = Mutex::new(None);
/// Verdict of the last drag; `None` while it runs.
pub(super) static DRAG_RESULT: Mutex<Option<HRESULT>> = Mutex::new(None);

#[repr(C)]
pub(super) struct WNDCLASSW {
    pub(super) style: u32,
    pub(super) wnd_proc: unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize,
    pub(super) cls_extra: i32,
    pub(super) wnd_extra: i32,
    pub(super) instance: *mut c_void,
    pub(super) icon: *mut c_void,
    pub(super) cursor: *mut c_void,
    pub(super) background: *mut c_void,
    pub(super) menu_name: *const u16,
    pub(super) class_name: *const u16,
}

#[link(name = "user32")]
unsafe extern "system" {
    pub(super) fn RegisterClassW(class: *const WNDCLASSW) -> u16;
    pub(super) fn CreateWindowExW(
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
    pub(super) fn DefWindowProcW(hwnd: *mut c_void, msg: u32, w: usize, l: isize) -> isize;
    pub(super) fn DestroyWindow(hwnd: *mut c_void) -> i32;
    pub(super) fn SetWindowPos(
        hwnd: *mut c_void,
        after: *mut c_void,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        flags: u32,
    ) -> i32;
    pub(super) fn ReleaseCapture() -> i32;
}

pub(super) const WM_LBUTTONDOWN: u32 = 0x0201;
pub(super) const WS_POPUP: u32 = 0x8000_0000;
pub(super) const WS_VISIBLE: u32 = 0x1000_0000;
pub(super) const WS_EX_TOPMOST: u32 = 0x8;
pub(super) const WS_EX_TOOLWINDOW: u32 = 0x80;
pub(super) const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
pub(super) const SWP_NOSIZE: u32 = 0x1;
pub(super) const SWP_NOACTIVATE: u32 = 0x10;

/// Window under the pointer for one drag. A drag can only start from a button press inside a window of this
/// thread (Windows gives that window the mouse), so the press lands here, not in the application below, which
/// therefore never sees a click. The window then moves away and the drag runs from its message.
pub(super) unsafe extern "system" fn catcher_proc(hwnd: *mut c_void, msg: u32, w: usize, l: isize) -> isize {
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
pub(super) fn drag_onto_pointer(offer: Offer) {
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
