//! Files of the update directory (`%ProgramData%\OpenDesk\updates`, SYSTEM and Administrators only):
//!
//! ```text
//! state.json              pin, versions marked bad, rollback floor
//! pending_health.json     see `health`
//! agent.json              heartbeat of the agent (idle, relay online, its settings)
//! healthy                 version of the agent that reached the relay after an update
//! requests\*.json         commands from `opendesk --rollback|--update-now|--pin-version|--unpin`
//! download\               installers being downloaded and verified
//! installed\<v>\setup.exe verified installer of each installed version (the newest KEEP_VERSIONS)
//! backup-<v>\             the user's config, history and identity files when leaving <v>
//! logs\                   installer logs
//! ```
//!
//! Every path is relative to a root passed in, so tests run in a temporary directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::health::Pending;
use super::manifest::Version;

/// Installed versions whose installer (and data backup) is kept for a rollback.
pub const KEEP_VERSIONS: usize = 2;
const INSTALLER: &str = "setup.exe";
const DIGEST: &str = "setup.json";

/// A kept installer; checked against `size` and `sha256` again before it runs.
#[derive(Debug, PartialEq)]
pub struct Kept {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

#[derive(Serialize, Deserialize)]
struct Digest {
    size: u64,
    sha256: String,
}
/// User data files copied before an update and restored by an automatic rollback (top level of the app
/// directory). Files starting with `device` or ending with `.key` (device identity) are included as well.
const USER_FILES: [&str; 2] = ["config.json", "history.json"];

/// `state.json`. New fields must be optional so older versions keep reading it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct State {
    pub pinned: Option<String>,
    pub bad: Vec<String>,
    /// `min_version` of the newest installed manifest: manual rollbacks below it need `--force`.
    pub min_version: Option<String>,
}

/// `agent.json`, rewritten by the agent every few seconds.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct AgentReport {
    pub version: String,
    pub desk_id: String,
    /// No hosted and no viewer session.
    pub idle: bool,
    pub online: bool,
    /// From the user's settings and environment.
    pub auto_update: bool,
    pub update_url: String,
    /// The user's `%APPDATA%\OpenDesk`, backed up before an update.
    pub app_dir: String,
    /// Unix seconds of the write.
    pub at: u64,
}

