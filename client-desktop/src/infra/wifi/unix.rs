//! Outside Windows there is no portable way to pause background Wi-Fi scans for an unprivileged process, so
//! streaming mode is not offered; the worker is never started.

/// False: [`super::streaming`] guards only count.
pub const SUPPORTED: bool = false;

/// Never created on this platform.
pub enum Lease {}

/// Not reached ([`SUPPORTED`] is false); kept so the shared worker compiles on every platform.
pub fn enable() -> (Option<Lease>, String) {
    (None, "Wi-Fi streaming mode is not available on this platform.".into())
}
