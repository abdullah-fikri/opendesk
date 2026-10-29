//! Health check after an update and the automatic rollback, as a pure state machine over
//! `pending_health.json`. The service feeds it what it observes on every poll (see `runner`).
//!
//! ```text
//! Installing{from,to} --new service runs `to`--> Verifying{from,to,deadline}
//!     | still `from` after INSTALL_TIMEOUT: InstallFailed (mark `to` bad)
//! Verifying --agent wrote `healthy` = to--> Confirmed (file removed)
//!     | deadline passed / CRASH_LIMIT early agent exits: Rollback (mark `to` bad, reinstall `from`)
//! RollingBack{target} --service runs `target`--> Done
//!     | not there after INSTALL_TIMEOUT: Done (logged, never looped)
//! ```

use serde::{Deserialize, Serialize};

use super::manifest::Version;

/// Time the new version has to report healthy, counted from the first agent start after the update (a
/// machine nobody is logged on to has no agent to check).
pub const HEALTH_WINDOW_SECS: u64 = 5 * 60;
/// An installer that has not produced the expected version after this long has failed.
pub const INSTALL_TIMEOUT_SECS: u64 = 15 * 60;
/// Agent exits right after start (`MIN_UPTIME` in the service) that count as a crash loop.
pub const CRASH_LIMIT: u32 = 3;

/// Contents of `pending_health.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Pending {
    /// The old service started the installer of `to` (unix seconds `started`).
    Installing { from: String, to: String, started: u64 },
    /// `to` runs; `deadline` is set once the service has started an agent.
    Verifying {
        from: String,
        to: String,
        #[serde(default)]
        deadline: Option<u64>,
        #[serde(default)]
        crashes: u32,
    },
    /// The installer of `target` was started (automatic rollback or `opendesk rollback`).
    RollingBack { target: String, started: u64 },
}

/// What the service saw on this poll.
pub struct Observed {
    pub current: Version,
    pub now: u64,
    /// The service has an agent running in a user session.
    pub agent_running: bool,
    /// Version written to the `healthy` marker by an agent, if any.
    pub healthy: Option<Version>,
    /// The agent exited right after start since the last poll.
    pub early_exit: bool,
}

#[derive(Debug, PartialEq)]
pub enum Step {
    Keep,
    Save(Pending),
    /// `to` is healthy: forget the pending check and keep its installer.
    Confirmed {
        to: Version,
    },
    /// Mark `bad` and reinstall `target` silently with the user data backup of `target`.
    Rollback {
        bad: Version,
        target: Version,
        reason: String,
    },
    /// The installer of `to` never completed: mark it bad.
    InstallFailed {
        to: Version,
    },
    /// Nothing more to do; the message is logged.
    Done(String),
}

