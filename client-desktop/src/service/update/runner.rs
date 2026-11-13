//! Service side (Windows, SYSTEM): checks for updates, installs them while no session is active, verifies
//! the new version and rolls back automatically or on request. Driven by the service's supervise loop.
//!
//! The installer stops this service (`sc stop`) and starts the new one, so every step that has to survive
//! the install is written to `pending_health.json` first and continued by the next service (see `health`).
//! Every phase is also reported in `progress.json` (see `progress`) for `opendesk update` to show.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use super::fetch::{self, download_installer, fetch_manifest, latest_url, versioned_url};
use super::health::{self, INSTALL_TIMEOUT_SECS, Observed, Pending, Step};
use super::manifest::{Decision, Manifest, Version, decide, decide_target, rollback_allowed};
use super::progress::Reporter;
use super::store::{AgentReport, Phase, Request, State, Store};
use super::{auto_update_env, now_unix, secure_dir, update_url_env};
use crate::domain::edition::Edition;
use crate::infra::logger;

/// First check after the service starts (the machine has just booted or been updated).
const FIRST_CHECK_SECS: u64 = 10 * 60;
const CHECK_EVERY_SECS: u64 = 6 * 3600;
const RETRY_SECS: u64 = 3600;
/// An agent report older than this is stale (the agent writes every 10 s).
const FRESH_SECS: u64 = 60;
/// Pause between reporting `installing` and starting the installer: a watching `opendesk update` (which is
/// `OpenDesk.exe`, killed by the installer) polls every 250 ms and exits on that phase, so it holds no file the
/// installer replaces.
const CLI_EXIT_GRACE: Duration = Duration::from_secs(2);

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// A verified installer of a newer version, waiting for an idle moment.
struct Ready {
    manifest: Manifest,
    version: Version,
    path: PathBuf,
}

pub struct Updater {
    store: Store,
    progress: Reporter,
    current: Version,
    next_check: u64,
    /// `opendesk update`: ignore rollout and bad marks on the next check.
    forced: bool,
    /// `opendesk update <version> [--force]`: the next check fetches that release's manifest instead of the latest.
    target: Option<(Version, bool)>,
    /// `opendesk update --now`: install the update the next check finds without waiting for sessions to end.
    install_now: bool,
    worker: Option<Receiver<Result<Option<Ready>, String>>>,
    ready: Option<Ready>,
    early_exit: bool,
    /// An installer was started by this process: keep the agent stopped until it stops the service.
    installing_since: Option<u64>,
}

impl Updater {
    /// None when the update directory cannot be secured (logged); the service then runs without updates.
    pub fn new() -> Option<Updater> {
        let root = secure_dir::root();
        if let Err(e) = secure_dir::ensure(&root) {
            logger::warn("update", &format!("Auto-update off: {} cannot be secured: {e}", root.display()));
            return None;
        }
        let progress = Reporter::load(root.clone());
        let next_check = now_unix() + FIRST_CHECK_SECS;
        progress.next_check(next_check);
        Some(Updater {
            store: Store::new(root),
            progress,
            current: Version::current(),
            next_check,
            forced: false,
            target: None,
            install_now: false,
            worker: None,
            ready: None,
            early_exit: false,
            installing_since: None,
        })
    }

    /// The agent exited right after it was started (crash-loop detection after an update).
    pub fn agent_exited_early(&mut self) {
        self.early_exit = true;
    }

    /// True while an installer runs: the service must not start the agent.
    pub fn holds_agent(&self) -> bool {
        self.installing_since.is_some_and(|t| now_unix() < t + INSTALL_TIMEOUT_SECS)
    }

