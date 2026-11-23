//! Watchdog of the connection loop: the loop marks progress every tick, and a separate thread checks it.
//!
//! The loop itself recovers from panics, dead sockets and hung connects, but anything else that blocks its
//! thread for good (a deadlock, a call that never returns) would leave the process running and the device
//! offline until someone restarts OpenDesk, which is what the field saw. Then the only cure is a fresh
//! process: the service's agent exits and the service starts a new one. A copy nothing restarts only logs it.

use std::sync::LazyLock;
use std::sync::atomic::AtomicU64;

use super::*;

/// No progress of the loop for this long means it is stuck. Far above anything it legitimately waits on
/// (a connect is capped at `CONNECT_DEADLINE`, a reconnect pause is seconds).
const STALL_LIMIT: Duration = Duration::from_secs(180);
/// How often the watchdog looks.
const CHECK_EVERY: Duration = Duration::from_secs(15);

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
/// Milliseconds since `EPOCH` of the loop's last progress.
static BEAT: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    EPOCH.elapsed().as_millis() as u64
}

/// The connection loop made progress.
pub(super) fn beat() {
    BEAT.store(now_ms(), Ordering::Relaxed);
}

/// Whether the loop is stuck: no beat for `STALL_LIMIT`. `gap` is how long the watchdog itself slept: much
/// longer than `CHECK_EVERY` means the machine was asleep, and the loop gets a fresh chance after waking.
fn stalled(since_beat: Duration, gap: Duration) -> bool {
    gap < CHECK_EVERY * 3 && since_beat >= STALL_LIMIT
}

/// Starts the watchdog thread.
pub(super) fn spawn() {
    beat();
    crate::infra::thread::spawn_named("relay-watchdog", || {
        let mut last = Instant::now();
        let mut reported = false;
        loop {
            thread::sleep(CHECK_EVERY);
            let gap = last.elapsed();
            last = Instant::now();
            let since_beat = Duration::from_millis(now_ms().saturating_sub(BEAT.load(Ordering::Relaxed)));
            if gap >= CHECK_EVERY * 3 {
                // Woke from sleep: the loop gets a full `STALL_LIMIT` from now.
                beat();
                continue;
            }
            if !stalled(since_beat, gap) {
                reported = false;
                continue;
            }
            // The service starts its agent again when it exits.
            if crate::infra::autostart::supervised_by_service() {
                logger::error(
                    "relay",
                    &format!(
                        "The relay connection loop has been stuck for {} s; exiting so the service restarts OpenDesk.",
                        since_beat.as_secs()
                    ),
                );
                std::process::exit(3);
            }
            if !std::mem::replace(&mut reported, true) {
                logger::error(
                    "relay",
                    &format!(
                        "The relay connection loop has been stuck for {} s; restart OpenDesk to reconnect.",
                        since_beat.as_secs()
                    ),
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_long_stall_while_awake_counts() {
        let tick = CHECK_EVERY;
        assert!(!stalled(Duration::from_secs(60), tick), "a slow connect or a reconnect pause");
        assert!(!stalled(STALL_LIMIT - Duration::from_secs(1), tick));
        assert!(stalled(STALL_LIMIT, tick));
        // The machine slept: the loop could not beat, it is not stuck.
        assert!(!stalled(Duration::from_secs(3600), Duration::from_secs(3600)));
        // Even the longest legitimate wait (a connect attempt) stays far below the limit.
        assert!(dial::CONNECT_DEADLINE * 3 < STALL_LIMIT);
    }
}
