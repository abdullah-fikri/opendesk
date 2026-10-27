//! Named threads, so the trace log and a debugger show which part of the app a thread belongs to, and
//! supervision for the threads a session or the whole app cannot work without.

use std::panic::{self, AssertUnwindSafe};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::infra::logger;

/// Spawns `f` on a thread called `name`. Panics if the OS cannot create the thread, like `thread::spawn`.
pub fn spawn_named<F, T>(name: &str, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new().name(name.into()).spawn(f).unwrap_or_else(|e| panic!("spawn {name}: {e}"))
}

/// Like [`spawn_named`] (`f` last so a closure body keeps its indentation), and if `f` panics, `on_panic` runs on that thread once the unwind is over (the panic
/// hook has logged it by then); the thread then ends with the same panic. `on_panic` runs after the unwind
/// rather than from a drop guard during it, so a panic inside it ends the thread instead of aborting the app.
pub fn spawn_supervised<F, T, P>(name: &str, on_panic: P, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    P: FnOnce() + Send + 'static,
{
    spawn_named(name, move || match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(payload) => {
            on_panic();
            panic::resume_unwind(payload)
        }
    })
}

/// Runs `body(state)` until it returns. After each panic, `recover(state)` cleans up, and `body` runs again
/// `delay` later: for a loop the app is useless without. A panic in `recover` is logged and ignored.
pub fn restart_on_panic<S>(
    what: &str,
    delay: Duration,
    state: &mut S,
    mut body: impl FnMut(&mut S),
    mut recover: impl FnMut(&mut S),
) {
    while panic::catch_unwind(AssertUnwindSafe(|| body(state))).is_err() {
        logger::error("thread", &format!("{what} panicked; restarting in {} ms.", delay.as_millis()));
        if panic::catch_unwind(AssertUnwindSafe(|| recover(state))).is_err() {
            logger::error("thread", &format!("Cleanup after the {what} panic panicked too."));
        }
        thread::sleep(delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn thread_carries_its_name() {
        let name = spawn_named("test-named", || thread::current().name().map(str::to_string)).join().unwrap();
        assert_eq!(name.as_deref(), Some("test-named"));
    }

    #[test]
    fn on_panic_runs_once_and_only_after_a_panic() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let crashed = spawn_supervised(
            "test-supervised",
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
            || panic!("deliberate"),
        );
        assert!(crashed.join().is_err(), "the thread still ends with its panic");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let counter = Arc::clone(&calls);
        let fine = spawn_supervised(
            "test-supervised",
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
            || 7,
        );
        assert_eq!(fine.join().unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no callback on a normal exit");
    }

    #[test]
    fn restarts_after_each_panic_until_the_body_returns() {
        // (runs, recoveries)
        let mut state = (0, 0);
        restart_on_panic(
            "test loop",
            Duration::ZERO,
            &mut state,
            |s| {
                s.0 += 1;
                if s.0 < 3 {
                    panic!("deliberate");
                }
            },
            |s| s.1 += 1,
        );
        assert_eq!(state, (3, 2));
    }
}