    /// One poll of the service loop. `stop_agent` ends the agent synchronously (before a rollback restores
    /// the user's files and before an installer runs).
    pub fn poll(&mut self, agent_running: bool, stop_agent: &mut dyn FnMut()) {
        let now = now_unix();
        let report: Option<AgentReport> = self.store.read("agent.json");
        let fresh = report.as_ref().filter(|r| now.saturating_sub(r.at) < FRESH_SECS);

        let requests = self.store.take_requests();
        let took_requests = !requests.is_empty();
        for request in requests {
            self.handle(request, now, stop_agent);
        }
        if took_requests {
            self.progress.touch();
        }

        if let Some(pending) = self.store.pending() {
            let observed = Observed {
                current: self.current,
                now,
                agent_running,
                healthy: self.store.healthy(),
                early_exit: std::mem::take(&mut self.early_exit),
            };
            self.advance(&pending, health::step(&pending, &observed), report.as_ref(), now, stop_agent);
            return;
        }
        self.early_exit = false;
        if self.holds_agent() {
            return;
        }

        self.collect_worker(now);
        if may_install(fresh, agent_running, self.install_now)
            && let Some(ready) = self.ready.take()
        {
            if self.install_now {
                logger::warn("update", "Installing now as requested (--now); active sessions are ended.");
            }
            self.install_now = false;
            self.install(ready, report.as_ref(), now, stop_agent);
            return;
        }
        if let Some(ready) = &self.ready {
            // An agent before 1.3.0 reports only `idle`: at least one session is active.
            let sessions = fresh.map_or(1, |r| r.sessions.max(1));
            self.progress.waiting_idle(&ready.version.to_string(), sessions, took_requests);
        }

        if now >= self.next_check && self.worker.is_none() && self.ready.is_none() {
            // The agent knows the desk ID and the user's settings; without any report there is nothing to do.
            let Some(r) = report.as_ref() else { return };
            let enabled = self.forced || (auto_update_env() && r.auto_update);
            self.schedule(now + CHECK_EVERY_SECS);
            if enabled {
                self.start_check(r, now);
            }
        }
    }

    /// Sets the time of the next check and reports it.
    fn schedule(&mut self, at: u64) {
        self.next_check = at;
        self.progress.next_check(at);
    }

    /// Logs `message` and reports the update as failed.
    fn fail(&self, message: String) {
        logger::warn("update", &message);
        self.progress.set(Phase::Failed, None, Some(message));
    }

    fn start_check(&mut self, report: &AgentReport, now: u64) {
        let url = latest_url(&update_url_env().unwrap_or_else(|| report.update_url.clone()), Edition::current());
        let (tx, rx) = mpsc::channel();
        let root = self.store.root().to_path_buf();
        let (current, desk_id, state, forced, target) = (
            self.current,
            report.desk_id.clone(),
            self.store.state(),
            std::mem::take(&mut self.forced),
            self.target.take(),
        );
        self.progress.checking(now);
        let progress = self.progress.clone();
        crate::infra::thread::spawn_named("update-check", move || {
            let job = Check { store: &Store::new(root), progress: &progress, current, forced, target };
            let _ = tx.send(job.run(&url, &desk_id, &state));
        });
        self.worker = Some(rx);
    }

