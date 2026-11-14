//! Quality of the UDP video path as the viewer sees it, for the stats overlay: fragment loss from the frame ids
//! and fragment counts of received datagrams, and how often the viewer had to ask for a key frame.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::perf_stats::Counter;

/// Gaps in frame ids larger than this are a restart (new path, wrapped id), not loss.
const MAX_ID_GAP: u32 = 1000;

/// Counts video fragments received and lost on the UDP path.
///
/// The host numbers frames and says how many fragments each has; the viewer keeps only the newest frame (see
/// `infra::p2p`), so a frame is lost when a newer one starts before all its fragments arrived. Its missing
/// fragments count as lost. Frames skipped entirely (an id gap) count as one lost fragment each, as their
/// size is unknown: the loss shown is a lower bound. The path's replay window drops duplicates before they
/// get here, so every fragment counts once.
#[derive(Default)]
pub struct LossMeter {
    /// The newest frame seen: id, fragment count, fragments received.
    current: Option<(u32, usize, usize)>,
    received: Counter,
    lost: Counter,
    lost_frames: Counter,
}

/// Loss over one overlay lap.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LossLap {
    /// Fragments received.
    pub received: u64,
    /// Fragments lost (lower bound, see [`LossMeter`]).
    pub lost: u64,
    /// Frames that never completed.
    pub lost_frames: u64,
}

impl LossLap {
    /// Lost fragments as a percentage of those sent; 0 when nothing was sent.
    pub fn percent(&self) -> f64 {
        let sent = self.received + self.lost;
        if sent == 0 { 0.0 } else { self.lost as f64 * 100.0 / sent as f64 }
    }
}

impl LossMeter {
    /// One authentic video fragment of frame `id`, which has `count` fragments.
    pub fn fragment(&mut self, id: u32, count: usize) {
        self.received += 1;
        match self.current {
            Some((cur, _, ref mut got)) if cur == id => *got += 1,
            // Older than the current frame: arrived late, already counted as lost with its frame.
            Some((cur, _, _)) if (id.wrapping_sub(cur) as i32) < 0 => {}
            previous => {
                if let Some((cur, cur_count, got)) = previous {
                    if got < cur_count {
                        self.lost += (cur_count - got) as u64;
                        self.lost_frames += 1;
                    }
                    let skipped = id.wrapping_sub(cur).wrapping_sub(1);
                    if skipped < MAX_ID_GAP {
                        self.lost += u64::from(skipped);
                        self.lost_frames += u64::from(skipped);
                    }
                }
                self.current = Some((id, count, 1));
            }
        }
    }

    /// The numbers since the previous lap; starts a new one.
    pub fn take_lap(&mut self) -> LossLap {
        LossLap {
            received: self.received.take_lap(),
            lost: self.lost.take_lap(),
            lost_frames: self.lost_frames.take_lap(),
        }
    }
}

/// Events in the last minute (key frame requests), for a per-minute rate.
#[derive(Default)]
pub struct EventRate {
    times: VecDeque<Instant>,
}

impl EventRate {
    const SPAN: Duration = Duration::from_secs(60);

    /// One event at `now`.
    pub fn record(&mut self, now: Instant) {
        self.forget(now);
        self.times.push_back(now);
    }

    /// Events within the minute before `now`.
    pub fn per_minute(&mut self, now: Instant) -> u32 {
        self.forget(now);
        self.times.len() as u32
    }

    fn forget(&mut self, now: Instant) {
        while self.times.front().is_some_and(|t| now.duration_since(*t) >= Self::SPAN) {
            self.times.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(m: &mut LossMeter, id: u32, count: usize, got: usize) {
        for _ in 0..got {
            m.fragment(id, count);
        }
    }

    #[test]
    fn complete_frames_lose_nothing() {
        let mut m = LossMeter::default();
        frame(&mut m, 1, 3, 3);
        frame(&mut m, 2, 1, 1);
        frame(&mut m, 3, 5, 5);
        let lap = m.take_lap();
        assert_eq!(lap, LossLap { received: 9, lost: 0, lost_frames: 0 });
        assert_eq!(lap.percent(), 0.0);
    }

    #[test]
    fn an_incomplete_frame_counts_its_missing_fragments() {
        let mut m = LossMeter::default();
        frame(&mut m, 10, 10, 7);
        frame(&mut m, 11, 2, 2);
        let lap = m.take_lap();
        assert_eq!(lap, LossLap { received: 9, lost: 3, lost_frames: 1 });
        assert_eq!(lap.percent(), 25.0);
        assert_eq!(m.take_lap(), LossLap::default(), "laps reset");
    }

    #[test]
    fn skipped_frames_count_as_one_fragment_each_and_late_ones_are_ignored() {
        let mut m = LossMeter::default();
        frame(&mut m, u32::MAX, 1, 1);
        frame(&mut m, 2, 1, 1); // ids 0 and 1 never arrived (the id wraps)
        m.fragment(1, 4); // a late fragment of a frame already counted as lost
        assert_eq!(m.take_lap(), LossLap { received: 3, lost: 2, lost_frames: 2 });
        // A restart far ahead is not loss.
        frame(&mut m, 50_000, 1, 1);
        assert_eq!(m.take_lap(), LossLap { received: 1, lost: 0, lost_frames: 0 });
    }

    #[test]
    fn event_rate_covers_the_last_minute() {
        let mut r = EventRate::default();
        let t0 = Instant::now();
        assert_eq!(r.per_minute(t0), 0);
        r.record(t0);
        r.record(t0 + Duration::from_secs(30));
        assert_eq!(r.per_minute(t0 + Duration::from_secs(59)), 2);
        assert_eq!(r.per_minute(t0 + Duration::from_secs(60)), 1);
        assert_eq!(r.per_minute(t0 + Duration::from_secs(91)), 0);
    }
}
