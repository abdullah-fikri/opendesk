//! Delay-based pacing of file transfers (LEDBAT style), so a large file never fills the uplink queue that the
//! session's video, input and P2P keep-alives share. The relay connection measures its round trip with
//! WebSocket pings that queue behind the file bytes; when that round trip grows more than [`TARGET_DELAY`]
//! above the lowest one seen recently, the file rate halves, and while it stays below the rate grows a little
//! per sample. The rate never drops under [`FLOOR_RATE`], so a transfer always progresses.
//!
//! Pure logic: callers pass round trips and the time.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Queuing delay the file transfer may add on top of the base round trip.
pub const TARGET_DELAY: Duration = Duration::from_millis(25);
/// Slowest file rate, bytes per second: transfers always move.
pub const FLOOR_RATE: u64 = 64 * 1024;
/// Rate a transfer starts at before the first measurements, bytes per second.
pub const START_RATE: u64 = 1024 * 1024;
/// Fastest file rate, bytes per second.
pub const MAX_RATE: u64 = 256 * 1024 * 1024;
/// Most growth per sample, as a fraction of the rate, when the queue is empty.
const GAIN: f64 = 0.1;
/// The base round trip is the lowest of this many buckets...
const BASE_BUCKETS: usize = 5;
/// ...each this long, so a route change is learned within a few minutes.
const BASE_BUCKET: Duration = Duration::from_secs(60);
/// The current round trip is the lowest of the last few samples, which filters out single delayed pongs.
const FILTER: usize = 3;
/// Bounds of the pause between two decreases (one base round trip).
const MIN_DECREASE_GAP: Duration = Duration::from_millis(100);
const MAX_DECREASE_GAP: Duration = Duration::from_secs(1);
/// Burst a token bucket may save up, as time at the current rate.
const BURST: Duration = Duration::from_millis(20);

/// The file-rate controller of one relay connection.
pub struct FileRate {
    /// Bytes per second.
    rate: f64,
    base: VecDeque<Duration>,
    bucket_start: Option<Instant>,
    recent: VecDeque<Duration>,
    last_decrease: Option<Instant>,
}

impl Default for FileRate {
    fn default() -> Self {
        Self::new()
    }
}

impl FileRate {
    /// A controller at [`START_RATE`] with no measurements.
    pub fn new() -> Self {
        Self {
            rate: START_RATE as f64,
            base: VecDeque::new(),
            bucket_start: None,
            recent: VecDeque::new(),
            last_decrease: None,
        }
    }

    /// Current file rate, bytes per second.
    pub fn rate(&self) -> u64 {
        self.rate as u64
    }

    /// Lowest round trip seen recently.
    pub fn base_rtt(&self) -> Option<Duration> {
        self.base.iter().min().copied()
    }

    /// Delay the queue adds now: filtered round trip minus the base.
    pub fn queuing_delay(&self) -> Option<Duration> {
        Some(self.recent.iter().min()?.saturating_sub(self.base_rtt()?))
    }

    /// One round-trip sample. `limited` says file messages waited for the rate since the previous sample;
    /// only then may it grow (a sender held back by its disk proves nothing about the link).
    pub fn on_rtt(&mut self, rtt: Duration, limited: bool, now: Instant) {
        self.update_base(rtt, now);
        self.recent.push_back(rtt);
        if self.recent.len() > FILTER {
            self.recent.pop_front();
        }
        let Some(queued) = self.queuing_delay() else { return };
        if queued > TARGET_DELAY {
            self.decrease(now);
        } else if limited {
            let off_target = (TARGET_DELAY - queued).as_secs_f64() / TARGET_DELAY.as_secs_f64();
            self.rate = (self.rate * (1.0 + GAIN * off_target)).min(MAX_RATE as f64);
        }
    }

    /// The probe sent `age` ago has not come back: the round trip is at least that, which already says the
    /// queue is too long without waiting for the answer (a bloated queue may hold it for seconds).
    pub fn on_waiting(&mut self, age: Duration, now: Instant) {
        if let Some(base) = self.base_rtt()
            && age > base + TARGET_DELAY
        {
            self.decrease(now);
        }
    }

    /// Halves the rate, at most once per base round trip: the bloated round trip would wait seconds between
    /// decreases while the queue keeps growing, and an early extra halving only costs a short ramp.
    fn decrease(&mut self, now: Instant) {
        let gap = self.base_rtt().unwrap_or(MIN_DECREASE_GAP).clamp(MIN_DECREASE_GAP, MAX_DECREASE_GAP);
        if self.last_decrease.is_some_and(|at| now.duration_since(at) < gap) {
            return;
        }
        self.last_decrease = Some(now);
        self.rate = (self.rate / 2.0).max(FLOOR_RATE as f64);
    }

    fn update_base(&mut self, rtt: Duration, now: Instant) {
        match (self.bucket_start, self.base.back_mut()) {
            (Some(start), Some(min)) if now.duration_since(start) < BASE_BUCKET => *min = (*min).min(rtt),
            _ => {
                self.base.push_back(rtt);
                if self.base.len() > BASE_BUCKETS {
                    self.base.pop_front();
                }
                self.bucket_start = Some(now);
            }
        }
    }
}

/// Spends a rate as bytes: a message may go while the balance is not negative, and its size is then
/// subtracted, so a 128 KiB chunk at a low rate simply waits longer for the next one.
#[derive(Default)]
pub struct TokenBucket {
    tokens: f64,
    last: Option<Instant>,
}

