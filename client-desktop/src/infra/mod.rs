//! Infrastructure layer: adapters to the OS, the file system and the network that services use. Modules here
//! hold no business rules; they wrap one external facility each. OS-specific modules are directories with a
//! `win` and a `unix` backend exposing the same functions, selected with `cfg` (static dispatch).

pub mod audio;
pub mod autostart;
#[cfg(windows)]
pub mod clipboard_helper;
pub mod clock;
pub mod console;
pub mod device;
pub mod e2e;
pub mod file_attrs;
pub mod identity;
pub mod key_pins;
pub mod logger;
pub mod p2p;
pub mod pointer_lock;
pub mod port_map;
pub mod power;
pub mod privacy;
pub mod process_usage;
pub mod send_gate;
pub mod storage;
pub mod system_service;
pub mod tcp_path;
pub mod thread;
pub mod transfer_badge;
pub mod tray;
pub mod udp_socket;
pub mod user_clipboard;
pub mod virtual_clipboard;
pub mod wifi;
pub mod window;
