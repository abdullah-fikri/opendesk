//! Wi-Fi media streaming mode while a remote session runs: background Wi-Fi scans stall the radio for ~100 ms
//! every few seconds, which shows as latency spikes. Anything streaming holds a [`Streaming`] guard; the first
//! guard asks the WLAN service for media streaming mode on every connected wireless interface, and dropping the
//! last one hands the setting back (see `docs/features/wifi-streaming.md`).
//!
//! The WLAN calls run on one worker thread, started with the first session, so a slow or hung WLAN service never
//! blocks the UI or a session thread. `OPENDESK_WIFI_STREAMING=0` disables it. Windows only; a no-op elsewhere
//! and on machines without Wi-Fi.

#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as sys;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
use unix as sys;

use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use parking_lot::Mutex;

use crate::infra::logger;

/// Number of live [`Streaming`] guards; reports when the WLAN setting has to change.
#[derive(Default)]
struct Users(usize);

impl Users {
    /// Counts one more user; true when it is the first (streaming mode must be turned on).
    fn acquire(&mut self) -> bool {
        self.0 += 1;
        self.0 == 1
    }

    /// Counts one user less; true when it was the last (streaming mode must be released).
    fn release(&mut self) -> bool {
        self.0 = self.0.saturating_sub(1);
        self.0 == 0
    }
}

static USERS: Mutex<Users> = Mutex::new(Users(0));

/// Keeps Wi-Fi media streaming mode requested while alive. Cheap to create and drop: it never waits for the
/// WLAN service.
pub struct Streaming(());

/// Requests streaming mode until the guard is dropped (sessions overlap: a counter).
pub fn streaming() -> Streaming {
    let mut users = USERS.lock();
    if users.acquire() {
        request(true);
    }
    Streaming(())
}

impl Drop for Streaming {
    fn drop(&mut self) {
        let mut users = USERS.lock();
        if users.release() {
            request(false);
        }
    }
}

/// Queues the wanted state for the worker. Callers hold the [`USERS`] lock, so a session ending while another
/// starts cannot reorder "off" after "on"; the send itself never blocks.
fn request(on: bool) {
    if let Some(worker) = worker() {
        // The worker lives as long as the process; a send can only fail during shutdown.
        let _ = worker.send(on);
    }
}

/// The worker's queue, or `None` when the feature is off (kill switch, platform). Logs the reason once.
fn worker() -> Option<&'static Sender<bool>> {
    static WORKER: OnceLock<Option<Sender<bool>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            if std::env::var("OPENDESK_WIFI_STREAMING").as_deref() == Ok("0") {
                logger::info("wifi", "Wi-Fi streaming mode disabled (OPENDESK_WIFI_STREAMING=0).");
                return None;
            }
            // Unit tests open viewer sessions; they must not change the developer's Wi-Fi adapter.
            if !sys::SUPPORTED || cfg!(test) {
                logger::info("wifi", "Wi-Fi streaming mode is not available on this platform.");
                return None;
            }
            let (tx, rx) = mpsc::channel();
            crate::infra::thread::spawn_named("wifi-streaming", move || run(rx));
            Some(tx)
        })
        .as_ref()
}

/// Applies each requested state in order. Logs an outcome only when it differs from the previous one, so a
/// machine without Wi-Fi says so once instead of on every session.
fn run(rx: mpsc::Receiver<bool>) {
    let mut lease: Option<sys::Lease> = None;
    let mut last_report = String::new();
    for on in rx {
        if !on {
            lease = None;
            continue;
        }
        if lease.is_some() {
            continue;
        }
        let (applied, report) = sys::enable();
        lease = applied;
        if report != last_report {
            logger::info("wifi", &report);
            last_report = report;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Users;

    #[test]
    fn first_acquire_and_last_release_change_the_setting() {
        let mut users = Users::default();
        assert!(users.acquire());
        assert!(!users.acquire());
        assert!(!users.acquire());
        assert!(!users.release());
        assert!(!users.release());
        assert!(users.release());
        // A new first session turns it on again.
        assert!(users.acquire());
        assert!(users.release());
    }

    #[test]
    fn release_without_acquire_stays_at_zero() {
        let mut users = Users::default();
        assert!(users.release());
        assert!(users.acquire());
    }
}
