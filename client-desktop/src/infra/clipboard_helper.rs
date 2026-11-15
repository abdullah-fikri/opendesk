//! Clipboard helper: the service's SYSTEM agent cannot put virtual files on the user's clipboard (the user's
//! Explorer will not pull from a SYSTEM process), so it starts `OpenDesk.exe --clipboard-helper` as the
//! logged-on user (the way AnyDesk splits its service from its user frontend). The helper holds only the OLE
//! objects (`IDataObject`, `IStream`); the agent keeps the network, the offer bookkeeping, the size and SHA-256
//! checks, and passes each paste stream its bytes over two one-way named pipes.
//!
//! Security: the pipes are created before the helper starts, as the first and only instance, local clients only,
//! with a DACL of SYSTEM + the session user. The agent accepts only the PID it started; the helper accepts only
//! the agent PID it was given. Messages are size-limited and the agent validates every request against its own
//! copy of the offers (`virtual_clipboard::helper_request`). The helper has no network access of its own.
//!
//! Wire format, both directions: `[u32 LE length of what follows][u8 tag][body]`; tag 1 = JSON message,
//! tag 2 = paste-stream bytes `[u8 tid length][tid][bytes]` (agent to helper only).

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};

use crate::infra::{logger, system_service, virtual_clipboard};
use crate::protocol::OfferEntry;

type HANDLE = *mut c_void;
type BOOL = i32;

const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
const PIPE_ACCESS_INBOUND: u32 = 0x1;
const PIPE_ACCESS_OUTBOUND: u32 = 0x2;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x8;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
const ERROR_PIPE_CONNECTED: u32 = 535;
const SDDL_REVISION_1: u32 = 1;
const PIPE_BUFFER: u32 = 256 * 1024;

/// Largest frame either side accepts: one 128 KiB chunk plus header, or an offer listing.
const MAX_FRAME: usize = 4 * 1024 * 1024;
const TAG_JSON: u8 = 1;
const TAG_DATA: u8 = 2;
/// How long the agent waits for a started helper to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[repr(C)]
struct SECURITY_ATTRIBUTES {
    n_length: u32,
    lp_security_descriptor: *mut c_void,
    b_inherit_handle: BOOL,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer: u32,
        in_buffer: u32,
        default_timeout: u32,
        security: *const SECURITY_ATTRIBUTES,
    ) -> HANDLE;
    fn ConnectNamedPipe(pipe: HANDLE, overlapped: *mut c_void) -> BOOL;
    fn GetNamedPipeClientProcessId(pipe: HANDLE, pid: *mut u32) -> BOOL;
    fn GetNamedPipeServerProcessId(pipe: HANDLE, pid: *mut u32) -> BOOL;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        disposition: u32,
        flags: u32,
        template: HANDLE,
    ) -> HANDLE;
    fn ReadFile(file: HANDLE, buf: *mut c_void, len: u32, read: *mut u32, overlapped: *mut c_void) -> BOOL;
    fn WriteFile(file: HANDLE, buf: *const c_void, len: u32, written: *mut u32, overlapped: *mut c_void) -> BOOL;
    fn CloseHandle(h: HANDLE) -> BOOL;
    fn GetLastError() -> u32;
    fn GetCurrentProcessId() -> u32;
    fn LocalFree(h: *mut c_void) -> *mut c_void;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl: *const u16,
        revision: u32,
        sd: *mut *mut c_void,
        sd_len: *mut u32,
    ) -> BOOL;
}

/// Agent to helper.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum ToHelper {
    /// Put the partner's offer on the clipboard (the helper validates `files` again). With `drop` it is dragged
    /// onto the window under the cursor instead and the clipboard stays as it is.
    Set {
        from: String,
        offer_id: String,
        files: Vec<OfferEntry>,
        #[serde(default)]
        drop: bool,
    },
    /// Session with `partner` ended (empty = any).
    Clear { partner: String },
    /// Paste stream `tid` is complete (`ok`) or failed.
    End { tid: String, ok: bool },
}

