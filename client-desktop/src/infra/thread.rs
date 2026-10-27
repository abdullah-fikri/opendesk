//! Named threads, so the trace log and a debugger show which part of the app a thread belongs to.

use std::thread::{self, JoinHandle};

/// Spawns `f` on a thread called `name`. Panics if the OS cannot create the thread, like `thread::spawn`.
pub fn spawn_named<F, T>(name: &str, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new().name(name.into()).spawn(f).unwrap_or_else(|e| panic!("spawn {name}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_carries_its_name() {
        let name = spawn_named("test-named", || thread::current().name().map(str::to_string)).join().unwrap();
        assert_eq!(name.as_deref(), Some("test-named"));
    }
}
