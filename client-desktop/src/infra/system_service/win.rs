//! `OpenDeskService` (LocalSystem, installed by the setup) keeps one OpenDesk process, the agent, running as
//! SYSTEM in the active console session. Only SYSTEM may open the "Winlogon" desktop where the UAC prompt and
//! the lock screen live; the capture and input threads follow it there (`core_engine::desktop`).
//!
//! The agent is the normal app (window, tray) started with the logged-on user's environment block, so
//! `%APPDATA%`, `%USERPROFILE%` and therefore settings, history and received files stay in that user's
//! profile. The service:
//! - starts the agent when a user is logged on to the console, and again when it exits,
//! - stops it when the console session changes (fast user switching) and when the service stops,
//! - leaves it stopped while `user_exit.flag` exists (tray "Stop & Exit", `opendesk stop`), until the next
//!   launch from the Start Menu removes the flag.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::{Duration, Instant};

use crate::infra::autostart::STOP_FLAG;
use crate::infra::logger;

type HANDLE = *mut c_void;
type BOOL = i32;

/// Must match `ServiceName` in installer/opendesk.iss.
pub const SERVICE_NAME: &str = "OpenDeskService";

const SERVICE_WIN32_OWN_PROCESS: u32 = 0x10;
const SERVICE_STOPPED: u32 = 1;
const SERVICE_STOP_PENDING: u32 = 3;
const SERVICE_RUNNING: u32 = 4;
const SERVICE_ACCEPT_STOP: u32 = 0x1;
const SERVICE_ACCEPT_SHUTDOWN: u32 = 0x4;
const SERVICE_CONTROL_STOP: u32 = 1;
const SERVICE_CONTROL_INTERROGATE: u32 = 4;
const SERVICE_CONTROL_SHUTDOWN: u32 = 5;
const NO_ERROR: u32 = 0;
const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
const SC_MANAGER_CONNECT: u32 = 0x1;
const SERVICE_QUERY_STATUS: u32 = 0x4;

const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_DUPLICATE: u32 = 0x0002;
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
const SECURITY_IMPERSONATION: u32 = 2;
const TOKEN_PRIMARY: u32 = 1;
const TOKEN_USER_CLASS: u32 = 1; // TokenUser
const TOKEN_SESSION_ID_CLASS: u32 = 12; // TokenSessionId
const WIN_LOCAL_SYSTEM_SID: u32 = 22;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const INFINITE: u32 = 0xFFFF_FFFF;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;
/// `WTSGetActiveConsoleSessionId` while the console is being attached or detached.
const NO_SESSION: u32 = 0xFFFF_FFFF;

/// How often the service checks the console session and the agent.
const POLL_MS: u32 = 2000;
/// An agent that exits sooner than this (e.g. a copy started without the service already runs in the
/// session) is started again only after [`RETRY_AFTER`], not on every poll.
const MIN_UPTIME: Duration = Duration::from_secs(10);
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Wait after a user logs on (or the service first sees one) before starting the agent.
const LOGON_GRACE: Duration = Duration::from_secs(10);

#[repr(C)]
struct SERVICE_STATUS {
    dw_service_type: u32,
    dw_current_state: u32,
    dw_controls_accepted: u32,
    dw_win32_exit_code: u32,
    dw_service_specific_exit_code: u32,
    dw_check_point: u32,
    dw_wait_hint: u32,
}

#[repr(C)]
struct SERVICE_TABLE_ENTRYW {
    lp_service_name: *mut u16,
    lp_service_proc: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
}

#[repr(C)]
struct STARTUPINFOW {
    cb: u32,
    lp_reserved: *mut u16,
    lp_desktop: *mut u16,
    lp_title: *mut u16,
    dw_x: u32,
    dw_y: u32,
    dw_x_size: u32,
    dw_y_size: u32,
    dw_x_count_chars: u32,
    dw_y_count_chars: u32,
    dw_fill_attribute: u32,
    dw_flags: u32,
    w_show_window: u16,
    cb_reserved2: u16,
    lp_reserved2: *mut u8,
    h_std_input: HANDLE,
    h_std_output: HANDLE,
    h_std_error: HANDLE,
}

#[repr(C)]
struct PROCESS_INFORMATION {
    h_process: HANDLE,
    h_thread: HANDLE,
    dw_process_id: u32,
    dw_thread_id: u32,
}

