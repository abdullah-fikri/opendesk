//! Infrastructure layer: adapters to Windows, the file system and the network that services use. Modules here
//! hold no business rules; they wrap one external facility each.

pub mod audio;
pub mod autostart;
pub mod clock;
pub mod device;
pub mod logger;
pub mod p2p;
pub mod privacy;
pub mod storage;
pub mod tray;
pub mod virtual_clipboard;
pub mod window;
