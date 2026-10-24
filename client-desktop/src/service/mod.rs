//! Service layer (use cases). Services own the client's behavior: they read and write domain state, send
//! protocol messages through the relay connection and report to the user interface only through
//! [`ui_port::UiPort`]. Nothing here depends on Slint widgets.

pub mod clipboard_sync;
pub mod context;
pub mod file_transfer;
pub mod history;
pub mod host;
#[cfg(feature = "admin")]
pub mod monitor_wall;
pub mod relay;
pub mod sessions;
pub mod settings;
pub mod ui_port;
pub mod viewer;

#[cfg(test)]
pub mod test_support;