#[repr(C)]
struct SID_AND_ATTRIBUTES {
    sid: *mut c_void,
    attributes: u32,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn StartServiceCtrlDispatcherW(lp_service_start_table: *const SERVICE_TABLE_ENTRYW) -> BOOL;
    fn RegisterServiceCtrlHandlerExW(
        lp_service_name: *const u16,
        lp_handler_proc: unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32,
        lp_context: *mut c_void,
    ) -> HANDLE;
    fn SetServiceStatus(h_service_status: HANDLE, lp_service_status: *const SERVICE_STATUS) -> BOOL;
    fn OpenSCManagerW(lp_machine_name: *const u16, lp_database_name: *const u16, dw_desired_access: u32) -> HANDLE;
    fn OpenServiceW(h_sc_manager: HANDLE, lp_service_name: *const u16, dw_desired_access: u32) -> HANDLE;
    fn CloseServiceHandle(h_sc_object: HANDLE) -> BOOL;
    fn OpenProcessToken(process: HANDLE, desired_access: u32, token: *mut HANDLE) -> BOOL;
    fn GetTokenInformation(token: HANDLE, class: u32, info: *mut c_void, info_len: u32, return_len: *mut u32) -> BOOL;
    fn SetTokenInformation(token: HANDLE, class: u32, info: *const c_void, info_len: u32) -> BOOL;
    fn DuplicateTokenEx(
        h_existing_token: HANDLE,
        dw_desired_access: u32,
        lp_token_attributes: *const c_void,
        impersonation_level: u32,
        token_type: u32,
        ph_new_token: *mut HANDLE,
    ) -> BOOL;
    fn IsWellKnownSid(sid: *const c_void, well_known_sid_type: u32) -> BOOL;
    fn CreateProcessAsUserW(
        h_token: HANDLE,
        lp_application_name: *const u16,
        lp_command_line: *mut u16,
        lp_process_attributes: *const c_void,
        lp_thread_attributes: *const c_void,
        b_inherit_handles: BOOL,
        dw_creation_flags: u32,
        lp_environment: *const c_void,
        lp_current_directory: *const u16,
        lp_startup_info: *const STARTUPINFOW,
        lp_process_information: *mut PROCESS_INFORMATION,
    ) -> BOOL;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> HANDLE;
    fn GetCurrentProcessId() -> u32;
    fn ProcessIdToSessionId(dw_process_id: u32, p_session_id: *mut u32) -> BOOL;
    fn WTSGetActiveConsoleSessionId() -> u32;
    fn CreateEventW(
        lp_event_attributes: *const c_void,
        b_manual_reset: BOOL,
        b_initial_state: BOOL,
        lp_name: *const u16,
    ) -> HANDLE;
    fn SetEvent(h_event: HANDLE) -> BOOL;
    fn WaitForSingleObject(h_handle: HANDLE, dw_milliseconds: u32) -> u32;
    fn GetExitCodeProcess(h_process: HANDLE, lp_exit_code: *mut u32) -> BOOL;
    fn TerminateProcess(h_process: HANDLE, u_exit_code: u32) -> BOOL;
    fn CloseHandle(h_object: HANDLE) -> BOOL;
    fn GetLastError() -> u32;
}

#[link(name = "wtsapi32")]
unsafe extern "system" {
    fn WTSQueryUserToken(session_id: u32, ph_token: *mut HANDLE) -> BOOL;
}

#[link(name = "userenv")]
unsafe extern "system" {
    fn CreateEnvironmentBlock(lp_environment: *mut *mut c_void, h_token: HANDLE, b_inherit: BOOL) -> BOOL;
    fn DestroyEnvironmentBlock(lp_environment: *mut c_void) -> BOOL;
}

static STATUS_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static STOP_EVENT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// True when the setup installed the service (any user may query it). Cached for the process lifetime.
pub fn installed() -> bool {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    *INSTALLED.get_or_init(|| unsafe {
        let scm = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return false;
        }
        let name = to_wide(SERVICE_NAME);
        let service = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS);
        if !service.is_null() {
            CloseServiceHandle(service);
        }
        CloseServiceHandle(scm);
        !service.is_null()
    })
}

/// True when this process runs as LocalSystem (the service, or the agent it started).
pub fn is_local_system() -> bool {
    static SYSTEM: OnceLock<bool> = OnceLock::new();
    *SYSTEM.get_or_init(|| unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        // TOKEN_USER is a SID_AND_ATTRIBUTES followed by the SID itself.
        let mut buf = [0u64; 16];
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TOKEN_USER_CLASS, buf.as_mut_ptr() as *mut c_void, 128, &mut len);
        CloseHandle(token);
        ok != 0 && IsWellKnownSid((*(buf.as_ptr() as *const SID_AND_ATTRIBUTES)).sid, WIN_LOCAL_SYSTEM_SID) != 0
    })
}

