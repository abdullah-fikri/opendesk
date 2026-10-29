//! End-to-end video latency, split into stages, for the stats overlay and the `viewer.perf` line.
//!
//! A host that sees `frame-stamp` from its viewer appends a [`FrameStamp`] to every framed message: its
//! monotonic clock at capture and how long capture to hand-off for sending took. The viewer adds its own
//! instants (message received, batch decoded, image handed to the UI) and maps host time onto its clock with
//! a [`ClockOffset`] estimated from `stats-ping`/`stats-pong`:
//!
//! - Host: capture to hand-off (from the stamp; the send queue holds at most one frame, see `can_send_frame`).
//! - Network: hand-off on the host to the complete message in the viewer's inbox (all P2P fragments in).
//! - Decode: inbox to decoded, including a wait while the previous batch decodes.
//! - Display: decoded to handed to Slint; the compositor adds up to one refresh interval after that.
//! - Total: capture to handed to Slint.
//!
//! Without stamps (hosts up to 1.2.0) Host, Network and Total stay empty; without an offset (no pong with
//! host time yet) Network and Total do.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use core_engine::codec::frame::FrameStamp;

use super::perf_stats::{Stage, StageSummary};

/// Microseconds of `t` on this process's monotonic clock, from an epoch fixed at first use. Negative before
/// it; peers send it as `u64` (two's complement) and read it back with `as i64`, so only differences count.
pub fn mono_us(t: Instant) -> i64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    match t.checked_duration_since(epoch) {
        Some(d) => d.as_micros() as i64,
        None => -(epoch.duration_since(t).as_micros() as i64),
    }
}

/// Host clock minus viewer clock, from ping round trips.
///
/// Approximation: each sample assumes the pong left the host halfway through the round trip (one way =
/// RTT / 2), so `offset = host_time - (sent + received) / 2`. An asymmetric path shifts the split between
/// Network and the other stages by up to half the asymmetry; Total stays right to within it as well. The
/// sample with the shortest round trip of the last few is kept, as queueing only ever lengthens one.
#[derive(Default)]
pub struct ClockOffset {
    samples: VecDeque<(i64, i64)>,
}

impl ClockOffset {
    /// Samples considered.
    const KEEP: usize = 8;

    /// Adds one round trip: the ping left at `sent_us` and its pong arrived at `received_us` (viewer clock),
    /// carrying the host clock `host_us`.
    pub fn add(&mut self, sent_us: i64, host_us: i64, received_us: i64) {
        let rtt = received_us.wrapping_sub(sent_us);
        if rtt < 0 {
            return;
        }
        if self.samples.len() == Self::KEEP {
            self.samples.pop_front();
        }
        let mid = sent_us.wrapping_add(rtt / 2);
        self.samples.push_back((rtt, host_us.wrapping_sub(mid)));
    }

    /// The current estimate, host minus viewer, in microseconds.
    pub fn get(&self) -> Option<i64> {
        self.samples.iter().min_by_key(|s| s.0).map(|s| s.1)
    }
}

/// Viewer-side instants of one displayed frame, on [`mono_us`].
#[derive(Clone, Copy, Debug)]
pub struct FrameTimes {
    /// The host's stamp of the newest message in the decoded batch.
    pub stamp: Option<FrameStamp>,
    pub received_us: i64,
    pub decoded_us: i64,
    pub shown_us: i64,
}

/// One session's latency stages and clock offset (see the module docs).
#[derive(Default)]
pub struct LatencyMeter {
    offset: ClockOffset,
    /// The last `stats-ping`: its `t` and when it left.
    ping: Option<(u64, i64)>,
    host: Stage,
    network: Stage,
    decode: Stage,
    display: Stage,
    total: Stage,
}

/// The stages over one window; None where nothing was measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LatencyStats {
    pub host: Option<StageSummary>,
    pub network: Option<StageSummary>,
    pub decode: Option<StageSummary>,
    pub display: Option<StageSummary>,
    pub total: Option<StageSummary>,
}

fn span(from_us: i64, to_us: i64) -> Duration {
    Duration::from_micros(to_us.wrapping_sub(from_us).max(0) as u64)
}

fn some(s: StageSummary) -> Option<StageSummary> {
    (s.count > 0).then_some(s)
}

impl LatencyMeter {
    /// A `stats-ping` with `t` left at `at_us`.
    pub fn ping_sent(&mut self, t: u64, at_us: i64) {
        self.ping = Some((t, at_us));
    }

    /// The pong for `t` arrived at `at_us`; `host_us` is the host's clock when it answered (None from hosts up
    /// to 1.2.0). Pongs of older pings are ignored.
    pub fn pong(&mut self, t: u64, host_us: Option<i64>, at_us: i64) {
        if let (Some((sent_t, sent_us)), Some(host_us)) = (self.ping, host_us)
            && sent_t == t
        {
            self.ping = None;
            self.offset.add(sent_us, host_us, at_us);
        }
    }

    /// Records one displayed frame.
    pub fn record(&mut self, f: &FrameTimes) {
        self.decode.record(span(f.received_us, f.decoded_us));
        self.display.record(span(f.decoded_us, f.shown_us));
        let Some(stamp) = f.stamp else { return };
        self.host.record(Duration::from_micros(u64::from(stamp.host_us)));
        if let Some(offset) = self.offset.get() {
            let captured = (stamp.captured_us as i64).wrapping_sub(offset);
            let sent = captured.wrapping_add(i64::from(stamp.host_us));
            self.network.record(span(sent, f.received_us));
            self.total.record(span(captured, f.shown_us));
        }
    }

