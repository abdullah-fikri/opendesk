//! Windows backend: the WLAN API. `wlanapi.dll` is loaded at run time rather than imported, because Windows
//! Server without the Wireless LAN Service feature does not ship it and a missing import would stop the app from
//! starting at all.
//!
//! Media streaming mode lasts as long as the WLAN handle that set it: closing the handle (or the process ending)
//! hands it back. Disabling background scans is tried as well; Windows may refuse it to a standard user, which is
//! fine, and it is re-enabled explicitly before the handle closes.

use std::ffi::c_void;

use windows::Win32::Foundation::{FreeLibrary, HANDLE, HMODULE};
use windows::Win32::NetworkManagement::WiFi::{
    WLAN_INTERFACE_INFO, WLAN_INTERFACE_INFO_LIST, WLAN_INTF_OPCODE, wlan_interface_state_connected,
    wlan_intf_opcode_background_scan_enabled, wlan_intf_opcode_media_streaming_mode,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::core::{GUID, PCSTR, s, w};

/// Streaming mode can be requested on Windows.
pub const SUPPORTED: bool = true;

/// `WLAN_API_VERSION_2_0` (Vista and later).
const CLIENT_VERSION: u32 = 2;
const ERROR_SERVICE_NOT_ACTIVE: u32 = 1062;

type OpenHandleFn = unsafe extern "system" fn(u32, *const c_void, *mut u32, *mut HANDLE) -> u32;
type CloseHandleFn = unsafe extern "system" fn(HANDLE, *const c_void) -> u32;
type EnumInterfacesFn = unsafe extern "system" fn(HANDLE, *const c_void, *mut *mut WLAN_INTERFACE_INFO_LIST) -> u32;
type SetInterfaceFn =
    unsafe extern "system" fn(HANDLE, *const GUID, WLAN_INTF_OPCODE, u32, *const c_void, *const c_void) -> u32;
type FreeMemoryFn = unsafe extern "system" fn(*const c_void);
/// What `GetProcAddress` returns before the cast to the real signature.
type RawProc = unsafe extern "system" fn() -> isize;

/// The WLAN functions this module uses, resolved from `wlanapi.dll`.
struct Api {
    module: HMODULE,
    open_handle: OpenHandleFn,
    close_handle: CloseHandleFn,
    enum_interfaces: EnumInterfacesFn,
    set_interface: SetInterfaceFn,
    free_memory: FreeMemoryFn,
}

impl Api {
    /// Loads `wlanapi.dll` from System32; `None` when it is missing or lacks a function.
    fn load() -> Option<Api> {
        // SAFETY: loading a system DLL by name from System32 only; the module is freed in `Drop`.
        let module = unsafe { LoadLibraryExW(w!("wlanapi.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.ok()?;
        let find = |name: PCSTR| {
            // SAFETY: `module` is loaded and `name` is a NUL-terminated literal.
            unsafe { GetProcAddress(module, name) }
        };
        let resolved = (|| {
            let (open, close, enumerate, set, free) = (
                find(s!("WlanOpenHandle"))?,
                find(s!("WlanCloseHandle"))?,
                find(s!("WlanEnumInterfaces"))?,
                find(s!("WlanSetInterface"))?,
                find(s!("WlanFreeMemory"))?,
            );
            // SAFETY: each pointer is the documented export of that name, cast to its documented signature.
            unsafe {
                Some(Api {
                    module,
                    open_handle: std::mem::transmute::<RawProc, OpenHandleFn>(open),
                    close_handle: std::mem::transmute::<RawProc, CloseHandleFn>(close),
                    enum_interfaces: std::mem::transmute::<RawProc, EnumInterfacesFn>(enumerate),
                    set_interface: std::mem::transmute::<RawProc, SetInterfaceFn>(set),
                    free_memory: std::mem::transmute::<RawProc, FreeMemoryFn>(free),
                })
            }
        })();
        if resolved.is_none() {
            // SAFETY: nothing from the module is in use.
            let _ = unsafe { FreeLibrary(module) };
        }
        resolved
    }

    /// Sets a BOOL interface option; returns the Win32 error code (0 on success).
    fn set_bool(&self, handle: HANDLE, interface: &GUID, opcode: WLAN_INTF_OPCODE, value: bool) -> u32 {
        let value = u32::from(value);
        // SAFETY: `handle` is open; the GUID and the 4-byte BOOL outlive the call, which copies them.
        unsafe {
            (self.set_interface)(
                handle,
                interface,
                opcode,
                size_of::<u32>() as u32,
                (&value as *const u32).cast(),
                std::ptr::null(),
            )
        }
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        // SAFETY: the WLAN handle (if any) was closed before this, so no function from the module is in use.
        let _ = unsafe { FreeLibrary(self.module) };
    }
}

/// An open WLAN handle with streaming mode applied; dropping it hands the settings back.
pub struct Lease {
    api: Api,
    handle: HANDLE,
    /// Interfaces whose background scans this lease turned off.
    scan_off: Vec<GUID>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        for interface in &self.scan_off {
            self.api.set_bool(self.handle, interface, wlan_intf_opcode_background_scan_enabled, true);
        }
        // SAFETY: the handle was opened by this lease and is closed exactly once; closing it ends streaming mode.
        unsafe { (self.api.close_handle)(self.handle, std::ptr::null()) };
    }
}

/// Requests streaming mode on every connected wireless interface. Returns the lease to keep while streaming
/// (`None` when nothing was applied) and a one-line report for the log.
pub fn enable() -> (Option<Lease>, String) {
    let Some(api) = Api::load() else {
        return (None, "Wi-Fi streaming mode: no WLAN API on this machine.".into());
    };
    let mut handle = HANDLE::default();
    let mut negotiated = 0u32;
    // SAFETY: both out-pointers are valid locals.
    let err = unsafe { (api.open_handle)(CLIENT_VERSION, std::ptr::null(), &mut negotiated, &mut handle) };
    if err == ERROR_SERVICE_NOT_ACTIVE {
        return (None, "Wi-Fi streaming mode: the WLAN service is not running (no Wi-Fi).".into());
    }
    if err != 0 {
        return (None, format!("Wi-Fi streaming mode: WlanOpenHandle failed (error {err})."));
    }
    // From here the lease owns the handle, so every early return closes it.
    let mut lease = Lease { api, handle, scan_off: Vec::new() };

    let connected = match connected_interfaces(&lease.api, handle) {
        Ok(list) => list,
        Err(err) => return (None, format!("Wi-Fi streaming mode: WlanEnumInterfaces failed (error {err}).")),
    };
    if connected.is_empty() {
        return (None, "Wi-Fi streaming mode: no connected Wi-Fi interface.".into());
    }

    let mut streaming = 0;
    let mut refused = 0;
    for interface in &connected {
        match lease.api.set_bool(handle, interface, wlan_intf_opcode_media_streaming_mode, true) {
            0 => streaming += 1,
            err => refused = err,
        }
        if lease.api.set_bool(handle, interface, wlan_intf_opcode_background_scan_enabled, false) == 0 {
            lease.scan_off.push(*interface);
        }
    }
    if streaming == 0 && lease.scan_off.is_empty() {
        return (None, format!("Wi-Fi streaming mode: refused by Windows (error {refused})."));
    }
    let report = format!(
        "Wi-Fi streaming mode on: media streaming on {streaming} of {} connected interface(s), background scan \
         off on {}.",
        connected.len(),
        lease.scan_off.len()
    );
    (Some(lease), report)
}

/// GUIDs of the wireless interfaces that are connected, or the Win32 error of the enumeration.
fn connected_interfaces(api: &Api, handle: HANDLE) -> Result<Vec<GUID>, u32> {
    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    // SAFETY: `handle` is open and `list` is a valid out-pointer.
    let err = unsafe { (api.enum_interfaces)(handle, std::ptr::null(), &mut list) };
    if err != 0 || list.is_null() {
        return Err(err);
    }
    // SAFETY: WlanEnumInterfaces returned a list of `dwNumberOfItems` entries laid out after the header (the
    // declared one-element array is a C flexible array); it stays valid until WlanFreeMemory below.
    let connected = unsafe {
        let count = (*list).dwNumberOfItems as usize;
        let first = std::ptr::addr_of!((*list).InterfaceInfo).cast::<WLAN_INTERFACE_INFO>();
        (0..count)
            .map(|i| &*first.add(i))
            .filter(|info| info.isState == wlan_interface_state_connected)
            .map(|info| info.InterfaceGuid)
            .collect()
    };
    // SAFETY: `list` came from WlanEnumInterfaces and nothing refers to it any more.
    unsafe { (api.free_memory)(list.cast()) };
    Ok(connected)
}

#[cfg(test)]
mod tests {
    /// Applies and releases streaming mode on this machine's Wi-Fi: `cargo test --release -- --ignored
    /// wifi --nocapture`. Prints what Windows allowed for the current account.
    #[test]
    #[ignore = "changes the Wi-Fi adapter settings of this machine for a moment"]
    fn applies_and_releases_streaming_mode() {
        let (lease, report) = super::enable();
        println!("{report}");
        drop(lease);
    }
}