/// Entry point of `OpenDesk.exe --service`; returns when the service stops. Fails (1) outside the Service
/// Control Manager.
pub fn run() -> i32 {
    let mut name = to_wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW { lp_service_name: name.as_mut_ptr(), lp_service_proc: Some(service_main) },
        SERVICE_TABLE_ENTRYW { lp_service_name: ptr::null_mut(), lp_service_proc: None },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        logger::warn("service", "--service only runs under the Service Control Manager.");
        return 1;
    }
    0
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = to_wide(SERVICE_NAME);
    let status = unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), control_handler, ptr::null_mut()) };
    if status.is_null() {
        return;
    }
    STATUS_HANDLE.store(status, Ordering::SeqCst);
    let stop = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    STOP_EVENT.store(stop, Ordering::SeqCst);
    report(SERVICE_RUNNING);
    logger::info("service", &format!("{SERVICE_NAME} {} started.", env!("CARGO_PKG_VERSION")));
    supervise(stop);
    logger::info("service", &format!("{SERVICE_NAME} stopped."));
    report(SERVICE_STOPPED);
}

unsafe extern "system" fn control_handler(control: u32, _event: u32, _data: *mut c_void, _ctx: *mut c_void) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            report(SERVICE_STOP_PENDING);
            unsafe { SetEvent(STOP_EVENT.load(Ordering::SeqCst)) };
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

fn report(state: u32) {
    let status = SERVICE_STATUS {
        dw_service_type: SERVICE_WIN32_OWN_PROCESS,
        dw_current_state: state,
        dw_controls_accepted: if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN } else { 0 },
        dw_win32_exit_code: NO_ERROR,
        dw_service_specific_exit_code: 0,
        dw_check_point: 0,
        dw_wait_hint: if state == SERVICE_STOP_PENDING { 5000 } else { 0 },
    };
    unsafe { SetServiceStatus(STATUS_HANDLE.load(Ordering::SeqCst), &status) };
}

/// The agent process the service started.
struct Agent {
    process: HANDLE,
    session: u32,
    started: Instant,
}

impl Agent {
    fn running(&self) -> bool {
        unsafe { WaitForSingleObject(self.process, 0) == WAIT_TIMEOUT }
    }

    fn terminate(self) {
        unsafe { TerminateProcess(self.process, 0) };
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.process) };
    }
}

/// Keeps one agent in the active console session until `stop` is signalled.
fn supervise(stop: HANDLE) {
    let mut agent: Option<Agent> = None;
    let mut retry_at = Instant::now();
    let mut logon = None;
    loop {
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        if let Some(a) = agent.take() {
            if a.session != session {
                logger::info(
                    "service",
                    &format!("Console moved to session {session}; stopping the agent of {}.", a.session),
                );
                a.terminate();
            } else if a.running() {
                agent = Some(a);
            } else if a.started.elapsed() < MIN_UPTIME {
                logger::warn("service", "Agent exited right after start; retrying in 30 s.");
                retry_at = Instant::now() + RETRY_AFTER;
            }
        }
        if agent.is_none() && session != NO_SESSION && Instant::now() >= retry_at {
            match start_agent(session, &mut logon) {
                Ok(started) => agent = started,
                Err(e) => {
                    logger::warn("service", &format!("Could not start the agent in session {session}: {e}"));
                    retry_at = Instant::now() + RETRY_AFTER;
                }
            }
        }
        if unsafe { WaitForSingleObject(stop, POLL_MS) } == WAIT_OBJECT_0 {
            break;
        }
    }
    if let Some(a) = agent {
        a.terminate();
    }
}

/// Starts the agent in `session`. `Ok(None)`: nothing to start yet (no user logged on, the logon is too recent,
/// or the user stopped OpenDesk). `logon` remembers since when `session` has had a logged-on user.
fn start_agent(session: u32, logon: &mut Option<(u32, Instant)>) -> Result<Option<Agent>, String> {
    let Some(user) = Token::of_session_user(session) else { return Ok(None) };
    let since = match *logon {
        Some((s, since)) if s == session => since,
        _ => logon.insert((session, Instant::now())).1,
    };
    // Explorer's notification area needs a few seconds after logon before the tray icon can be added.
    if since.elapsed() < LOGON_GRACE {
        return Ok(None);
    }
    let env = Environment::of(&user)?;
    if env.stop_flag().is_some_and(|flag| flag.exists()) {
        return Ok(None);
    }
    let system = Token::system_in_session(session)?;
    let process = spawn(&system, "--background", &env)?;
    logger::info("service", &format!("Agent started in session {session}."));
    Ok(Some(Agent { process, session, started: Instant::now() }))
}

