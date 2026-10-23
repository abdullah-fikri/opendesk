//! Facts about this machine: a fingerprint for new desk IDs and a display name for the relay.

use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::desk_id;

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

/// A new desk ID for this machine. Mixes the C: volume serial, machine and user names and a nanosecond
/// timestamp with FNV-1a, so even cloned VMs get distinct IDs. The result is stored and reused.
pub fn new_desk_id() -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64-bit offset basis
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
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
    if ok != 0 {
        mix(&serial.to_le_bytes());
    }
    mix(&std::process::id().to_le_bytes());
    for key in ["COMPUTERNAME", "USERNAME"] {
        if let Ok(v) = env::var(key) {
            mix(v.as_bytes());
        }
    }
    if let Ok(dur) = SystemTime::now().duration_since(UNIX_EPOCH) {
        mix(&dur.as_nanos().to_le_bytes());
    }
    desk_id::from_fingerprint(hash)
}

/// "COMPUTER (user)", shown to administrators in the host list.
pub fn display_name() -> String {
    let computer = env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows-PC".to_string());
    match env::var("USERNAME") {
        Ok(user) if !user.is_empty() => format!("{computer} ({user})"),
        _ => computer,
    }
}