/// Helper to agent.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum ToAgent {
    /// An application pastes entry `index` (of the validated list) of offer `offer_id`; stream it as `tid`.
    Pull { from: String, offer_id: String, index: usize, tid: String },
    /// The application closed paste stream `tid` (finished or cancelled).
    Close { tid: String },
}

/// One end of a one-way pipe; closed on drop. Writers take `write` so frames from several threads never interleave.
struct Pipe(HANDLE, Mutex<()>);

// SAFETY: a pipe handle may be used from any thread; each end is used by one reader or one writer at a time.
unsafe impl Send for Pipe {}
unsafe impl Sync for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl Pipe {
    fn write_all(&self, mut bytes: &[u8]) -> bool {
        while !bytes.is_empty() {
            let mut n = 0u32;
            let len = bytes.len().min(u32::MAX as usize) as u32;
            if unsafe { WriteFile(self.0, bytes.as_ptr() as *const c_void, len, &mut n, ptr::null_mut()) } == 0
                || n == 0
            {
                return false;
            }
            bytes = &bytes[n as usize..];
        }
        true
    }

    fn read_exact(&self, out: &mut [u8]) -> bool {
        let mut at = 0;
        while at < out.len() {
            let mut n = 0u32;
            let len = (out.len() - at) as u32;
            let p = out[at..].as_mut_ptr() as *mut c_void;
            if unsafe { ReadFile(self.0, p, len, &mut n, ptr::null_mut()) } == 0 || n == 0 {
                return false;
            }
            at += n as usize;
        }
        true
    }

    /// Writes one frame; false when the other side is gone.
    fn send(&self, tag: u8, body: &[&[u8]]) -> bool {
        let _frame = self.1.lock();
        let len: usize = 1 + body.iter().map(|b| b.len()).sum::<usize>();
        let mut frame = Vec::with_capacity(4 + len);
        frame.extend_from_slice(&(len as u32).to_le_bytes());
        frame.push(tag);
        for b in body {
            frame.extend_from_slice(b);
        }
        self.write_all(&frame)
    }

