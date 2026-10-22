use std::env;
use std::ffi::c_void;
use std::fs;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::ptr;

use crate::config::{
    format_desk_id, get_app_dir, get_device_display_name, load_config, normalize_gateway_url,
    save_config,
};
use crate::logger;
use crate::tray::WM_TRAYICON;

type HANDLE = *mut c_void;
type HWND = *mut c_void;
type BOOL = i32;

const ERROR_ALREADY_EXISTS: u32 = 183;
const WM_LBUTTONUP: isize = 0x0202;
const CREATE_NO_WINDOW: u32 = 0x08000000;
const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
const STD_OUTPUT_HANDLE: u32 = 0xFFFFFFF5;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(
        lp_mutex_attributes: *const c_void,
        b_initial_owner: BOOL,
        lp_name: *const u16,
    ) -> HANDLE;
    fn GetLastError() -> u32;
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
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn cli_println(msg: &str) {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        let h_out = GetStdHandle(STD_OUTPUT_HANDLE);
        let line = format!("\r\n{}\r\n", msg);
        if !h_out.is_null() {
            let mut written: u32 = 0;
            WriteFile(
                h_out,
                line.as_ptr(),
                line.len() as u32,
                &mut written,
                ptr::null_mut(),
            );
        }
        let _ = std::io::stdout().write_all(line.as_bytes());
        let _ = std::io::stdout().flush();
    }
}

/// Checks if the user invoked OpenDesk via CLI (e.g. `opendesk server --wss://...` or `opendesk status`).
/// If a CLI command was handled, updates config and exits immediately.
pub fn handle_cli_commands() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        return;
    }

    let first = args[0].as_str();

    // 1. `opendesk server --wss://...` or `opendesk server wss://...` or `opendesk --server=...`
    if first.eq_ignore_ascii_case("server") || first.starts_with("--server=") {
        let raw_url = if let Some(rest) = first.strip_prefix("--server=") {
            rest.to_string()
        } else if args.len() >= 2 {
            args[1].trim_start_matches('-').to_string()
        } else {
            String::new()
        };

        if raw_url.is_empty() {
            let cfg = load_config();
            cli_println(&format!(
                "[OpenDesk CLI]\r\n  ID Perangkat : {}\r\n  Server Aktif : {}\r\n  Cara Ubah    : opendesk server --wss://domain-anda.trycloudflare.com",
                format_desk_id(&cfg.desk_id),
                cfg.gateway_url
            ));
            std::process::exit(0);
        }

        let mut cfg = load_config();
        let clean_url = normalize_gateway_url(&raw_url);
        cfg.gateway_url = clean_url.clone();
        save_config(&cfg);

        logger::info(
            "cli",
            &format!("Gateway URL updated via CLI to: {}", clean_url),
        );
        cli_println(&format!(
            "[OpenDesk CLI] Berhasil!\r\n  Server Relay : {}\r\n  ID Perangkat : {}\r\n  Status       : Daemon background otomatis menyambung ulang ke server baru.",
            clean_url,
            format_desk_id(&cfg.desk_id)
        ));
        std::process::exit(0);
    }

    // 2. `opendesk status`, `opendesk id`, `opendesk stop`, `opendesk uninstall`
    if first.eq_ignore_ascii_case("stop") || first.eq_ignore_ascii_case("kill") {
        mark_intentional_stop();
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "wscript.exe"])
            .output();
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "OpenDesk.exe"])
            .output();
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "OpenDesk-Host.exe"])
            .output();
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "OpenDesk-Admin.exe"])
            .output();
        cli_println("[OpenDesk CLI] OpenDesk dan Watchdog Guardian berhasil dihentikan.");
        std::process::exit(0);
    }

    if first.eq_ignore_ascii_case("uninstall") {
        mark_intentional_stop();
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "wscript.exe"])
            .output();
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "OpenDesk.exe"])
            .output();
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "OpenDesk-Host.exe"])
            .output();
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "OpenDesk-Admin.exe"])
            .output();
        let _ = Command::new("reg")
            .args(["delete", "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run", "/v", "OpenDesk", "/f"])
            .output();
        cli_println("[OpenDesk CLI] Proses dihentikan dan autorun registry dihapus.\r\nSilakan jalankan uninstaller atau hapus folder instalasi.");
        std::process::exit(0);
    }

    if first.eq_ignore_ascii_case("status")
        || first.eq_ignore_ascii_case("id")
        || first.eq_ignore_ascii_case("--status")
        || first.eq_ignore_ascii_case("help")
        || first.eq_ignore_ascii_case("--help")
    {
        let cfg = load_config();
        cli_println(&format!(
            "[OpenDesk CLI]\r\n  Nama Perangkat : {}\r\n  ID Perangkat   : {}\r\n  Server Relay   : {}\r\n\r\nPerintah CLI:\r\n  opendesk server --wss://{{url}}   Ubah alamat Server Relay\r\n  opendesk status                  Lihat ID & Server aktif\r\n  opendesk stop                    Hentikan proses & watchdog\r\n  opendesk uninstall               Persiapan uninstall bersih",
            get_device_display_name(),
            format_desk_id(&cfg.desk_id),
            cfg.gateway_url
        ));
        std::process::exit(0);
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
        if !handle.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            if !start_in_background && !cfg!(feature = "headless") {
                let tray_class = to_wide("OpenDeskTrayMsgClass");
                let tray_win = to_wide("OpenDeskTrayMessageWindow");
                let hwnd = FindWindowW(tray_class.as_ptr(), tray_win.as_ptr());
                if !hwnd.is_null() {
                    PostMessageW(hwnd, WM_TRAYICON, 0, WM_LBUTTONUP);
                }
            }
            std::process::exit(0);
        }
        let _ = handle;
    }
}

/// Marks an intentional user stop from the System Tray ("Stop & Keluar") so the watchdog pauses until next launch.
pub fn mark_intentional_stop() {
    let flag_path = get_app_dir().join("user_exit.flag");
    let _ = fs::write(flag_path, "stopped");
}

/// Registers `opendesk.cmd` in `%LOCALAPPDATA%\Microsoft\WindowsApps` (which is in Windows PATH by default)
/// so the user can run `opendesk server --wss://...` from any terminal immediately after install.
fn register_cli_shim(exe_str: &str) {
    if let Ok(local_app) = env::var("LOCALAPPDATA") {
        let win_apps = PathBuf::from(local_app)
            .join("Microsoft")
            .join("WindowsApps");
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
    let app_dir = get_app_dir();
    let stop_flag = app_dir.join("user_exit.flag");
    let _ = fs::remove_file(&stop_flag);

    let exe_path = match env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let exe_str = match exe_path.to_str() {
        Some(s) => s.to_string(),
        None => return,
    };
    let exe_name = exe_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("OpenDesk.exe")
        .to_string();

    register_cli_shim(&exe_str);

    let lock_path = app_dir.join("watchdog.lock");
    let vbs_path = app_dir.join("opendesk-watchdog.vbs");

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
        let _ = Command::new("wscript.exe")
            .args(["//B", "//NoLogo", vbs_str])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();

        logger::info(
            "app.service",
            "Background autostart, CLI shim (`opendesk`), & 10s watchdog guardian active.",
        );
    }
}