    fn collect_worker(&mut self, now: u64) {
        let Some(rx) = &self.worker else { return };
        match rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.worker = None,
            Ok(result) => {
                self.worker = None;
                // `--now` applies to the update this check found, not to a later automatic one.
                if !matches!(result, Ok(Some(_))) {
                    self.install_now = false;
                }
                match result {
                    Ok(ready) => self.ready = ready,
                    Err(e) => {
                        self.fail(format!("Update check failed: {e}"));
                        self.schedule(now + RETRY_SECS);
                    }
                }
            }
        }
    }

    fn handle(&mut self, request: Request, now: u64, stop_agent: &mut dyn FnMut()) {
        logger::info("update", &format!("Request: {request:?}"));
        let mut state = self.store.state();
        match request {
            Request::UpdateNow { now: install_now } => {
                self.forced = true;
                self.install_now = install_now;
                self.schedule(now);
            }
            Request::Pin { version } => {
                state.pinned = Version::parse(&version).map(|v| v.to_string());
                self.save(&state);
            }
            Request::Unpin => {
                state.pinned = None;
                self.save(&state);
            }
            Request::Install { version, force, now: install_now } => {
                let Some(target) = Version::parse(&version) else { return };
                // Stay on the chosen release: automatic checks offer nothing else until `opendesk update latest`.
                state.pinned = Some(target.to_string());
                self.save(&state);
                if target == self.current {
                    logger::info("update", &format!("Already on {target}; pinned to it."));
                    self.progress.set(Phase::UpToDate, Some(target.to_string()), None);
                    return;
                }
                self.ready = None;
                self.forced = true;
                self.install_now = install_now;
                self.target = Some((target, force));
                self.schedule(now);
            }
            Request::Rollback { version, force } => {
                let target = match version.as_deref().map(Version::parse) {
                    Some(Some(v)) => Some(v),
                    Some(None) => None,
                    None => self.store.installed_versions().into_iter().find(|v| *v != self.current),
                };
                let Some(target) = target.filter(|t| *t != self.current) else {
                    return self.fail("Rollback: no other installed version to go back to.".into());
                };
                if !rollback_allowed(target, state.min_version.as_deref().and_then(Version::parse), force) {
                    return self.fail(format!("Rollback to {target} refused: below min_version (use --force)."));
                }
                if self.store.installer(target).is_none() {
                    return self.fail(format!("Rollback: no kept installer of {target}."));
                }
                // Not offered again automatically; `opendesk update` still installs it.
                mark_bad(&mut state, self.current);
                self.save(&state);
                self.ready = None;
                self.run_installer(
                    target,
                    Pending::RollingBack { target: target.to_string(), started: now },
                    None,
                    stop_agent,
                );
            }
        }
    }

    fn advance(
        &mut self,
        pending: &Pending,
        step: Step,
        report: Option<&AgentReport>,
        now: u64,
        stop_agent: &mut dyn FnMut(),
    ) {
        match step {
            Step::Keep => {}
            Step::Save(next) => {
                logger::info("update", &format!("Update state: {next:?}"));
                let _ = self.store.set_pending(Some(&next));
                if let Pending::Verifying { to, .. } = &next
                    && self.progress.phase() != Phase::Health
                {
                    self.progress.set(Phase::Health, Some(to.clone()), None);
                }
            }
            Step::Confirmed { to } => {
                logger::info("update", &format!("Version {to} is healthy; update complete."));
                let _ = self.store.set_pending(None);
                self.store.remove("healthy");
                self.store.prune(&[self.current]);
                self.progress.set(Phase::Done, Some(to.to_string()), None);
            }
            Step::InstallFailed { to } => {
                let message = format!("The installer of {to} did not complete; {to} is marked bad.");
                logger::error("update", &message);
                let mut state = self.store.state();
                mark_bad(&mut state, to);
                self.save(&state);
                let _ = self.store.set_pending(None);
                self.installing_since = None;
                self.progress.set(Phase::Failed, Some(to.to_string()), Some(message));
            }
            Step::Rollback { bad, target, reason } => {
                let message = format!("Version {bad} failed its health check ({reason}); rolling back to {target}.");
                logger::error("update", &message);
                let mut state = self.store.state();
                mark_bad(&mut state, bad);
                self.save(&state);
                if self.store.installer(target).is_none() {
                    logger::error("update", &format!("No kept installer of {target}; staying on {bad}."));
                    let _ = self.store.set_pending(None);
                    let message = format!("{message} No kept installer of {target}; staying on {bad}.");
                    self.progress.set(Phase::Failed, Some(bad.to_string()), Some(message));
                    return;
                }
                stop_agent();
                thread::sleep(Duration::from_secs(1));
                if let Some(app_dir) = report.map(|r| PathBuf::from(&r.app_dir)).filter(|d| d.is_dir()) {
                    match self.store.restore_user_data(target, &app_dir) {
                        Ok(n) => logger::info("update", &format!("Restored {n} user files of {target}.")),
                        Err(e) => logger::warn("update", &format!("User files of {target} not restored: {e}")),
                    }
                }
                self.run_installer(
                    target,
                    Pending::RollingBack { target: target.to_string(), started: now },
                    Some(message),
                    stop_agent,
                );
            }
            Step::Done(message) => {
                logger::info("update", &message);
                let _ = self.store.set_pending(None);
                let rolled_back = matches!(pending, Pending::RollingBack { target, .. } if Version::parse(target) == Some(self.current));
                let version = Some(self.current.to_string());
                if rolled_back {
                    // Keeps the reason of an automatic rollback, reported with `installing`.
                    let reason = self.store.progress().and_then(|p| p.error);
                    self.progress.set(Phase::Done, version, reason);
                } else {
                    self.progress.set(Phase::Failed, version, Some(message));
                }
            }
        }
    }

    fn install(&mut self, ready: Ready, report: Option<&AgentReport>, now: u64, stop_agent: &mut dyn FnMut()) {
        let Ready { manifest, version, path } = ready;
        if let Some(app_dir) = report.map(|r| PathBuf::from(&r.app_dir)).filter(|d| d.is_dir())
            && let Err(e) = self.store.backup_user_data(&app_dir, self.current)
        {
            self.fail(format!("Update to {version} postponed by an hour: user files not backed up: {e}"));
            self.schedule(now + RETRY_SECS);
            return;
        }
        if let Err(e) = self.store.keep_installer(version, &path, manifest.size, &manifest.sha256) {
            self.fail(format!("Update to {version} postponed by an hour: {e}"));
            self.schedule(now + RETRY_SECS);
            return;
        }
        let mut state = self.store.state();
        state.min_version = Some(manifest.min_version.clone());
        self.save(&state);
        self.store.remove("healthy");
        let pending = Pending::Installing { from: self.current.to_string(), to: version.to_string(), started: now };
        logger::info("update", &format!("Installing {version} (from {}).", self.current));
        self.run_installer(version, pending, None, stop_agent);
    }

    /// Records `pending`, stops the agent and starts the kept installer of `version` silently. `note` is
    /// reported with `installing` (the reason of an automatic rollback).
    fn run_installer(
        &mut self,
        version: Version,
        pending: Pending,
        note: Option<String>,
        stop_agent: &mut dyn FnMut(),
    ) {
        let result = self
            .store
            .installer(version)
            .ok_or_else(|| format!("no kept installer of {version}"))
            .and_then(|kept| fetch::verify_file(&kept.path, kept.size, &kept.sha256).map(|()| kept.path))
            .and_then(|path| {
                self.store.set_pending(Some(&pending)).map_err(|e| e.to_string())?;
                stop_agent();
                self.progress.set(Phase::Installing, Some(version.to_string()), note);
                thread::sleep(CLI_EXIT_GRACE);
                launch(&path, &self.store.log_path(version))
            });
        match result {
            Ok(()) => self.installing_since = Some(now_unix()),
            Err(e) => {
                logger::error("update", &format!("Installer of {version} not started: {e}"));
                let _ = self.store.set_pending(None);
                let error = format!("The installer of {version} did not start: {e}");
                self.progress.set(Phase::Failed, Some(version.to_string()), Some(error));
            }
        }
    }

    fn save(&self, state: &State) {
        if let Err(e) = self.store.save_state(state) {
            logger::warn("update", &format!("state.json not saved: {e}"));
        }
    }
}