/// Runs this executable with `args` as the user logged on to this process's session and waits for it.
/// Used by the agent for UI that must not run as SYSTEM (the file picker). False when it could not start or
/// exited with an error.
pub fn run_as_session_user(args: &str) -> bool {
    let mut session = 0u32;
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 {
        return false;
    }
    let Some(user) = Token::of_session_user(session) else { return false };
    let result = Environment::of(&user).and_then(|env| spawn(&user, args, &env));
    match result {
        Ok(process) => unsafe {
            WaitForSingleObject(process, INFINITE);
            let mut code = 1u32;
            GetExitCodeProcess(process, &mut code);
            CloseHandle(process);
            code == 0
        },
        Err(e) => {
            logger::warn("service", &format!("Could not run '{args}' as the session user: {e}"));
            false
        }
    }
}

/// An owned access token.
struct Token(HANDLE);

impl Token {
    /// The logged-on user's token; None when nobody is logged on to `session`.
    fn of_session_user(session: u32) -> Option<Token> {
        let mut token: HANDLE = ptr::null_mut();
        (unsafe { WTSQueryUserToken(session, &mut token) } != 0).then_some(Token(token))
    }

    /// A primary copy of this process's SYSTEM token, moved to `session`.
    fn system_in_session(session: u32) -> Result<Token, String> {
        unsafe {
            let mut own: HANDLE = ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut own) == 0 {
                return Err(last_error("OpenProcessToken"));
            }
            let own = Token(own);
            let mut dup: HANDLE = ptr::null_mut();
            if DuplicateTokenEx(own.0, MAXIMUM_ALLOWED, ptr::null(), SECURITY_IMPERSONATION, TOKEN_PRIMARY, &mut dup)
                == 0
            {
                return Err(last_error("DuplicateTokenEx"));
            }
            let dup = Token(dup);
            let id = session;
            if SetTokenInformation(dup.0, TOKEN_SESSION_ID_CLASS, &id as *const u32 as *const c_void, 4) == 0 {
                return Err(last_error("SetTokenInformation(TokenSessionId)"));
            }
            Ok(dup)
        }
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// A user's environment block (`%APPDATA%`, `%USERPROFILE%`, `%TEMP%` of that user).
struct Environment(*mut c_void);

impl Environment {
    fn of(user: &Token) -> Result<Environment, String> {
        let mut block: *mut c_void = ptr::null_mut();
        if unsafe { CreateEnvironmentBlock(&mut block, user.0, 0) } == 0 {
            return Err(last_error("CreateEnvironmentBlock"));
        }
        Ok(Environment(block))
    }

    /// Value of `name` (case-insensitive), read from the double-NUL terminated UTF-16 block.
    fn var(&self, name: &str) -> Option<String> {
        let prefix = format!("{}=", name.to_ascii_uppercase());
        let mut p = self.0 as *const u16;
        unsafe {
            loop {
                let mut len = 0;
                while *p.add(len) != 0 {
                    len += 1;
                }
                if len == 0 {
                    return None;
                }
                let entry = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
                if entry.len() > prefix.len() && entry[..prefix.len()].eq_ignore_ascii_case(&prefix) {
                    return Some(entry[prefix.len()..].to_string());
                }
                p = p.add(len + 1);
            }
        }
    }

    /// The user's `%APPDATA%\OpenDesk\user_exit.flag` (see `storage::app_dir`).
    fn stop_flag(&self) -> Option<PathBuf> {
        self.var("APPDATA").map(|dir| PathBuf::from(dir).join("OpenDesk").join(STOP_FLAG))
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        unsafe { DestroyEnvironmentBlock(self.0) };
    }
}

/// Starts this executable with `args` under `token` on the session's interactive desktop.
fn spawn(token: &Token, args: &str, env: &Environment) -> Result<HANDLE, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    let app = to_wide(&exe.to_string_lossy());
    let mut cmd = to_wide(&format!("\"{}\" {args}", exe.display()));
    let cwd = to_wide(&dir.to_string_lossy());
    let mut desktop = to_wide("winsta0\\default");
    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.lp_desktop = desktop.as_mut_ptr();
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessAsUserW(
            token.0,
            app.as_ptr(),
            cmd.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_UNICODE_ENVIRONMENT,
            env.0,
            cwd.as_ptr(),
            &si,
            &mut pi,
        );
        if ok == 0 {
            return Err(last_error("CreateProcessAsUserW"));
        }
        CloseHandle(pi.h_thread);
        Ok(pi.h_process)
    }
}

fn last_error(call: &str) -> String {
    format!("{call} failed (error {})", unsafe { GetLastError() })
}