    /// Reads one frame as `(tag, body)`; None when the other side is gone or sent garbage.
    fn recv(&self) -> Option<(u8, Vec<u8>)> {
        let mut len = [0u8; 4];
        if !self.read_exact(&mut len) {
            return None;
        }
        let len = u32::from_le_bytes(len) as usize;
        if len == 0 || len > MAX_FRAME {
            return None;
        }
        let mut frame = vec![0u8; len];
        self.read_exact(&mut frame).then_some(())?;
        let tag = frame.remove(0);
        Some((tag, frame))
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn pipe_path(base: &str, dir: &str) -> String {
    format!(r"\\.\pipe\{base}-{dir}")
}

/// `a2h`: agent to helper; `h2a`: helper to agent.
const A2H: &str = "a2h";
const H2A: &str = "h2a";

/// Encodes paste-stream bytes (tag 2 body).
fn data_body(tid: &str) -> Vec<u8> {
    let mut head = Vec::with_capacity(1 + tid.len());
    head.push(tid.len() as u8);
    head.extend_from_slice(tid.as_bytes());
    head
}

/// Splits a tag 2 body into `(tid, bytes)`.
fn parse_data(body: &[u8]) -> Option<(&str, &[u8])> {
    let n = *body.first()? as usize;
    let tid = std::str::from_utf8(body.get(1..1 + n)?).ok()?;
    Some((tid, &body[1 + n..]))
}

// ---------------------------------------------------------------------------------------------
// Agent side.

/// Writer to the connected helper, if any.
static LINK: Mutex<Option<Arc<Pipe>>> = Mutex::new(None);
static READY: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
static STARTED: AtomicBool = AtomicBool::new(false);

/// `OPENDESK_CLIPBOARD_HELPER=0` keeps the 1.3.x behavior (no helper; copied files go the fallback way).
pub fn enabled() -> bool {
    std::env::var("OPENDESK_CLIPBOARD_HELPER").map_or(true, |v| v != "0")
}

/// True while a helper is connected, so offers can be put on the user's clipboard.
pub fn ready() -> bool {
    *READY.0.lock()
}

/// Waits up to `timeout` for the helper (only if one is being started). Lets the first relay registration
/// already advertise `file-offer`.
pub fn wait_ready(timeout: Duration) -> bool {
    if !STARTED.load(Ordering::SeqCst) {
        return false;
    }
    let mut ready = READY.0.lock();
    if !*ready {
        READY.1.wait_for(&mut ready, timeout);
    }
    *ready
}

fn set_ready(value: bool) {
    *READY.0.lock() = value;
    READY.1.notify_all();
}

/// Sends a message to the helper; false when none is connected.
pub fn send(msg: &ToHelper) -> bool {
    let Some(link) = LINK.lock().clone() else { return false };
    let json = serde_json::to_vec(msg).unwrap_or_default();
    link.send(TAG_JSON, &[&json])
}

/// Sends paste-stream bytes to the helper; false when none is connected.
pub fn send_data(tid: &str, bytes: &[u8]) -> bool {
    let Some(link) = LINK.lock().clone() else { return false };
    link.send(TAG_DATA, &[&data_body(tid), bytes])
}

/// Starts keeping a helper running for the session user (service agent only). Idempotent.
pub fn start() {
    if !enabled() || STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::infra::thread::spawn_named("clipboard-helper", || {
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = Instant::now();
            match run_once() {
                Ok(()) => logger::info("clipboard_helper", "Clipboard helper disconnected; restarting it."),
                Err(e) => logger::warn("clipboard_helper", &format!("Clipboard helper unavailable: {e}")),
            }
            set_ready(false);
            *LINK.lock() = None;
            virtual_clipboard::helper_lost();
            // A helper that keeps dying right away (no user, logoff in progress) is retried less often.
            backoff = if started.elapsed() > Duration::from_secs(60) {
                Duration::from_secs(1)
            } else {
                (backoff * 2).min(Duration::from_secs(60))
            };
            std::thread::sleep(backoff);
        }
    });
}

/// Creates one server pipe that only SYSTEM and `user_sid` may open.
fn create_server(path: &str, inbound: bool, user_sid: &str) -> Result<Pipe, String> {
    // SYSTEM full access, the session user read/write, nobody else; P = no inherited ACEs.
    let sddl = to_wide(&format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{user_sid})"));
    unsafe {
        let mut sd: *mut c_void = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            ptr::null_mut(),
        ) == 0
        {
            return Err(format!("security descriptor failed (error {})", GetLastError()));
        }
        let sa = SECURITY_ATTRIBUTES {
            n_length: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lp_security_descriptor: sd,
            b_inherit_handle: 0,
        };
        let access = if inbound { PIPE_ACCESS_INBOUND } else { PIPE_ACCESS_OUTBOUND };
        let name = to_wide(path);
        let h = CreateNamedPipeW(
            name.as_ptr(),
            access | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            &sa,
        );
        LocalFree(sd);
        if h == INVALID_HANDLE_VALUE {
            return Err(format!("CreateNamedPipe failed (error {})", GetLastError()));
        }
        Ok(Pipe(h, Mutex::new(())))
    }
}

/// Waits for a client on `pipe` and checks it is process `pid`.
fn accept(pipe: &Pipe, pid: u32) -> Result<(), String> {
    unsafe {
        if ConnectNamedPipe(pipe.0, ptr::null_mut()) == 0 && GetLastError() != ERROR_PIPE_CONNECTED {
            return Err(format!("ConnectNamedPipe failed (error {})", GetLastError()));
        }
        let mut client = 0u32;
        if GetNamedPipeClientProcessId(pipe.0, &mut client) == 0 || client != pid {
            return Err(format!("unexpected client process {client} (expected {pid})"));
        }
    }
    Ok(())
}