    /// The stages since the previous call (stats overlay), independent of the log window.
    pub fn take_lap(&mut self) -> LatencyStats {
        LatencyStats {
            host: some(self.host.take_lap()),
            network: some(self.network.take_lap()),
            decode: some(self.decode.take_lap()),
            display: some(self.display.take_lap()),
            total: some(self.total.take_lap()),
        }
    }

    /// The log window's part of the `viewer.perf` line (avg/max per stage, `-` when unmeasured); resets it.
    pub fn take_line(&mut self) -> String {
        let fmt = |s: StageSummary| if s.count == 0 { "-".to_string() } else { s.to_string() };
        format!(
            "latency host={} net={} decode={} display={} total={}",
            fmt(self.host.take_summary()),
            fmt(self.network.take_summary()),
            fmt(self.decode.take_summary()),
            fmt(self.display.take_summary()),
            fmt(self.total.take_summary()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_clock_is_signed_around_its_epoch() {
        let now = Instant::now();
        let a = mono_us(now);
        // Truncation to whole microseconds may differ by one, depending on which side of the epoch `now` is.
        assert!((mono_us(now + Duration::from_millis(5)) - a - 5000).abs() <= 1);
        // An instant before the epoch still converts, and round-trips through the u64 wire form.
        if let Some(before) = now.checked_sub(Duration::from_secs(3600)) {
            let us = mono_us(before);
            assert!(us < a);
            assert_eq!((us as u64) as i64, us);
        }
    }

    #[test]
    fn offset_assumes_half_the_round_trip_each_way() {
        let mut o = ClockOffset::default();
        assert_eq!(o.get(), None);
        // Host clock runs 1 s ahead; the pong was sent 40 ms into an 80 ms round trip.
        o.add(10_000, 1_050_000, 90_000);
        assert_eq!(o.get(), Some(1_000_000));
        // A slower round trip (queueing) does not replace the tighter sample...
        o.add(200_000, 1_300_000, 500_000);
        assert_eq!(o.get(), Some(1_000_000));
        // ...a faster one does, and samples age out.
        o.add(600_000, 1_610_000, 620_000);
        assert_eq!(o.get(), Some(1_000_000));
        for i in 0..ClockOffset::KEEP as i64 {
            o.add(i * 1000, 2_000_000 + i * 1000 + 50, i * 1000 + 100);
        }
        assert_eq!(o.get(), Some(2_000_000));
        o.add(5, 0, 4); // a negative round trip is noise
        assert_eq!(o.samples.len(), ClockOffset::KEEP);
    }

    #[test]
    fn stages_add_up_to_the_total() {
        let mut m = LatencyMeter::default();
        // Host clock = viewer clock + 1 s, symmetric 20 ms round trip.
        m.ping_sent(7, 100_000);
        m.pong(6, Some(1_110_000), 120_000); // stale pong: ignored
        m.pong(7, Some(1_110_000), 120_000);
        let f = FrameTimes {
            stamp: Some(FrameStamp { captured_us: 1_200_000, host_us: 5_000 }),
            received_us: 215_000,
            decoded_us: 218_000,
            shown_us: 220_000,
        };
        m.record(&f);
        let lap = m.take_lap();
        let ms = |s: Option<StageSummary>| s.map(|s| s.avg_ms);
        assert_eq!(ms(lap.host), Some(5.0));
        assert_eq!(ms(lap.network), Some(10.0));
        assert_eq!(ms(lap.decode), Some(3.0));
        assert_eq!(ms(lap.display), Some(2.0));
        assert_eq!(ms(lap.total), Some(20.0));
        assert_eq!(m.take_lap(), LatencyStats::default(), "laps reset");
        assert_eq!(
            m.take_line(),
            "latency host=5.0/5.0ms net=10.0/10.0ms decode=3.0/3.0ms display=2.0/2.0ms total=20.0/20.0ms"
        );
    }

    #[test]
    fn unstamped_frames_and_old_hosts_leave_host_stages_empty() {
        let mut m = LatencyMeter::default();
        // A host up to 1.2.0: no stamp, and pongs without host time.
        m.ping_sent(1, 0);
        m.pong(1, None, 10_000);
        m.record(&FrameTimes { stamp: None, received_us: 0, decoded_us: 4_000, shown_us: 5_000 });
        let lap = m.take_lap();
        assert!(lap.host.is_none() && lap.network.is_none() && lap.total.is_none());
        assert_eq!(lap.decode.map(|s| s.avg_ms), Some(4.0));
        // Stamped, but no offset yet: only the host's own number.
        m.record(&FrameTimes {
            stamp: Some(FrameStamp { captured_us: 9, host_us: 1_500 }),
            received_us: 0,
            decoded_us: 0,
            shown_us: 0,
        });
        let lap = m.take_lap();
        assert_eq!((lap.host.map(|s| s.avg_ms), lap.total), (Some(1.5), None));
        // The log window spans both frames (decode 4 ms and 0 ms).
        assert!(m.take_line().starts_with("latency host=1.5/1.5ms net=- decode=2.0/4.0ms"));
    }

    #[test]
    fn clock_jitter_never_yields_negative_stages() {
        let mut m = LatencyMeter::default();
        m.ping_sent(1, 0);
        m.pong(1, Some(0), 0);
        // Received "before" it was sent on the estimated clock.
        m.record(&FrameTimes {
            stamp: Some(FrameStamp { captured_us: 10_000, host_us: 1_000 }),
            received_us: 5_000,
            decoded_us: 4_000,
            shown_us: 6_000,
        });
        let lap = m.take_lap();
        assert_eq!((lap.network.unwrap().max_ms, lap.decode.unwrap().max_ms), (0.0, 0.0));
    }
}
