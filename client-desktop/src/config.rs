use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

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
    fn GetCurrentProcessId() -> u32;
}

const LEGACY_HARDCODED_ID: &str = "145494982";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub gateway_url: String,
    pub desk_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gateway_url: "ws://127.0.0.1:8088".to_string(),
            desk_id: generate_unique_desk_id(),
        }
    }
}

pub fn generate_unique_desk_id() -> String {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a 64-bit offset basis
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };

    // 1. Hardware Volume Serial Number (C:\)
    unsafe {
        let root: [u16; 4] = [b'C' as u16, b':' as u16, b'\\' as u16, 0];
        let mut serial: u32 = 0;
        if GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            mix(&serial.to_le_bytes());
        }
        mix(&GetCurrentProcessId().to_le_bytes());
    }

    // 2. Machine & User Environment
    if let Ok(comp) = env::var("COMPUTERNAME") {
        mix(comp.as_bytes());
    }
    if let Ok(user) = env::var("USERNAME") {
        mix(user.as_bytes());
    }

    // 3. High-resolution nanosecond entropy so even cloned VMs get distinct IDs
    if let Ok(dur) = SystemTime::now().duration_since(UNIX_EPOCH) {
        mix(&dur.as_nanos().to_le_bytes());
    }

    // Map to 9-digit number [100_000_000 .. 999_999_999]
    let id_num = 100_000_000u64 + (hash % 900_000_000u64);
    if id_num.to_string() == LEGACY_HARDCODED_ID {
        return "284910573".to_string();
    }
    id_num.to_string()
}

pub fn normalize_gateway_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return "ws://127.0.0.1:8088".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("https://") {
        return format!("wss://{}", rest);
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        return format!("ws://{}", rest);
    }
    if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        return trimmed.to_string();
    }
    if trimmed.starts_with("127.")
        || trimmed.starts_with("localhost")
        || trimmed.starts_with("192.168.")
        || trimmed.starts_with("10.")
    {
        format!("ws://{}", trimmed)
    } else {
        format!("wss://{}", trimmed)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryRecord {
    pub desk_id: String,
    pub alias: String,
    pub last_connected: String,
}

pub fn get_app_dir() -> PathBuf {
    if let Ok(appdata) = env::var("APPDATA") {
        let p = PathBuf::from(appdata).join("OpenDesk");
        let _ = fs::create_dir_all(&p);
        p
    } else {
        PathBuf::from(".")
    }
}

pub fn load_config() -> Config {
    let path = get_app_dir().join("config.json");
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(mut cfg) = serde_json::from_str::<Config>(&data) {
            let clean = clean_desk_id(&cfg.desk_id);
            let mut changed = false;
            if clean.len() < 9 || clean == LEGACY_HARDCODED_ID {
                cfg.desk_id = generate_unique_desk_id();
                changed = true;
            }
            let norm_gw = normalize_gateway_url(&cfg.gateway_url);
            if norm_gw != cfg.gateway_url {
                cfg.gateway_url = norm_gw;
                changed = true;
            }
            if changed {
                save_config(&cfg);
            }
            return cfg;
        }
    }

    let cfg = Config::default();
    save_config(&cfg);
    cfg
}

pub fn save_config(cfg: &Config) {
    let path = get_app_dir().join("config.json");
    if let Ok(data) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(&path, &data);
    }
}

pub fn load_history() -> Vec<HistoryRecord> {
    let path = get_app_dir().join("history.json");
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(list) = serde_json::from_str::<Vec<HistoryRecord>>(&data) {
            return list;
        }
    }
    Vec::new()
}

pub fn save_history(list: &[HistoryRecord]) {
    let path = get_app_dir().join("history.json");
    if let Ok(data) = serde_json::to_string_pretty(list) {
        let _ = fs::write(&path, &data);
    }
}

pub fn format_desk_id(raw: &str) -> String {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 9 {
        format!("{} {} {}", &digits[0..3], &digits[3..6], &digits[6..9])
    } else {
        digits
    }
}

pub fn clean_desk_id(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_digit()).collect()
}

pub fn get_device_display_name() -> String {
    let comp = env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows-PC".to_string());
    let user = env::var("USERNAME").unwrap_or_default();
    if user.is_empty() {
        comp
    } else {
        format!("{} ({})", comp, user)
    }
}
