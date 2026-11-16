//! EcoQoS while idle (Windows): with no session and no traffic the process asks Windows for power throttling
//! (efficiency cores, lower clocks); anything that needs speed holds an [`Awake`] guard, and throttling returns
//! only when the last guard is dropped. It applies to the calling process only, so the SYSTEM service and the
//! agent are independent; it is armed by [`arm`] (the relay connection), so other processes never throttle.
//! `OPENDESK_ECOQOS=0` disables it, thread priority included. A no-op on other platforms.

use std::sync::OnceLock;

use parking_lot::Mutex;

use crate::infra::logger;

struct State {
    /// Live [`Awake`] guards.
    awake: usize,
    /// Throttling may be requested (the relay connection runs in this process).
    armed: bool,
    throttled: bool,
}

static STATE: Mutex<State> = Mutex::new(State { awake: 0, armed: false, throttled: false });

/// Keeps the process out of power throttling while alive.
pub struct Awake(());

/// Holds the process at full speed until the guard is dropped (sessions overlap: a counter).
pub fn awake() -> Awake {
    let mut s = STATE.lock();
    s.awake += 1;
    apply(&mut s);
    Awake(())
}

impl Drop for Awake {
    fn drop(&mut self) {
        let mut s = STATE.lock();
        s.awake -= 1;
        apply(&mut s);
    }
}

/// Enables throttling for this process; it starts at once if nothing holds it awake.
pub fn arm() {
    let mut s = STATE.lock();
    s.armed = true;
    apply(&mut s);
}

/// Raises the calling thread above normal priority (the host capture loop).
pub fn raise_thread_priority() {
    if enabled() {
        sys::raise_thread_priority();
    }
}

/// Multimedia Class Scheduler task a latency-critical thread joins.
#[derive(Clone, Copy)]
pub enum Mmcss {
    /// Capture, encode, decode and the P2P path: frames and input (MMCSS "Games": high category, ahead of
    /// ordinary threads under load; Windows keeps 20% of the CPU for everything else).
    Games,
    /// Microphone, speaker and host sound loops ("Audio"). Only the Windows audio loops join it.
    #[cfg_attr(not(windows), allow(dead_code))]
    Audio,
}

/// The calling thread's MMCSS registration; dropping it (on the same thread) leaves the class.
pub struct MmcssGuard(sys::MmcssHandle);

impl Drop for MmcssGuard {
    fn drop(&mut self) {
        sys::mmcss_leave(&self.0);
    }
}

/// Registers the calling thread with MMCSS for `task` until the guard drops; without MMCSS (the service
/// stopped, `OPENDESK_ECOQOS=0`, not Windows) the thread keeps its priority.
// Off Windows the handle is a unit struct.
#[allow(clippy::default_constructed_unit_structs)]
pub fn mmcss(task: Mmcss) -> MmcssGuard {
    let name = match task {
        Mmcss::Games => "Games",
        Mmcss::Audio => "Audio",
    };
    MmcssGuard(if enabled() { sys::mmcss_join(name) } else { sys::MmcssHandle::default() })
}

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("OPENDESK_ECOQOS").as_deref() != Ok("0"))
}

fn apply(s: &mut State) {
    let want = s.armed && s.awake == 0;
    if want == s.throttled || !enabled() {
        return;
    }
    if sys::set_throttled(want) {
        s.throttled = want;
        logger::info("power", if want { "Idle: EcoQoS power throttling on." } else { "Busy: EcoQoS off." });
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;

    const PROCESS_POWER_THROTTLING: i32 = 4;
    const PROCESS_POWER_THROTTLING_CURRENT_VERSION: u32 = 1;
    const PROCESS_POWER_THROTTLING_EXECUTION_SPEED: u32 = 0x1;
    const THREAD_PRIORITY_ABOVE_NORMAL: i32 = 1;

    #[repr(C)]
    struct PowerThrottlingState {
        version: u32,
        control_mask: u32,
        state_mask: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetCurrentThread() -> *mut c_void;
        fn SetProcessInformation(process: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
        fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
    }

    /// Returns false when Windows refused (older than Windows 10 1709).
    pub fn set_throttled(on: bool) -> bool {
        let state = PowerThrottlingState {
            version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            control_mask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            state_mask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
        };
        // SAFETY: the pseudo handle needs no closing; the struct is valid for the call and its size is exact.
        unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                PROCESS_POWER_THROTTLING,
                (&state as *const PowerThrottlingState).cast(),
                size_of::<PowerThrottlingState>() as u32,
            ) != 0
        }
    }

    pub fn raise_thread_priority() {
        // SAFETY: the pseudo handle names the calling thread and needs no closing.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };
    }

    #[link(name = "avrt")]
    unsafe extern "system" {
        fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> *mut c_void;
        fn AvRevertMmThreadCharacteristics(handle: *mut c_void) -> i32;
    }

    /// Null when the thread is not registered.
    pub struct MmcssHandle(pub *mut c_void);

    impl Default for MmcssHandle {
        fn default() -> Self {
            Self(std::ptr::null_mut())
        }
    }

    pub fn mmcss_join(task: &str) -> MmcssHandle {
        let name: Vec<u16> = task.encode_utf16().chain(std::iter::once(0)).collect();
        let mut index = 0u32;
        // SAFETY: a NUL-terminated task name and an out-parameter, both valid for the call.
        MmcssHandle(unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) })
    }

    pub fn mmcss_leave(handle: &MmcssHandle) {
        if !handle.0.is_null() {
            // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on this thread (the guard is !Send).
            unsafe { AvRevertMmThreadCharacteristics(handle.0) };
        }
    }
}

#[cfg(not(windows))]
mod sys {
    pub fn set_throttled(_on: bool) -> bool {
        false
    }

    pub fn raise_thread_priority() {}

    #[derive(Default)]
    pub struct MmcssHandle;

    pub fn mmcss_join(_task: &str) -> MmcssHandle {
        MmcssHandle
    }

    pub fn mmcss_leave(_handle: &MmcssHandle) {}
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn threads_join_and_leave_mmcss() {
        let guard = super::mmcss(super::Mmcss::Games);
        assert!(!guard.0.0.is_null(), "MMCSS refused the Games task");
        drop(guard);
    }

    #[test]
    fn windows_accepts_the_throttling_request() {
        assert!(super::sys::set_throttled(true));
        assert!(super::sys::set_throttled(false));
    }
}
