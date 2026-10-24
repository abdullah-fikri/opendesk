//! Following the input desktop for capture threads. Only Windows has separate input desktops (Winlogon, UAC).

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

/// Other platforms have a single desktop per session: never switches.
#[cfg(not(windows))]
pub fn switch_to_input_desktop() -> bool {
    false
}
