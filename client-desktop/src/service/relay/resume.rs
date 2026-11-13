//! Session resume (see `docs/features/session-resume.md` at the repository root): when the relay connection
//! drops, sessions whose partner and relay list `session-resume` wait up to 30 s for a new connection to take
//! them over with the resume token instead of ending.

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How long sessions wait for the relay connection to come back.
pub const GRACE: Duration = Duration::from_secs(30);
/// Status of a session the relay did not give back.
pub const EXPIRED: &str = "The connection was lost for too long.";
/// First reconnect attempt while sessions wait; later ones back off up to `MAX_DELAY`.
const FIRST_DELAY: Duration = Duration::from_millis(300);
const MAX_DELAY: Duration = Duration::from_millis(2000);

/// Whether this client lists `session-resume`: on unless `OPENDESK_SESSION_RESUME=0`.
pub fn enabled() -> bool {
    enabled_by(std::env::var("OPENDESK_SESSION_RESUME").ok().as_deref())
}

fn enabled_by(env: Option<&str>) -> bool {
    env.map(str::trim) != Some("0")
}

/// Resume state of the connection thread.
#[derive(Debug, Default)]
pub struct Resume {
    /// The token from the last `registered-ack`, for the relay URL that issued it.
    token: Option<(String, String)>,
    /// Partners whose `session-connected.peerCaps` listed `session-resume`.
    capable: HashSet<String>,
    /// Sessions waiting for the connection, and until when.
    waiting: Vec<String>,
    deadline: Option<Instant>,
    attempts: u32,
    /// The countdown last shown (see `countdown`).
    shown: Option<u64>,
}

impl Resume {
    /// Stores the token of a fresh registration (None when the relay does not list the capability).
    pub fn registered(&mut self, url: &str, token: Option<String>) {
        self.token = token.filter(|t| !t.is_empty()).map(|t| (url.to_string(), t));
    }

    /// The token to present to `url`, only while sessions wait for it.
    pub fn token_for(&self, url: &str) -> Option<String> {
        if self.waiting.is_empty() {
            return None;
        }
        self.token.as_ref().filter(|(u, _)| u == url).map(|(_, t)| t.clone())
    }

    /// A session started; `capable` is whether its partner listed `session-resume`.
    pub fn session_started(&mut self, partner: &str, capable: bool) {
        if capable {
            self.capable.insert(partner.to_string());
        } else {
            self.capable.remove(partner);
        }
    }

    /// Whether the session with `partner` may wait for a reconnect.
    pub fn can_wait(&self, partner: &str) -> bool {
        self.token.is_some() && self.capable.contains(partner)
    }

    /// The connection dropped at `now`: `sessions` wait until the grace is over. A later drop never extends an
    /// earlier deadline.
    pub fn suspend(&mut self, sessions: Vec<String>, now: Instant) {
        self.waiting = sessions;
        if self.waiting.is_empty() {
            self.deadline = None;
        } else if self.deadline.is_none() {
            self.deadline = Some(now + GRACE);
        }
        self.attempts = 0;
    }

    pub fn is_waiting(&self, partner: &str) -> bool {
        self.waiting.iter().any(|p| p == partner)
    }

    pub fn any_waiting(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// Seconds left of the grace, for "Reconnecting… (N s)".
    pub fn seconds_left(&self, now: Instant) -> u64 {
        self.deadline.map_or(0, |d| d.saturating_duration_since(now).as_secs())
    }

    /// The "Reconnecting… (N s)" countdown at `now` when it differs from the one last shown: N while sessions
    /// wait, 0 (hide it) once none does.
    pub fn countdown(&mut self, now: Instant) -> Option<u64> {
        let secs = if self.any_waiting() { self.seconds_left(now).max(1) } else { 0 };
        (self.shown != Some(secs)).then(|| *self.shown.insert(secs))
    }

    /// The grace is over at `now`: returns the sessions that must end.
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        match self.deadline {
            Some(d) if now >= d => self.finish(),
            _ => Vec::new(),
        }
    }

    /// The relay answered the registration: `resumed` are the partners whose sessions continue. Returns the
    /// waiting sessions it did not give back, which must end.
    pub fn resumed(&mut self, resumed: &[String]) -> Vec<String> {
        let mut lost = self.finish();
        lost.retain(|p| !resumed.contains(p));
        lost
    }

