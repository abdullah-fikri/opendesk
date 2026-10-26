//! Process lifecycle on Windows: single instance, the SYSTEM service (`infra::system_service`), the elevated
//! logon task of older installs, HKCU Run autostart, the watchdog that restarts OpenDesk when it is killed,
//! and the `opendesk` command shim.

use std::env;
use std::ffi::c_void;
use std::fs;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::ptr;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use crate::infra::logger;
use crate::infra::storage::app_dir;
use crate::infra::system_service;
use crate::infra::tray::WM_TRAYICON;

type HANDLE = *mut c_void;
type HWND = *mut c_void;
type BOOL = i32;

const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_ACCESS_DENIED: u32 = 5;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_ELEVATION_CLASS: u32 = 20; // TokenElevation
const MSGFLT_ADD: u32 = 1;

/// Task Scheduler task registered by the installer (elevated, at logon, for the installing user).
/// Must match `ElevatedTaskName` in installer/opendesk.iss.
pub const ELEVATED_TASK_NAME: &str = "OpenDesk Autostart";
/// In the app directory while the user wants OpenDesk stopped: pauses the watchdog and the service.
pub const STOP_FLAG: &str = "user_exit.flag";
const WM_LBUTTONUP: isize = 0x0202;
const CREATE_NO_WINDOW: u32 = 0x08000000;
const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
const STD_OUTPUT_HANDLE: u32 = 0xFFFFFFF5;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(lp_mutex_attributes: *const c_void, b_initial_owner: BOOL, lp_name: *const u16) -> HANDLE;
    fn GetLastError() -> u32;
    fn CloseHandle(h_object: HANDLE) -> BOOL;
    fn GetCurrentProcess() -> HANDLE;
    fn AttachConsole(dw_process_id: u32) -> BOOL;
    fn GetStdHandle(n_std_handle: u32) -> HANDLE;
    fn WriteFile(
        h_file: HANDLE,
        lp_buffer: *const u8,
        n_number_of_bytes_to_write: u32,
        lp_number_of_bytes_written: *mut u32,
        lp_overlapped: *mut c_void,
    ) -> BOOL;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(lp_class_name: *const u16, lp_window_name: *const u16) -> HWND;
    fn PostMessageW(h_wnd: HWND, msg: u32, w_param: usize, l_param: isize) -> BOOL;
    fn ChangeWindowMessageFilter(message: u32, dw_flag: u32) -> BOOL;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: HANDLE, desired_access: u32, token: *mut HANDLE) -> BOOL;
    fn GetTokenInformation(token: HANDLE, class: u32, info: *mut c_void, info_len: u32, return_len: *mut u32) -> BOOL;
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// True when this process runs with an elevated (admin) token.
fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevated: u32 = 0;
        let mut ret_len: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            &mut elevated as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
            &mut ret_len,
        );
        CloseHandle(token);
        ok != 0 && elevated != 0
    }
}

/// True when the installer's elevated logon task is visible to this user.
/// Cached so `schtasks` is spawned at most once per process. Portable/dev runs: no task -> false.
fn elevated_task_installed() -> bool {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    *INSTALLED.get_or_init(|| run_schtasks("/Query"))
}