/// Starts one helper and serves it until it disconnects.
fn run_once() -> Result<(), String> {
    let sid = system_service::session_user_sid().ok_or("nobody is logged on")?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_micros();
    let base = format!("opendesk-clip-{}-{now}", std::process::id());
    let (a2h, h2a) = (pipe_path(&base, A2H), pipe_path(&base, H2A));
    let to_helper = create_server(&a2h, false, &sid)?;
    let from_helper = create_server(&h2a, true, &sid)?;
    let helper =
        Arc::new(system_service::start_as_session_user(&format!("--clipboard-helper {base} {}", std::process::id()))?);

    // ConnectNamedPipe blocks; if the helper dies or never connects, connect ourselves so it returns (the PID
    // check then fails).
    let connected = Arc::new(AtomicBool::new(false));
    {
        let (helper, connected, a2h, h2a) = (Arc::clone(&helper), Arc::clone(&connected), a2h.clone(), h2a.clone());
        crate::infra::thread::spawn_named("clipboard-helper-watch", move || {
            let deadline = Instant::now() + CONNECT_TIMEOUT;
            while !connected.load(Ordering::SeqCst) && Instant::now() < deadline && !helper.wait_exit(100) {}
            if !connected.load(Ordering::SeqCst) {
                for (path, access) in [(&a2h, GENERIC_READ), (&h2a, GENERIC_WRITE)] {
                    let name = to_wide(path);
                    let h = unsafe {
                        CreateFileW(name.as_ptr(), access, 0, ptr::null(), OPEN_EXISTING, 0, ptr::null_mut())
                    };
                    if h != INVALID_HANDLE_VALUE {
                        unsafe { CloseHandle(h) };
                    }
                }
            }
        });
    }
    let result = accept(&to_helper, helper.pid).and_then(|()| accept(&from_helper, helper.pid));
    connected.store(true, Ordering::SeqCst);
    if let Err(e) = result {
        helper.terminate();
        return Err(e);
    }

    logger::info("clipboard_helper", &format!("Clipboard helper connected (pid {}).", helper.pid));
    serve(to_helper, &from_helper);
    if !helper.wait_exit(2000) {
        helper.terminate();
    }
    Ok(())
}

/// Serves a connected helper until it disconnects.
fn serve(to_helper: Pipe, from_helper: &Pipe) {
    *LINK.lock() = Some(Arc::new(to_helper));
    set_ready(true);
    while let Some((tag, body)) = from_helper.recv() {
        let msg = (tag == TAG_JSON).then(|| serde_json::from_slice::<ToAgent>(&body).ok()).flatten();
        match msg {
            Some(msg) => virtual_clipboard::helper_request(msg),
            None => {
                logger::warn("clipboard_helper", "Clipboard helper sent an invalid message; restarting it.");
                break;
            }
        }
    }
    *LINK.lock() = None;
    set_ready(false);
}

/// Tests: starts `exe --clipboard-helper` as this user, with the test process as its agent, and serves it on a
/// thread (the real agent does the same as SYSTEM for the session user).
#[cfg(test)]
pub fn start_test_helper(exe: &std::path::Path) -> std::process::Child {
    let sid = system_service::process_user_sid().expect("own SID");
    let base = format!("opendesk-clip-test-{}", std::process::id());
    let to_helper = create_server(&pipe_path(&base, A2H), false, &sid).unwrap();
    let from_helper = create_server(&pipe_path(&base, H2A), true, &sid).unwrap();
    let child = std::process::Command::new(exe)
        .args(["--clipboard-helper", &base, &std::process::id().to_string()])
        .spawn()
        .expect("start the helper (build the app first: cargo build --release)");
    let pid = child.id();
    accept(&to_helper, pid).unwrap();
    accept(&from_helper, pid).unwrap();
    STARTED.store(true, Ordering::SeqCst);
    std::thread::spawn(move || serve(to_helper, &from_helper));
    assert!(wait_ready(Duration::from_secs(5)));
    child
}

// ---------------------------------------------------------------------------------------------
// Helper side (`OpenDesk.exe --clipboard-helper <pipe base> <agent pid>`, running as the user).