    /// A session ended: it no longer waits.
    pub fn ended(&mut self, partner: &str) {
        self.capable.remove(partner);
        self.waiting.retain(|p| p != partner);
        if self.waiting.is_empty() {
            self.deadline = None;
        }
    }

    fn finish(&mut self) -> Vec<String> {
        self.deadline = None;
        std::mem::take(&mut self.waiting)
    }

    /// Pause before the next reconnect attempt: short while sessions wait, `normal` otherwise.
    pub fn next_delay(&mut self, normal: Duration) -> Duration {
        if self.waiting.is_empty() {
            return normal;
        }
        let delay = FIRST_DELAY * 2u32.saturating_pow(self.attempts.min(4));
        self.attempts += 1;
        delay.min(MAX_DELAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn switch_turns_it_off() {
        assert!(enabled_by(None) && enabled_by(Some("1")));
        assert!(!enabled_by(Some("0")) && !enabled_by(Some(" 0 ")));
    }

    #[test]
    fn only_capable_partners_with_a_token_wait() {
        let mut r = Resume::default();
        r.session_started("1", true);
        r.session_started("2", false);
        assert!(!r.can_wait("1"), "no token from the relay");
        r.registered("wss://a", Some("t".into()));
        assert!(r.can_wait("1") && !r.can_wait("2"));
        r.registered("wss://a", None);
        assert!(!r.can_wait("1"), "a relay without the capability");
    }

    #[test]
    fn token_only_for_the_issuing_relay_while_waiting() {
        let mut r = Resume::default();
        r.registered("wss://a", Some("t".into()));
        assert_eq!(r.token_for("wss://a"), None);
        r.suspend(ids(&["1"]), Instant::now());
        assert_eq!(r.token_for("wss://a").as_deref(), Some("t"));
        assert_eq!(r.token_for("wss://b"), None);
    }

    #[test]
    fn grace_expires_and_is_never_extended() {
        let mut r = Resume::default();
        let t0 = Instant::now();
        r.suspend(ids(&["1", "2"]), t0);
        assert_eq!(r.seconds_left(t0), 30);
        r.suspend(ids(&["1", "2"]), t0 + Duration::from_secs(10));
        assert!(r.expire(t0 + Duration::from_secs(29)).is_empty());
        assert_eq!(r.expire(t0 + GRACE), ids(&["1", "2"]));
        assert!(!r.any_waiting());
    }

    #[test]
    fn sessions_not_given_back_end() {
        let mut r = Resume::default();
        r.suspend(ids(&["1", "2"]), Instant::now());
        assert_eq!(r.resumed(&ids(&["2"])), ids(&["1"]));
        assert!(!r.any_waiting());
        r.suspend(ids(&["3"]), Instant::now());
        r.ended("3");
        assert!(!r.any_waiting() && r.expire(Instant::now() + GRACE).is_empty());
    }

    #[test]
    fn countdown_changes_once_per_second_and_clears() {
        let mut r = Resume::default();
        let t0 = Instant::now();
        r.suspend(ids(&["1"]), t0);
        assert_eq!(r.countdown(t0), Some(30));
        assert_eq!(r.countdown(t0 + Duration::from_millis(500)), Some(29));
        assert_eq!(r.countdown(t0 + Duration::from_millis(900)), None, "same second");
        assert_eq!(r.countdown(t0 + Duration::from_millis(29_500)), Some(1));
        r.resumed(&ids(&["1"]));
        assert_eq!(r.countdown(t0 + Duration::from_secs(29)), Some(0));
        assert_eq!(r.countdown(t0 + Duration::from_secs(29)), None);
    }

    #[test]
    fn reconnects_fast_while_waiting() {
        let mut r = Resume::default();
        let normal = Duration::from_millis(1500);
        assert_eq!(r.next_delay(normal), normal);
        r.suspend(ids(&["1"]), Instant::now());
        assert_eq!(r.next_delay(normal), Duration::from_millis(300));
        assert_eq!(r.next_delay(normal), Duration::from_millis(600));
        for _ in 0..5 {
            r.next_delay(normal);
        }
        assert_eq!(r.next_delay(normal), MAX_DELAY);
    }
}