pub fn step(pending: &Pending, o: &Observed) -> Step {
    match pending {
        Pending::Installing { from, to, started } => {
            let (Some(from_v), Some(to_v)) = (Version::parse(from), Version::parse(to)) else {
                return Step::Done("unreadable pending update dropped".into());
            };
            if o.current == to_v {
                Step::Save(Pending::Verifying { from: from.clone(), to: to.clone(), deadline: None, crashes: 0 })
            } else if o.current == from_v {
                if o.now > started + INSTALL_TIMEOUT_SECS { Step::InstallFailed { to: to_v } } else { Step::Keep }
            } else {
                Step::Done(format!("version {} runs instead of {to}; pending update dropped", o.current))
            }
        }
        Pending::Verifying { from, to, deadline, crashes } => {
            let (Some(from_v), Some(to_v)) = (Version::parse(from), Version::parse(to)) else {
                return Step::Done("unreadable pending update dropped".into());
            };
            if o.current != to_v {
                return Step::Done(format!("version {} runs instead of {to}; health check dropped", o.current));
            }
            if o.healthy == Some(to_v) {
                return Step::Confirmed { to: to_v };
            }
            let crashes = crashes + u32::from(o.early_exit);
            if crashes >= CRASH_LIMIT {
                return Step::Rollback { bad: to_v, target: from_v, reason: format!("agent crashed {crashes} times") };
            }
            match deadline {
                Some(d) if o.now > *d => Step::Rollback {
                    bad: to_v,
                    target: from_v,
                    reason: format!("not healthy within {HEALTH_WINDOW_SECS} s"),
                },
                None if o.agent_running => Step::Save(Pending::Verifying {
                    from: from.clone(),
                    to: to.clone(),
                    deadline: Some(o.now + HEALTH_WINDOW_SECS),
                    crashes,
                }),
                _ if o.early_exit => {
                    Step::Save(Pending::Verifying { from: from.clone(), to: to.clone(), deadline: *deadline, crashes })
                }
                _ => Step::Keep,
            }
        }
        Pending::RollingBack { target, started } => {
            if Version::parse(target) == Some(o.current) {
                Step::Done(format!("rolled back to {target}"))
            } else if o.now > started + INSTALL_TIMEOUT_SECS {
                Step::Done(format!("rollback to {target} did not complete; still running {}", o.current))
            } else {
                Step::Keep
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM: Version = Version(1, 0, 0);
    const TO: Version = Version(1, 1, 0);

    fn obs(current: Version, now: u64) -> Observed {
        Observed { current, now, agent_running: false, healthy: None, early_exit: false }
    }

    fn verifying(deadline: Option<u64>, crashes: u32) -> Pending {
        Pending::Verifying { from: "1.0.0".into(), to: "1.1.0".into(), deadline, crashes }
    }

    /// Drives the machine like the service does, applying `Save`; returns the first terminal step.
    fn run(mut p: Pending, mut observe: impl FnMut(u64) -> Observed) -> (Step, u64) {
        for t in (0..10_000).step_by(2) {
            match step(&p, &observe(t)) {
                Step::Keep => {}
                Step::Save(next) => p = next,
                done => return (done, t),
            }
        }
        panic!("no terminal step");
    }

    #[test]
    fn installing_moves_to_verifying_once_the_new_version_runs() {
        let p = Pending::Installing { from: "1.0.0".into(), to: "1.1.0".into(), started: 100 };
        assert_eq!(step(&p, &obs(TO, 130)), Step::Save(verifying(None, 0)));
        assert_eq!(step(&p, &obs(FROM, 130)), Step::Keep);
        assert_eq!(step(&p, &obs(FROM, 100 + INSTALL_TIMEOUT_SECS + 1)), Step::InstallFailed { to: TO });
        assert!(matches!(step(&p, &obs(Version(2, 0, 0), 130)), Step::Done(_)));
    }

    #[test]
    fn healthy_agent_confirms_the_update() {
        let (s, _) = run(verifying(None, 0), |t| Observed {
            agent_running: true,
            healthy: (t > 40).then_some(TO),
            ..obs(TO, t)
        });
        assert_eq!(s, Step::Confirmed { to: TO });
    }

    #[test]
    fn missing_health_report_rolls_back_after_the_window() {
        let (s, t) = run(verifying(None, 0), |t| Observed { agent_running: t >= 60, ..obs(TO, t) });
        assert!(matches!(s, Step::Rollback { bad: TO, target: FROM, .. }));
        // The window starts with the first agent, not with the update.
        assert!(t > 60 + HEALTH_WINDOW_SECS && t <= 62 + HEALTH_WINDOW_SECS + 2, "{t}");
    }

    #[test]
    fn no_logged_on_user_never_triggers_a_rollback() {
        let p = verifying(None, 0);
        assert_eq!(step(&p, &obs(TO, 1_000_000)), Step::Keep);
    }

    #[test]
    fn a_healthy_marker_of_another_version_does_not_count() {
        let o = Observed { agent_running: true, healthy: Some(FROM), ..obs(TO, 10_000) };
        assert!(matches!(step(&verifying(Some(9_000), 0), &o), Step::Rollback { .. }));
    }

    #[test]
    fn crash_loop_rolls_back_before_the_deadline() {
        let (s, t) =
            run(verifying(None, 0), |t| Observed { agent_running: true, early_exit: t % 20 == 10, ..obs(TO, t) });
        assert!(matches!(s, Step::Rollback { bad: TO, target: FROM, .. }), "{s:?}");
        assert!(t < HEALTH_WINDOW_SECS);
    }

    #[test]
    fn rolling_back_finishes_or_gives_up_without_looping() {
        let p = Pending::RollingBack { target: "1.0.0".into(), started: 0 };
        assert!(matches!(step(&p, &obs(FROM, 10)), Step::Done(_)));
        assert_eq!(step(&p, &obs(TO, 10)), Step::Keep);
        assert!(matches!(step(&p, &obs(TO, INSTALL_TIMEOUT_SECS + 1)), Step::Done(_)));
    }

    #[test]
    fn pending_file_format_is_stable() {
        let json = serde_json::to_string(&verifying(Some(5), 1)).unwrap();
        assert_eq!(json, r#"{"phase":"verifying","from":"1.0.0","to":"1.1.0","deadline":5,"crashes":1}"#);
        let old: Pending = serde_json::from_str(r#"{"phase":"verifying","from":"1.0.0","to":"1.1.0"}"#).unwrap();
        assert_eq!(old, verifying(None, 0));
    }
}