fn run_schtasks(verb: &str) -> bool {
    Command::new("schtasks")
        .args([verb, "/TN", ELEVATED_TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn find_tray_window() -> HWND {
    let tray_class = to_wide("OpenDeskTrayMsgClass");
    let tray_win = to_wide("OpenDeskTrayMessageWindow");
    unsafe { FindWindowW(tray_class.as_ptr(), tray_win.as_ptr()) }
}

/// Non-elevated launch (Start Menu, desktop icon, leftover watchdog) while the elevated task exists:
/// start the task instead, so a UIPI-limited copy never runs. False = task not startable (caller falls back).
fn delegate_to_elevated_task(start_in_background: bool) -> bool {
    if !run_schtasks("/Run") {
        return false;
    }
    logger::info("app.service", "Non-elevated launch delegated to elevated logon task.");
    // The task always starts with --background; pop the window for interactive launches.
    if !start_in_background && !cfg!(feature = "headless") {
        for _ in 0..50 {
            let hwnd = find_tray_window();
            if !hwnd.is_null() {
                unsafe { PostMessageW(hwnd, WM_TRAYICON, 0, WM_LBUTTONUP) };
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
    true
}

/// Launch by the user (Start Menu, desktop icon) while the service is installed: the service runs the real
/// copy as SYSTEM. Clears a previous "Stop & Exit" so the service starts it again, then wakes its window.
/// False = no copy appeared (service stopped); the caller falls back to running this one.
fn delegate_to_service(start_in_background: bool) -> bool {
    let _ = fs::remove_file(app_dir().join(STOP_FLAG));
    // The service polls every 2 s and waits 10 s after it first sees the logon (right after install).
    for _ in 0..100 {
        let hwnd = find_tray_window();
        if !hwnd.is_null() {
            if !start_in_background && !cfg!(feature = "headless") {
                unsafe { PostMessageW(hwnd, WM_TRAYICON, 0, WM_LBUTTONUP) };
            }
            logger::info("app.service", "Launch handed over to the OpenDesk service.");
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    logger::warn("app.service", "OpenDesk service did not start the app; running without it.");
    false
}

/// True when this process is the service's agent, which the service restarts when it exits.
pub fn supervised_by_service() -> bool {
    system_service::is_local_system() && system_service::installed()
}

/// Writes `msg` to the console of the terminal that launched this GUI-subsystem process.
pub fn print_to_console(msg: &str) {
    let line = format!(
        "
{}
",
        msg
    );
    // SAFETY: attaching to the parent console and writing a byte slice of known length.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        let h_out = GetStdHandle(STD_OUTPUT_HANDLE);
        if !h_out.is_null() {
            let mut written: u32 = 0;
            WriteFile(h_out, line.as_ptr(), line.len() as u32, &mut written, ptr::null_mut());
        }
    }
    let _ = std::io::stdout().write_all(line.as_bytes());
    let _ = std::io::stdout().flush();
}

/// Process names of every OpenDesk build flavor.
const PROCESS_NAMES: [&str; 3] = ["OpenDesk.exe", "OpenDesk-Host.exe", "OpenDesk-Admin.exe"];

fn taskkill(image: &str) -> Option<i32> {
    Command::new("taskkill").args(["/F", "/IM", image]).creation_flags(CREATE_NO_WINDOW).output().ok()?.status.code()
}

/// Why stopping the running instance failed.
#[derive(Debug, PartialEq, Eq)]
pub enum StopError {
    /// The running instance is elevated and this process is not.
    AccessDenied,
}

/// Stops the watchdog and every running OpenDesk process (`opendesk stop`).
pub fn stop_running() -> Result<(), StopError> {
    mark_intentional_stop();
    // The service's SYSTEM agent cannot be killed from a user terminal; it exits by itself on the flag.
    if system_service::installed() {
        for _ in 0..25 {
            if find_tray_window().is_null() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
    // End via Task Scheduler first so its restart-on-failure policy does not bring it back.
    let _ = run_schtasks("/End");
    let _ = taskkill("wscript.exe");
    // taskkill exit code 1 = access denied (elevated instance, non-elevated terminal).
    if taskkill(PROCESS_NAMES[0]) == Some(1) {
        return Err(StopError::AccessDenied);
    }
    for name in &PROCESS_NAMES[1..] {
        let _ = taskkill(name);
    }
    Ok(())
}

/// Stops everything and removes autostart entries (`opendesk uninstall`). Removing the service and the elevated
/// task needs an elevated terminal; the uninstaller does it as well.
pub fn remove_autostart() {
    mark_intentional_stop();
    for verb in ["stop", "delete"] {
        let _ = Command::new("sc").args([verb, system_service::SERVICE_NAME]).creation_flags(CREATE_NO_WINDOW).output();
    }
    let _ = run_schtasks("/End");
    let _ = Command::new("schtasks")
        .args(["/Delete", "/TN", ELEVATED_TASK_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = taskkill("wscript.exe");
    for name in PROCESS_NAMES {
        let _ = taskkill(name);
    }
    for value in ["OpenDesk", "OpenDeskWatchdog"] {
        let _ = Command::new("reg")
            .args(["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", value, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
}

/// Ensures only a single instance of OpenDesk runs at any time.
/// If another instance is already running in the background:
/// - When launched interactively (not `--background` and not headless), wakes up and brings the existing window to the front.
/// - Terminates this duplicate process immediately so Task Manager never has 2 OpenDesk processes.
pub fn ensure_single_instance(start_in_background: bool) {
    unsafe {
        let mutex_name = to_wide("Local\\OpenDesk_SingleInstance_Mutex_v2");
        let handle = CreateMutexW(ptr::null(), 1, mutex_name.as_ptr());
        let err = GetLastError();
        // A mutex owned by an elevated instance cannot be opened from a non-elevated one
        // (admin-only DACL + High integrity label) -> ACCESS_DENIED also means "already running".
        let already_running =
            (!handle.is_null() && err == ERROR_ALREADY_EXISTS) || (handle.is_null() && err == ERROR_ACCESS_DENIED);
        if already_running {
            if !start_in_background && !cfg!(feature = "headless") {
                let hwnd = find_tray_window();
                if !hwnd.is_null() {
                    PostMessageW(hwnd, WM_TRAYICON, 0, WM_LBUTTONUP);
                }
            }
            std::process::exit(0);
        }

        let system = system_service::is_local_system();
        if is_elevated() || system {
            // UIPI drops messages from non-elevated senders; let a second launch (shortcut click)
            // still wake our tray window. Explorer's tray callbacks use the same message.
            let _ = ChangeWindowMessageFilter(WM_TRAYICON, MSGFLT_ADD);
        }
        if !system && system_service::installed() {
            // Release the lock first so the agent started by the service can take it.
            if !handle.is_null() {
                CloseHandle(handle);
            }
            if delegate_to_service(start_in_background) {
                std::process::exit(0);
            }
            let retry = CreateMutexW(ptr::null(), 1, mutex_name.as_ptr());
            if retry.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
                std::process::exit(0);
            }
        } else if !is_elevated() && elevated_task_installed() {
            // Release the lock first so the elevated copy started by the task can take it.
            if !handle.is_null() {
                CloseHandle(handle);
            }
            if delegate_to_elevated_task(start_in_background) {
                std::process::exit(0);
            }
            // Task could not be started: keep today's behaviour and run non-elevated.
            let retry = CreateMutexW(ptr::null(), 1, mutex_name.as_ptr());
            if retry.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
                std::process::exit(0);
            }
        }
    }
}

/// Marks an intentional user stop from the System Tray ("Stop & Exit") so the watchdog pauses until next launch.
pub fn mark_intentional_stop() {
    let flag_path = app_dir().join(STOP_FLAG);
    let _ = fs::write(flag_path, "stopped");
}

/// Registers `opendesk.cmd` in `%LOCALAPPDATA%\Microsoft\WindowsApps` (which is in Windows PATH by default)
/// so the user can run `opendesk server --wss://...` from any terminal immediately after install.
fn register_cli_shim(exe_str: &str) {
    if let Ok(local_app) = env::var("LOCALAPPDATA") {
        let win_apps = PathBuf::from(local_app).join("Microsoft").join("WindowsApps");
        if win_apps.exists() {
            let cmd_path = win_apps.join("opendesk.cmd");
            let script = format!("@echo off\r\n\"{}\" %*\r\n", exe_str);
            let _ = fs::write(cmd_path, script);
        }
    }
}

/// Clears any previous intentional stop flag, registers Windows boot autostart + CLI shim,
/// and starts the 10-second background watchdog guardian (respawns OpenDesk if killed via Task Manager).
pub fn init_background_guardian() {
    let app_dir = app_dir();
    let stop_flag = app_dir.join(STOP_FLAG);
    let _ = fs::remove_file(&stop_flag);

    let exe_path = match env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let exe_str = match exe_path.to_str() {
        Some(s) => s.to_string(),
        None => return,
    };
    let exe_name = exe_path.file_name().and_then(|n| n.to_str()).unwrap_or("OpenDesk.exe").to_string();

    register_cli_shim(&exe_str);

    let lock_path = app_dir.join("watchdog.lock");
    let vbs_path = app_dir.join("opendesk-watchdog.vbs");

    // Installed build with the service: it owns autostart and restarts. `opendesk stop` from a user terminal
    // cannot kill a SYSTEM process, so the agent watches for the stop flag itself.
    if system_service::installed() {
        if system_service::is_local_system() {
            thread::spawn(move || {
                loop {
                    thread::sleep(Duration::from_secs(1));
                    if stop_flag.exists() {
                        logger::info("app.service", "Stop requested (user_exit.flag); exiting.");
                        std::process::exit(0);
                    }
                }
            });
        }
        logger::info(
            "app.service",
            &format!("Service '{}' active; HKCU Run & watchdog skipped.", system_service::SERVICE_NAME),
        );
        return;
    }

    // Installed build: the elevated logon task owns autostart (+ restart on failure).
    // Re-adding HKCU Run / the VBS watchdog would start a second, non-elevated copy.
    if elevated_task_installed() {
        if vbs_path.exists() {
            // One-time cleanup of leftovers from pre-task versions (installer normally did this).
            for value in ["OpenDesk", "OpenDeskWatchdog"] {
                let _ = Command::new("reg")
                    .args(["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", value, "/f"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
            }
            let _ = fs::remove_file(&vbs_path);
            let _ = fs::remove_file(&lock_path);
        }
        logger::info(
            "app.service",
            &format!("Elevated logon task '{}' active; HKCU Run & watchdog skipped.", ELEVATED_TASK_NAME),
        );
        return;
    }

    let vbs_script = format!(
        r#"On Error Resume Next
Dim fso, shell, wmi, lockFile
Set fso = CreateObject("Scripting.FileSystemObject")
Set shell = CreateObject("WScript.Shell")

' Single-instance lock for watchdog itself (exclusive write handle)
Err.Clear
Set lockFile = fso.OpenTextFile("{lock}", 8, True)
If Err.Number <> 0 Then
    WScript.Quit 0
End If

Set wmi = GetObject("winmgmts:\\.\root\cimv2")

Do
    WScript.Sleep 10000
    If fso.FileExists("{stop}") Then
        WScript.Quit 0
    End If

    Dim procs
    Set procs = wmi.ExecQuery("SELECT ProcessId FROM Win32_Process WHERE Name = '{exe_name}' OR Name = 'OpenDesk.exe' OR Name = 'OpenDesk-Host.exe' OR Name = 'OpenDesk-Admin.exe' OR Name = 'opendesk-client.exe'")
    If procs.Count = 0 Then
        shell.Run """{exe_full}"" --background", 0, False
    End If
Loop
"#,
        lock = lock_path.to_string_lossy().replace('"', ""),
        stop = stop_flag.to_string_lossy().replace('"', ""),
        exe_name = exe_name.replace('\'', ""),
        exe_full = exe_str.replace('"', ""),
    );

    let _ = fs::write(&vbs_path, vbs_script);

    // 1. Register OpenDesk in HKCU Run (starts on Windows Boot / Login after Shutdown or Restart)
    let run_cmd = format!("\"{}\" --background", exe_str);
    let _ = Command::new("reg")
        .args([
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "OpenDesk",
            "/t",
            "REG_SZ",
            "/d",
            &run_cmd,
            "/f",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    // 2. Register Watchdog in HKCU Run
    if let Some(vbs_str) = vbs_path.to_str() {
        let wd_cmd = format!("wscript.exe //B //NoLogo \"{}\"", vbs_str);
        let _ = Command::new("reg")
            .args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "OpenDeskWatchdog",
                "/t",
                "REG_SZ",
                "/d",
                &wd_cmd,
                "/f",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output();

        // 3. Spawn the silent 10-second Watchdog Guardian right now
        let _ = Command::new("wscript.exe").args(["//B", "//NoLogo", vbs_str]).creation_flags(CREATE_NO_WINDOW).spawn();

        logger::info("app.service", "Background autostart, CLI shim (`opendesk`), & 10s watchdog guardian active.");
    }
}
