//! Infrastructure layer: adapters to the OS, the file system and the network that services use. Modules here
//! hold no business rules; they wrap one external facility each. OS-specific modules are directories with a
//! `win` and a `unix` backend exposing the same functions, selected with `cfg` (static dispatch).

pub mod audio;
pub mod autostart;
pub mod clock;
pub mod device;
pub mod logger;
pub mod p2p;
pub mod power;
pub mod privacy;
pub mod send_gate;
pub mod storage;
pub mod system_service;
pub mod thread;
pub mod tray;
pub mod user_clipboard;
pub mod virtual_clipboard;
pub mod window;
