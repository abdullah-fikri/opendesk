//! The service's writer of `progress.json` (see [`Progress`]): one instance shared by the service loop and the
//! check thread, so the schedule fields survive every phase change and two threads never race on the
//! temporary file. Download updates are limited to four writes a second.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::now_unix_ms;
use super::store::{Phase, Progress, Store};
use crate::infra::logger;

/// Minimum time between two download updates (at most four writes a second).
pub const DOWNLOAD_WRITE_EVERY: Duration = Duration::from_millis(250);

/// Shared handle to `progress.json`; clones write the same file.
#[derive(Clone)]
pub struct Reporter {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    store: Store,
    progress: Progress,
    last_download_write: Option<Instant>,
}

impl Reporter {
    /// Continues the file of the previous service in `root`. A phase that only a running check or idle wait
    /// can be in was cut short by a restart and is reported as failed (written with the next update).
    pub fn load(root: PathBuf) -> Reporter {
        let store = Store::new(root);
        let mut progress = store.progress().unwrap_or_default();
        if in_progress(progress.phase) {
            progress.phase = Phase::Failed;
            progress.error = Some("interrupted because the OpenDesk service restarted".into());
        }
        Reporter { inner: Arc::new(Mutex::new(Inner { store, progress, last_download_write: None })) }
    }

    /// The phase last reported.
    pub fn phase(&self) -> Phase {
        self.inner.lock().progress.phase
    }

    /// Reports `phase` for `version` (None keeps the version already reported) with an optional error.
    pub fn set(&self, phase: Phase, version: Option<String>, error: Option<String>) {
        let mut inner = self.inner.lock();
        let p = &mut inner.progress;
        p.phase = phase;
        if version.is_some() {
            p.version = version;
        }
        p.error = error;
        p.bytes = 0;
        p.total = 0;
        p.sessions_active = 0;
        write(&mut inner);
    }

    /// A check started at unix second `now`; the release is not known yet.
    pub fn checking(&self, now: u64) {
        let mut inner = self.inner.lock();
        inner.progress.checked_at = Some(now);
        inner.progress.version = None;
        drop(inner);
        self.set(Phase::Checking, None, None);
    }

    /// `bytes` of `total` downloaded for `version`; written at most every [`DOWNLOAD_WRITE_EVERY`] except the
    /// first and the last update.
    pub fn downloading(&self, version: &str, bytes: u64, total: u64) {
        let mut inner = self.inner.lock();
        let now = Instant::now();
        let first = inner.progress.phase != Phase::Downloading;
        if !first && bytes < total && !due(inner.last_download_write, now) {
            return;
        }
        inner.last_download_write = Some(now);
        let p = &mut inner.progress;
        p.phase = Phase::Downloading;
        p.version = Some(version.to_string());
        p.error = None;
        p.bytes = bytes;
        p.total = total;
        write(&mut inner);
    }

    /// The verified installer of `version` waits for `sessions` to end. Written when something changed or
    /// `refresh` is set (a request was just taken, so a waiting CLI sees a fresh file).
    pub fn waiting_idle(&self, version: &str, sessions: u32, refresh: bool) {
        let mut inner = self.inner.lock();
        let p = &inner.progress;
        let same =
            p.phase == Phase::WaitingIdle && p.version.as_deref() == Some(version) && p.sessions_active == sessions;
        if same && !refresh {
            return;
        }
        let p = &mut inner.progress;
        p.phase = Phase::WaitingIdle;
        p.version = Some(version.to_string());
        p.error = None;
        p.sessions_active = sessions;
        write(&mut inner);
    }

    /// The next automatic check is due at unix second `at`.
    pub fn next_check(&self, at: u64) {
        let mut inner = self.inner.lock();
        if inner.progress.next_check_at != Some(at) {
            inner.progress.next_check_at = Some(at);
            write(&mut inner);
        }
    }

    /// Rewrites an unfinished phase with a new timestamp, so a CLI that just queued a request sees the
    /// service took it while an earlier check is still running.
    pub fn touch(&self) {
        let mut inner = self.inner.lock();
        if in_progress(inner.progress.phase) {
            write(&mut inner);
        }
    }
}

fn write(inner: &mut Inner) {
    inner.progress.updated_at = now_unix_ms();
    if let Err(e) = inner.store.save_progress(&inner.progress) {
        logger::warn("update", &format!("progress.json not written: {e}"));
    }
}

/// A phase that lasts only while this service works on it (a check thread or the idle wait).
fn in_progress(phase: Phase) -> bool {
    matches!(phase, Phase::Checking | Phase::Downloading | Phase::Verifying | Phase::WaitingIdle)
}

/// True when a download update may be written at `now` after the one written at `last`.
fn due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|l| now.duration_since(l) >= DOWNLOAD_WRITE_EVERY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opendesk-progress-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn download_updates_are_limited_to_four_a_second() {
        let t = Instant::now();
        assert!(due(None, t));
        assert!(!due(Some(t), t + Duration::from_millis(249)));
        assert!(due(Some(t), t + Duration::from_millis(250)));
    }

    #[test]
    fn downloads_write_first_and_last_update_and_keep_the_schedule() {
        let dir = temp("download");
        let r = Reporter::load(dir.clone());
        r.next_check(1_000);
        r.checking(900);
        r.downloading("1.3.0", 0, 100);
        r.downloading("1.3.0", 50, 100);
        let p = Store::new(dir.clone()).progress().unwrap();
        assert_eq!((p.phase, p.bytes), (Phase::Downloading, 0), "second update within 250 ms is skipped");
        r.downloading("1.3.0", 100, 100);
        let p = Store::new(dir.clone()).progress().unwrap();
        assert_eq!((p.bytes, p.total, p.version.as_deref()), (100, 100, Some("1.3.0")));
        assert_eq!((p.checked_at, p.next_check_at), (Some(900), Some(1_000)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restart_turns_an_unfinished_phase_into_a_failure() {
        let dir = temp("restart");
        Reporter::load(dir.clone()).waiting_idle("1.3.0", 2, false);
        let r = Reporter::load(dir.clone());
        assert_eq!(r.phase(), Phase::Failed);
        r.set(Phase::Installing, Some("1.3.0".into()), None);
        assert_eq!(Reporter::load(dir.clone()).phase(), Phase::Installing, "installing is continued, not failed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn waiting_is_rewritten_only_on_change_or_refresh() {
        let dir = temp("waiting");
        let r = Reporter::load(dir.clone());
        r.waiting_idle("1.3.0", 1, false);
        let first = Store::new(dir.clone()).progress().unwrap().updated_at;
        std::thread::sleep(Duration::from_millis(5));
        r.waiting_idle("1.3.0", 1, false);
        assert_eq!(Store::new(dir.clone()).progress().unwrap().updated_at, first);
        r.waiting_idle("1.3.0", 1, true);
        assert!(Store::new(dir.clone()).progress().unwrap().updated_at > first);
        r.waiting_idle("1.3.0", 2, false);
        assert_eq!(Store::new(dir.clone()).progress().unwrap().sessions_active, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
