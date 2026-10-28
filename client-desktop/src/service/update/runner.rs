//! Service side (Windows, SYSTEM): checks for updates, installs them while no session is active, verifies
//! the new version and rolls back automatically or on request. Driven by the service's supervise loop.
//!
//! The installer stops this service (`sc stop`) and starts the new one, so every step that has to survive
//! the install is written to `pending_health.json` first and continued by the next service (see `health`).

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use super::fetch::{self, download_installer, fetch_manifest, latest_url, versioned_url};
use super::health::{self, INSTALL_TIMEOUT_SECS, Observed, Pending, Step};
use super::manifest::{Decision, Manifest, Version, decide, rollback_allowed};
use super::store::{AgentReport, Request, State, Store};
use super::{auto_update_env, now_unix, secure_dir, update_url_env};
use crate::domain::edition::Edition;
use crate::infra::logger;

/// First check after the service starts (the machine has just booted or been updated).
const FIRST_CHECK_SECS: u64 = 10 * 60;
const CHECK_EVERY_SECS: u64 = 6 * 3600;
const RETRY_SECS: u64 = 3600;
/// An agent report older than this is stale (the agent writes every 10 s).
const FRESH_SECS: u64 = 60;

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
    current: Version,
    next_check: u64,
    /// `opendesk --update-now`: ignore rollout and bad marks on the next check.
    forced: bool,
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
        Some(Updater {
            store: Store::new(root),
            current: Version::current(),
            next_check: now_unix() + FIRST_CHECK_SECS,
            forced: false,
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

        for request in self.store.take_requests() {
            self.handle(request, now, stop_agent);
        }

        if let Some(pending) = self.store.pending() {
            let observed = Observed {
                current: self.current,
                now,
                agent_running,
                healthy: self.store.healthy(),
                early_exit: std::mem::take(&mut self.early_exit),
            };
            self.advance(health::step(&pending, &observed), report.as_ref(), now, stop_agent);
            return;
        }
        self.early_exit = false;
        if self.holds_agent() {
            return;
        }

        self.collect_worker(now);
        let idle = fresh.map_or(!agent_running, |r| r.idle);
        if idle && let Some(ready) = self.ready.take() {
            self.install(ready, report.as_ref(), now, stop_agent);
            return;
        }

        if now >= self.next_check && self.worker.is_none() && self.ready.is_none() {
            // The agent knows the desk ID and the user's settings; without any report there is nothing to do.
            let Some(r) = report.as_ref() else { return };
            let enabled = self.forced || (auto_update_env() && r.auto_update);
            self.next_check = now + CHECK_EVERY_SECS;
            if enabled {
                self.start_check(r);
            }
        }
    }

    fn start_check(&mut self, report: &AgentReport) {
        let url = latest_url(&update_url_env().unwrap_or_else(|| report.update_url.clone()), Edition::current());
        let (tx, rx) = mpsc::channel();
        let root = self.store.root().to_path_buf();
        let (current, desk_id, state, forced) =
            (self.current, report.desk_id.clone(), self.store.state(), std::mem::take(&mut self.forced));
        thread::spawn(move || {
            let _ = tx.send(check(&Store::new(root), &url, current, &desk_id, &state, forced));
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
                match result {
                    Ok(ready) => self.ready = ready,
                    Err(e) => {
                        logger::warn("update", &format!("Update check failed: {e}"));
                        self.next_check = now + RETRY_SECS;
                    }
                }
            }
        }
    }

    fn handle(&mut self, request: Request, now: u64, stop_agent: &mut dyn FnMut()) {
        logger::info("update", &format!("Request: {request:?}"));
        let mut state = self.store.state();
        match request {
            Request::UpdateNow => {
                self.forced = true;
                self.next_check = now;
            }
            Request::Pin { version } => {
                state.pinned = Version::parse(&version).map(|v| v.to_string());
                self.save(&state);
            }
            Request::Unpin => {
                state.pinned = None;
                self.save(&state);
            }
            Request::Rollback { version, force } => {
                let target = match version.as_deref().map(Version::parse) {
                    Some(Some(v)) => Some(v),
                    Some(None) => None,
                    None => self.store.installed_versions().into_iter().find(|v| *v != self.current),
                };
                let Some(target) = target.filter(|t| *t != self.current) else {
                    logger::warn("update", "Rollback: no other installed version to go back to.");
                    return;
                };
                if !rollback_allowed(target, state.min_version.as_deref().and_then(Version::parse), force) {
                    logger::warn("update", &format!("Rollback to {target} refused: below min_version (use --force)."));
                    return;
                }
                if self.store.installer(target).is_none() {
                    logger::warn("update", &format!("Rollback: no kept installer of {target}."));
                    return;
                }
                // Not offered again automatically; `--update-now` still installs it.
                mark_bad(&mut state, self.current);
                self.save(&state);
                self.ready = None;
                self.run_installer(
                    target,
                    Pending::RollingBack { target: target.to_string(), started: now },
                    stop_agent,
                );
            }
        }
    }

    fn advance(&mut self, step: Step, report: Option<&AgentReport>, now: u64, stop_agent: &mut dyn FnMut()) {
        match step {
            Step::Keep => {}
            Step::Save(next) => {
                logger::info("update", &format!("Update state: {next:?}"));
                let _ = self.store.set_pending(Some(&next));
            }
            Step::Confirmed { to } => {
                logger::info("update", &format!("Version {to} is healthy; update complete."));
                let _ = self.store.set_pending(None);
                self.store.remove("healthy");
                self.store.prune(&[self.current]);
            }
            Step::InstallFailed { to } => {
                logger::error("update", &format!("The installer of {to} did not complete; {to} is marked bad."));
                let mut state = self.store.state();
                mark_bad(&mut state, to);
                self.save(&state);
                let _ = self.store.set_pending(None);
                self.installing_since = None;
            }
            Step::Rollback { bad, target, reason } => {
                logger::error(
                    "update",
                    &format!("Version {bad} failed its health check ({reason}); rolling back to {target}."),
                );
                let mut state = self.store.state();
                mark_bad(&mut state, bad);
                self.save(&state);
                if self.store.installer(target).is_none() {
                    logger::error("update", &format!("No kept installer of {target}; staying on {bad}."));
                    let _ = self.store.set_pending(None);
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
                    stop_agent,
                );
            }
            Step::Done(message) => {
                logger::info("update", &message);
                let _ = self.store.set_pending(None);
            }
        }
    }

    fn install(&mut self, ready: Ready, report: Option<&AgentReport>, now: u64, stop_agent: &mut dyn FnMut()) {
        let Ready { manifest, version, path } = ready;
        if let Some(app_dir) = report.map(|r| PathBuf::from(&r.app_dir)).filter(|d| d.is_dir())
            && let Err(e) = self.store.backup_user_data(&app_dir, self.current)
        {
            logger::warn("update", &format!("Update to {version} postponed: user files not backed up: {e}"));
            self.next_check = now + RETRY_SECS;
            return;
        }
        if let Err(e) = self.store.keep_installer(version, &path, manifest.size, &manifest.sha256) {
            logger::warn("update", &format!("Update to {version} postponed: {e}"));
            self.next_check = now + RETRY_SECS;
            return;
        }
        let mut state = self.store.state();
        state.min_version = Some(manifest.min_version.clone());
        self.save(&state);
        self.store.remove("healthy");
        let pending = Pending::Installing { from: self.current.to_string(), to: version.to_string(), started: now };
        logger::info("update", &format!("Installing {version} (from {}).", self.current));
        self.run_installer(version, pending, stop_agent);
    }

    /// Records `pending`, stops the agent and starts the kept installer of `version` silently.
    fn run_installer(&mut self, version: Version, pending: Pending, stop_agent: &mut dyn FnMut()) {
        let result = self
            .store
            .installer(version)
            .ok_or_else(|| format!("no kept installer of {version}"))
            .and_then(|kept| fetch::verify_file(&kept.path, kept.size, &kept.sha256).map(|()| kept.path))
            .and_then(|path| {
                self.store.set_pending(Some(&pending)).map_err(|e| e.to_string())?;
                stop_agent();
                launch(&path, &self.store.log_path(version))
            });
        match result {
            Ok(()) => self.installing_since = Some(now_unix()),
            Err(e) => {
                logger::error("update", &format!("Installer of {version} not started: {e}"));
                let _ = self.store.set_pending(None);
            }
        }
    }

    fn save(&self, state: &State) {
        if let Err(e) = self.store.save_state(state) {
            logger::warn("update", &format!("state.json not saved: {e}"));
        }
    }
}