/// One command of `requests\`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Request {
    UpdateNow,
    Rollback {
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        force: bool,
    },
    Pin {
        version: String,
    },
    Unpin,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Store { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn download_dir(&self) -> PathBuf {
        self.root.join("download")
    }

    pub fn log_path(&self, version: Version) -> PathBuf {
        self.root.join("logs").join(format!("install-{version}.log"))
    }

    fn installed_dir(&self, version: Version) -> PathBuf {
        self.root.join("installed").join(version.to_string())
    }

    fn backup_dir(&self, version: Version) -> PathBuf {
        self.root.join(format!("backup-{version}"))
    }

    pub fn read<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        serde_json::from_slice(&fs::read(self.path(name)).ok()?).ok()
    }

    /// Writes `name` through a temporary file and a rename, so a reader never sees half a file.
    pub fn write<T: Serialize>(&self, name: &str, value: &T) -> io::Result<()> {
        let tmp = self.path(&format!("{name}.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(io::Error::other)?)?;
        fs::rename(&tmp, self.path(name))
    }

    pub fn remove(&self, name: &str) {
        let _ = fs::remove_file(self.path(name));
    }

    pub fn state(&self) -> State {
        self.read("state.json").unwrap_or_default()
    }

    pub fn save_state(&self, state: &State) -> io::Result<()> {
        self.write("state.json", state)
    }

    pub fn pending(&self) -> Option<Pending> {
        self.read("pending_health.json")
    }

    pub fn set_pending(&self, pending: Option<&Pending>) -> io::Result<()> {
        match pending {
            Some(p) => self.write("pending_health.json", p),
            None => {
                self.remove("pending_health.json");
                Ok(())
            }
        }
    }

    /// Version the agent wrote to `healthy`.
    pub fn healthy(&self) -> Option<Version> {
        Version::parse(&fs::read_to_string(self.path("healthy")).ok()?)
    }

    /// Removes and returns the queued requests, oldest first. Unreadable files are dropped.
    pub fn take_requests(&self) -> Vec<Request> {
        let Ok(dir) = fs::read_dir(self.root.join("requests")) else { return Vec::new() };
        let mut files: Vec<PathBuf> =
            dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        files
            .into_iter()
            .filter_map(|p| {
                let data = fs::read(&p).ok();
                let _ = fs::remove_file(&p);
                serde_json::from_slice(&data?).ok()
            })
            .collect()
    }

    /// Queues a request (the CLI side); fails without write access (not an administrator).
    pub fn push_request(&self, request: &Request, unique: u128) -> io::Result<()> {
        let dir = self.root.join("requests");
        fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("{unique:032}.tmp"));
        fs::write(&tmp, serde_json::to_vec(request).map_err(io::Error::other)?)?;
        fs::rename(&tmp, dir.join(format!("{unique:032}.json")))
    }

    /// The kept installer of `version` with the size and SHA-256 it was verified against.
    pub fn installer(&self, version: Version) -> Option<Kept> {
        let dir = self.installed_dir(version);
        let digest: Digest = serde_json::from_slice(&fs::read(dir.join(DIGEST)).ok()?).ok()?;
        let path = dir.join(INSTALLER);
        path.is_file().then_some(Kept { path, size: digest.size, sha256: digest.sha256 })
    }

    /// Moves a verified installer into `installed\<version>\` with its digest.
    pub fn keep_installer(&self, version: Version, verified: &Path, size: u64, sha256: &str) -> io::Result<PathBuf> {
        let dir = self.installed_dir(version);
        fs::create_dir_all(&dir)?;
        let dest = dir.join(INSTALLER);
        if fs::rename(verified, &dest).is_err() {
            fs::copy(verified, &dest)?;
            let _ = fs::remove_file(verified);
        }
        let digest = Digest { size, sha256: sha256.to_string() };
        fs::write(dir.join(DIGEST), serde_json::to_vec(&digest).map_err(io::Error::other)?)?;
        Ok(dest)
    }

    /// Versions with a kept installer, newest first.
    pub fn installed_versions(&self) -> Vec<Version> {
        let mut out: Vec<Version> = fs::read_dir(self.root.join("installed"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Version::parse(&e.file_name().to_string_lossy()))
            .filter(|v| self.installer(*v).is_some())
            .collect();
        out.sort_by(|a, b| b.cmp(a));
        out
    }

    /// Keeps the installers and backups of the newest [`KEEP_VERSIONS`] installed versions plus `also`.
    pub fn prune(&self, also: &[Version]) {
        let installed = self.installed_versions();
        let keep: Vec<Version> = installed.iter().take(KEEP_VERSIONS).chain(also).copied().collect();
        for v in installed.iter().filter(|v| !keep.contains(v)) {
            let _ = fs::remove_dir_all(self.installed_dir(*v));
        }
        for entry in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(v) = name.strip_prefix("backup-").and_then(Version::parse)
                && !keep.contains(&v)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        let _ = fs::remove_dir_all(self.download_dir());
    }

    /// Copies the user's data files from `app_dir` to `backup-<version>\` (replacing an older copy).
    pub fn backup_user_data(&self, app_dir: &Path, version: Version) -> io::Result<usize> {
        no_link(app_dir)?;
        let dest = self.backup_dir(version);
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir_all(&dest)?;
        let mut copied = 0;
        for entry in fs::read_dir(app_dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // `file_type` does not follow links: a link is never copied.
            if is_user_file(&name) && entry.file_type().is_ok_and(|t| t.is_file()) {
                fs::copy(entry.path(), dest.join(&name))?;
                copied += 1;
            }
        }
        Ok(copied)
    }

    /// Puts the files of `backup-<version>\` back into `app_dir`. Returns how many were restored.
    pub fn restore_user_data(&self, version: Version, app_dir: &Path) -> io::Result<usize> {
        no_link(app_dir)?;
        let mut restored = 0;
        for entry in fs::read_dir(self.backup_dir(version))?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let dest = app_dir.join(&name);
            if is_user_file(&name) && no_link(&dest).is_ok() {
                fs::copy(entry.path(), dest)?;
                restored += 1;
            }
        }
        Ok(restored)
    }
}