static AGENT: OnceLock<Pipe> = OnceLock::new();

/// Sends a request to the agent (helper process only).
pub fn to_agent(msg: &ToAgent) -> bool {
    let Some(pipe) = AGENT.get() else { return false };
    let json = serde_json::to_vec(msg).unwrap_or_default();
    pipe.send(TAG_JSON, &[&json])
}

/// Opens a client end and checks the server is process `agent_pid`.
fn open_client(path: &str, write: bool, agent_pid: u32) -> Result<Pipe, String> {
    let name = to_wide(path);
    let access = if write { GENERIC_WRITE } else { GENERIC_READ };
    let flags = SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
    let h = unsafe { CreateFileW(name.as_ptr(), access, 0, ptr::null(), OPEN_EXISTING, flags, ptr::null_mut()) };
    if h == INVALID_HANDLE_VALUE {
        return Err(format!("could not open {path} (error {})", unsafe { GetLastError() }));
    }
    let pipe = Pipe(h, Mutex::new(()));
    let mut server = 0u32;
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut server) } == 0 || server != agent_pid {
        return Err(format!("{path} belongs to process {server}, not the agent {agent_pid}"));
    }
    Ok(pipe)
}

/// The helper's main loop; returns the process exit code.
pub fn run_helper(base: Option<&String>, agent_pid: Option<&String>) -> i32 {
    let (Some(base), Some(agent_pid)) = (base, agent_pid.and_then(|p| p.parse::<u32>().ok())) else { return 2 };
    if !base.starts_with("opendesk-clip-")
        || base.contains(['\\', '/'])
        || agent_pid == unsafe { GetCurrentProcessId() }
    {
        return 2;
    }
    let from_agent = match open_client(&pipe_path(base, A2H), false, agent_pid) {
        Ok(p) => p,
        Err(e) => {
            logger::warn("clipboard_helper", &e);
            return 1;
        }
    };
    match open_client(&pipe_path(base, H2A), true, agent_pid) {
        Ok(p) => {
            let _ = AGENT.set(p);
        }
        Err(e) => {
            logger::warn("clipboard_helper", &e);
            return 1;
        }
    }
    logger::info("clipboard_helper", "Clipboard helper running for the session user.");
    while let Some((tag, body)) = from_agent.recv() {
        match tag {
            TAG_JSON => match serde_json::from_slice::<ToHelper>(&body) {
                Ok(msg) => virtual_clipboard::helper_command(msg),
                Err(_) => break,
            },
            TAG_DATA => match parse_data(&body) {
                Some((tid, bytes)) => virtual_clipboard::helper_data(tid, bytes.to_vec()),
                None => break,
            },
            _ => break,
        }
    }
    // Agent gone: take our files off the clipboard rather than leave objects nobody can feed.
    virtual_clipboard::helper_command(ToHelper::Clear { partner: String::new() });
    std::thread::sleep(Duration::from_millis(300));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_as_tagged_json() {
        let msg = ToAgent::Pull { from: "111".into(), offer_id: "of_1".into(), index: 2, tid: "fp_1_2".into() };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"t":"pull","from":"111","offer_id":"of_1","index":2,"tid":"fp_1_2"}"#);
        assert_eq!(serde_json::from_str::<ToAgent>(&json).unwrap(), msg);
        let end = ToHelper::End { tid: "fp_1_2".into(), ok: true };
        assert_eq!(serde_json::from_str::<ToHelper>(&serde_json::to_string(&end).unwrap()).unwrap(), end);
        assert!(serde_json::from_str::<ToAgent>(r#"{"t":"format-disk"}"#).is_err());
    }

    #[test]
    fn data_frames_carry_the_transfer_id() {
        let mut body = data_body("fp_9_0");
        body.extend_from_slice(b"bytes");
        assert_eq!(parse_data(&body), Some(("fp_9_0", &b"bytes"[..])));
        assert_eq!(parse_data(&[200, b'a']), None, "id longer than the frame");
        assert_eq!(parse_data(&[]), None);
    }
}
