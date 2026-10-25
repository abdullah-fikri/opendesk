//! Facts about this machine: a fingerprint for new desk IDs and a display name for the relay.

use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::desk_id;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetVolumeInformationW(
        lp_root_path_name: *const u16,
        lp_volume_name_buffer: *mut u16,
        n_volume_name_size: u32,
        lp_volume_serial_number: *mut u32,
        lp_maximum_component_length: *mut u32,
        lp_file_system_flags: *mut u32,
        lp_file_system_name_buffer: *mut u16,
        n_file_system_name_size: u32,
    ) -> i32;
}

/// A new desk ID for this machine. Mixes a machine fingerprint (the C: volume serial on Windows,
/// `/etc/machine-id` elsewhere), machine and user names and a nanosecond timestamp with FNV-1a, so even cloned
/// VMs get distinct IDs. The result is stored and reused.
pub fn new_desk_id() -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64-bit offset basis
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    if let Some(fingerprint) = machine_fingerprint() {
        mix(&fingerprint);
    }
    mix(&std::process::id().to_le_bytes());
    for value in [machine_name(), user_name()].into_iter().flatten() {
        mix(value.as_bytes());
    }
    if let Ok(dur) = SystemTime::now().duration_since(UNIX_EPOCH) {
        mix(&dur.as_nanos().to_le_bytes());
    }
    desk_id::from_fingerprint(hash)
}

/// Serial number of the C: volume.
#[cfg(windows)]
fn machine_fingerprint() -> Option<Vec<u8>> {
    let root: [u16; 4] = [b'C' as u16, b':' as u16, b'\\' as u16, 0];
    let mut serial = 0u32;
    // SAFETY: `root` is NUL-terminated; every other out-pointer is optional and passed as null.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    (ok != 0).then(|| serial.to_le_bytes().to_vec())
}

/// The systemd/D-Bus machine ID.
#[cfg(not(windows))]
fn machine_fingerprint() -> Option<Vec<u8>> {
    std::fs::read("/etc/machine-id").or_else(|_| std::fs::read("/var/lib/dbus/machine-id")).ok()
}

/// Machine name: `COMPUTERNAME` on Windows, the host name elsewhere.
#[cfg(windows)]
fn machine_name() -> Option<String> {
    env::var("COMPUTERNAME").ok()
}

/// Machine name: `COMPUTERNAME` on Windows, the host name elsewhere.
#[cfg(not(windows))]
fn machine_name() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .or_else(|| env::var("HOSTNAME").ok())
}

/// Login name of the current user.
fn user_name() -> Option<String> {
    env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).ok()
}

/// "COMPUTER (user)", shown to administrators in the host list.
pub fn display_name() -> String {
    let fallback = if cfg!(windows) { "Windows-PC" } else { "Linux-PC" };
    let computer = machine_name().unwrap_or_else(|| fallback.to_string());
    match user_name() {
        Some(user) if !user.is_empty() => format!("{computer} ({user})"),
        _ => computer,
    }
}