/// Whether a verified installer may run now: no session is active (per a `fresh` agent report, or no agent
/// at all), or `opendesk update --now` asked to install even while sessions are active.
fn may_install(fresh: Option<&AgentReport>, agent_running: bool, install_now: bool) -> bool {
    install_now || fresh.map_or(!agent_running, |r| r.idle)
}

fn mark_bad(state: &mut State, version: Version) {
    let v = version.to_string();
    if !state.bad.contains(&v) {
        state.bad.push(v);
    }
}

/// One check on the worker thread.
struct Check<'a> {
    store: &'a Store,
    progress: &'a Reporter,
    current: Version,
    forced: bool,
    target: Option<(Version, bool)>,
}

impl Check<'_> {
    /// Verified manifest -> decision -> installer of the running version kept for a rollback -> verified
    /// download of the new installer.
    fn run(&self, url: &str, desk_id: &str, state: &State) -> Result<Option<Ready>, String> {
        let current = self.current;
        let (decision, manifest) = match self.target {
            Some((version, force)) => {
                let url = versioned_url(url, version).ok_or("the update URL has no per-release layout")?;
                let manifest = fetch_manifest(&url)?;
                let floor = state.min_version.as_deref().and_then(Version::parse);
                (decide_target(&manifest, current, version, floor, force), manifest)
            }
            None => {
                let manifest = fetch_manifest(url)?;
                (decide(&manifest, current, desk_id, &state.bad, state.pinned.as_deref(), self.forced), manifest)
            }
        };
        let version = match decision {
            Decision::Install(v) => v,
            Decision::BelowFloor => {
                return Err(format!(
                    "{} is below the minimum version {} of the installed release (add --force)",
                    manifest.version,
                    state.min_version.as_deref().unwrap_or("?")
                ));
            }
            Decision::WrongRelease => {
                return Err(format!("the manifest of that release describes version {}", manifest.version));
            }
            other => {
                logger::info("update", &format!("No update ({other:?}, manifest {}).", manifest.version));
                self.progress.set(Phase::UpToDate, Some(manifest.version.clone()), None);
                return Ok(None);
            }
        };
        let shown = version.to_string();
        // Never update without a way back.
        let rollback_copy = match self.store.installer(current) {
            Some(_) => None,
            None => {
                let url = versioned_url(url, current).ok_or("no rollback installer and no versioned manifest URL")?;
                let own = fetch_manifest(&url)?;
                if own.version() != Some(current) {
                    return Err(format!("{url} describes {}, not {current}", own.version));
                }
                Some(own)
            }
        };
        let before = rollback_copy.as_ref().map_or(0, |own| own.size);
        let total = before + manifest.size;
        if let Some(own) = rollback_copy {
            let path = download_installer(&own, &self.store.download_dir(), &mut |n| {
                self.progress.downloading(&shown, n, total)
            })?;
            self.store.keep_installer(current, &path, own.size, &own.sha256).map_err(|e| e.to_string())?;
        }
        let path = download_installer(&manifest, &self.store.download_dir(), &mut |n| {
            self.progress.downloading(&shown, before + n, total)
        })?;
        logger::info("update", &format!("Version {version} downloaded and verified."));
        self.progress.set(Phase::Verifying, Some(shown), None);
        Ok(Some(Ready { manifest, version, path }))
    }
}

/// Starts the installer detached from this service, which it is about to stop and replace.
fn launch(installer: &Path, log: &Path) -> Result<(), String> {
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    Command::new(installer)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .arg(format!("/LOG={}", log.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(idle: bool, sessions: u32) -> AgentReport {
        AgentReport { idle, sessions, ..AgentReport::default() }
    }

    #[test]
    fn install_waits_for_idle_unless_now_was_requested() {
        let busy = report(false, 2);
        let idle = report(true, 0);
        assert!(!may_install(Some(&busy), true, false), "active sessions: wait");
        assert!(may_install(Some(&busy), true, true), "--now skips waiting_idle");
        assert!(may_install(Some(&idle), true, false));
        assert!(!may_install(None, true, false), "running agent without a fresh report: wait");
        assert!(may_install(None, true, true));
        assert!(may_install(None, false, false), "no agent, no sessions");
    }
}
