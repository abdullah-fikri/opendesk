//! Domain layer: the client's data types and rules. Nothing here performs I/O, touches the UI or builds
//! wire messages, so every function is unit-testable.

pub mod capability;
pub mod desk_id;
pub mod edition;
pub mod frame_rate;
pub mod history;
pub mod keyboard;
pub mod latency;
pub mod monitor;
#[cfg(feature = "admin")]
pub mod monitor_wall;
pub mod perf_stats;
pub mod resolution;
pub mod settings;
pub mod transfer;