fn is_user_file(name: &str) -> bool {
    USER_FILES.contains(&name) || (name.starts_with("device") && name.ends_with(".json")) || name.ends_with(".key")
}

/// The app directory belongs to the user: refuse links there, so SYSTEM never copies through one.
fn no_link(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(io::Error::other(format!("{} is a link", path.display()))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("opendesk-update-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Store::new(dir)
    }

    fn fake_installer(store: &Store, v: Version) {
        let file = store.root().join(format!("dl-{v}"));
        fs::write(&file, v.to_string()).unwrap();
        store.keep_installer(v, &file, 5, "ab").unwrap();
    }

    #[test]
    fn state_round_trips_and_tolerates_unknown_fields() {
        let s = temp("state");
        assert_eq!(s.state(), State::default());
        let state = State { pinned: Some("1.0.0".into()), bad: vec!["1.1.0".into()], min_version: None };
        s.save_state(&state).unwrap();
        assert_eq!(s.state(), state);
        fs::write(s.path("state.json"), r#"{"bad":["2.0.0"],"from_the_future":1}"#).unwrap();
        assert_eq!(s.state().bad, vec!["2.0.0".to_string()]);
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn requests_are_taken_once_in_order() {
        let s = temp("req");
        s.push_request(&Request::Pin { version: "1.0.0".into() }, 2).unwrap();
        s.push_request(&Request::UpdateNow, 1).unwrap();
        fs::write(s.root().join("requests").join("3.json"), "garbage").unwrap();
        assert_eq!(s.take_requests(), vec![Request::UpdateNow, Request::Pin { version: "1.0.0".into() }]);
        assert!(s.take_requests().is_empty());
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn only_the_newest_installers_and_backups_are_kept() {
        let s = temp("prune");
        let app = s.root().join("app");
        fs::create_dir_all(&app).unwrap();
        for v in [Version(1, 0, 0), Version(1, 1, 0), Version(1, 2, 0)] {
            fake_installer(&s, v);
            s.backup_user_data(&app, v).unwrap();
        }
        assert_eq!(s.installed_versions(), vec![Version(1, 2, 0), Version(1, 1, 0), Version(1, 0, 0)]);
        s.prune(&[]);
        assert_eq!(s.installed_versions(), vec![Version(1, 2, 0), Version(1, 1, 0)]);
        assert!(!s.root().join("backup-1.0.0").exists());
        assert!(s.root().join("backup-1.1.0").exists());
        let kept = s.installer(Version(1, 1, 0)).unwrap();
        assert_eq!((kept.size, kept.sha256.as_str()), (5, "ab"));
        assert_eq!(fs::read_to_string(kept.path).unwrap(), "1.1.0");
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn user_data_backup_restores_config_history_and_identity_only() {
        let s = temp("backup");
        let app = s.root().join("app");
        fs::create_dir_all(&app).unwrap();
        for (name, body) in [("config.json", "old"), ("history.json", "h"), ("device_key.key", "k"), ("x.log", "l")] {
            fs::write(app.join(name), body).unwrap();
        }
        assert_eq!(s.backup_user_data(&app, Version(1, 0, 0)).unwrap(), 3);
        fs::write(app.join("config.json"), "broken by 1.1.0").unwrap();
        assert_eq!(s.restore_user_data(Version(1, 0, 0), &app).unwrap(), 3);
        assert_eq!(fs::read_to_string(app.join("config.json")).unwrap(), "old");
        assert!(!s.root().join("backup-1.0.0").join("x.log").exists());
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn healthy_marker_is_a_version() {
        let s = temp("healthy");
        assert_eq!(s.healthy(), None);
        fs::write(s.path("healthy"), "1.2.3\n").unwrap();
        assert_eq!(s.healthy(), Some(Version(1, 2, 3)));
        let _ = fs::remove_dir_all(s.root());
    }
}