fn mark_bad(state: &mut State, version: Version) {
    let v = version.to_string();
    if !state.bad.contains(&v) {
        state.bad.push(v);
    }
}

/// Worker thread: verified manifest -> decision -> installer of the running version kept for a rollback ->
/// verified download of the new installer.
fn check(
    store: &Store,
    url: &str,
    current: Version,
    desk_id: &str,
    state: &State,
    forced: bool,
) -> Result<Option<Ready>, String> {
    let manifest = fetch_manifest(url)?;
    let version = match decide(&manifest, current, desk_id, &state.bad, state.pinned.as_deref(), forced) {
        Decision::Install(v) => v,
        other => {
            logger::info("update", &format!("No update ({other:?}, manifest {}).", manifest.version));
            return Ok(None);
        }
    };
    // Never update without a way back.
    if store.installer(current).is_none() {
        let url = versioned_url(url, current).ok_or("no rollback installer and no versioned manifest URL")?;
        let own = fetch_manifest(&url)?;
        if own.version() != Some(current) {
            return Err(format!("{url} describes {}, not {current}", own.version));
        }
        let path = download_installer(&own, &store.download_dir())?;
        store.keep_installer(current, &path, own.size, &own.sha256).map_err(|e| e.to_string())?;
    }
    let path = download_installer(&manifest, &store.download_dir())?;
    logger::info("update", &format!("Version {version} downloaded and verified."));
    Ok(Some(Ready { manifest, version, path }))
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