impl TokenBucket {
    /// True when a message of `bytes` may be sent at `now` under `rate` (bytes per second); it is then charged.
    pub fn try_take(&mut self, bytes: usize, rate: u64, now: Instant) -> bool {
        let elapsed = self.last.map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        self.last = Some(now);
        let burst = rate as f64 * BURST.as_secs_f64();
        self.tokens = (self.tokens + rate as f64 * elapsed.as_secs_f64()).min(burst);
        if self.tokens < 0.0 {
            return false;
        }
        self.tokens -= bytes as f64;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn a_growing_queue_halves_the_rate_down_to_the_floor() {
        let t0 = Instant::now();
        let mut grown = FileRate::new();
        grown.on_rtt(MS(40), true, t0);
        assert_eq!(grown.rate(), START_RATE + START_RATE / 10, "an empty queue grows the rate");

        let mut rate = FileRate::new();
        rate.on_rtt(MS(40), false, t0);
        // 200 ms of queue: the filter waits until the low sample is out of the last three...
        rate.on_rtt(MS(240), false, t0 + MS(50));
        rate.on_rtt(MS(240), false, t0 + MS(100));
        assert_eq!(rate.rate(), START_RATE, "single slow pongs are filtered out");
        rate.on_rtt(MS(240), false, t0 + MS(150));
        assert_eq!(rate.rate(), START_RATE / 2);
        // ...and halves at most once per base round trip (100 ms at least).
        rate.on_rtt(MS(240), false, t0 + MS(200));
        assert_eq!(rate.rate(), START_RATE / 2, "a second halving waits");
        let mut t = t0 + MS(200);
        for _ in 0..40 {
            t += MS(300);
            rate.on_rtt(MS(240), true, t);
        }
        assert_eq!(rate.rate(), FLOOR_RATE);
    }

    #[test]
    fn an_idle_link_ramps_up_only_while_the_rate_limits() {
        let t0 = Instant::now();
        let mut rate = FileRate::new();
        let mut t = t0;
        for _ in 0..20 {
            t += MS(30);
            rate.on_rtt(MS(30), false, t);
        }
        assert_eq!(rate.rate(), START_RATE, "a sender that never waited for the rate does not raise it");
        for _ in 0..20 {
            t += MS(30);
            rate.on_rtt(MS(30), true, t);
        }
        assert!(rate.rate() > START_RATE * 6, "ramp is about 10% per sample: {}", rate.rate());
        for _ in 0..500 {
            t += MS(30);
            rate.on_rtt(MS(30), true, t);
        }
        assert_eq!(rate.rate(), MAX_RATE);
    }

    #[test]
    fn a_small_queue_grows_slower_than_none() {
        let t0 = Instant::now();
        let (mut empty, mut some) = (FileRate::new(), FileRate::new());
        empty.on_rtt(MS(20), true, t0);
        some.on_rtt(MS(20), true, t0);
        for i in 1..4 {
            empty.on_rtt(MS(20), true, t0 + MS(30 * i));
            some.on_rtt(MS(40), true, t0 + MS(30 * i)); // 20 ms queued: under the target
        }
        assert!(some.rate() > START_RATE && some.rate() < empty.rate());
    }

    #[test]
    fn a_pong_stuck_in_the_queue_lowers_the_rate_before_it_arrives() {
        let t0 = Instant::now();
        let mut rate = FileRate::new();
        rate.on_waiting(MS(500), t0);
        assert_eq!(rate.rate(), START_RATE, "no base round trip yet: nothing to compare with");
        rate.on_rtt(MS(30), false, t0);
        rate.on_waiting(MS(50), t0 + MS(10));
        assert_eq!(rate.rate(), START_RATE, "within the target");
        rate.on_waiting(MS(400), t0 + MS(400));
        assert_eq!(rate.rate(), START_RATE / 2);
        rate.on_waiting(MS(450), t0 + MS(450));
        assert_eq!(rate.rate(), START_RATE / 2, "at most once per 100 ms");
        rate.on_waiting(MS(600), t0 + MS(600));
        assert_eq!(rate.rate(), START_RATE / 4, "the stuck pong keeps lowering it");
    }

    #[test]
    fn the_base_round_trip_forgets_old_minimums() {
        let t0 = Instant::now();
        let mut rate = FileRate::new();
        rate.on_rtt(MS(10), false, t0);
        rate.on_rtt(MS(80), false, t0 + BASE_BUCKET);
        assert_eq!(rate.base_rtt(), Some(MS(10)));
        let later = t0 + BASE_BUCKET * (BASE_BUCKETS as u32 + 1);
        rate.on_rtt(MS(80), false, later);
        for i in 1..BASE_BUCKETS as u32 {
            rate.on_rtt(MS(80), false, later + BASE_BUCKET * i);
        }
        assert_eq!(rate.base_rtt(), Some(MS(80)), "a route that got slower becomes the new base");
    }

    #[test]
    fn the_bucket_averages_to_the_rate() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::default();
        let (mut sent, mut t) = (0usize, t0);
        // 1 MB/s for one second in 1 ms ticks, 128 KiB messages.
        for _ in 0..1000 {
            t += MS(1);
            while bucket.try_take(128 * 1024, 1_000_000, t) {
                sent += 128 * 1024;
            }
        }
        assert!((1_000_000..=1_000_000 + 2 * 128 * 1024).contains(&sent), "{sent}");
    }
}
