//! The session stats overlay: the viewer's own numbers (decoder thread, once a second), the host's
//! `host-stats` summary, the `stats-ping` round trip, the latency stages of `domain::latency` and the UDP
//! path's quality (`domain::link_quality`), formatted as label and value columns.

use std::sync::Arc;
use std::time::Instant;

use core_engine::codec::frame::FrameStamp;
use parking_lot::Mutex;

use crate::domain::latency::{FrameTimes, LatencyMeter, LatencyStats, mono_us};
use crate::domain::link_quality::LossLap;
use crate::domain::perf_stats::{ProcessUsage, StageSummary};
use crate::protocol::HostStats;

/// The decoder thread's numbers over the last second.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewerStats {
    /// Frames handed to the UI per second.
    pub fps: f32,
    /// Video bytes received, in kilobits per second.
    pub kbps: u64,
    /// This process; None until the second sample.
    pub usage: Option<ProcessUsage>,
}

/// What the overlay of one session shows. Hosts that predate `host-stats` leave `host` and `rtt_ms` empty;
/// hosts without `frame-stamp` leave the host-side latency stages empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionStats {
    pub viewer: ViewerStats,
    pub host: Option<HostStats>,
    pub rtt_ms: Option<u64>,
    pub latency: LatencyStats,
    /// Video fragment loss over the last second; None while video takes the relay's WebSocket.
    pub loss: Option<LossLap>,
    /// Key frames this viewer asked for in the last minute.
    pub key_requests: u32,
}

/// The viewer-side instants of one decoded frame on its way to the screen.
pub struct FrameLatency {
    pub meter: Arc<Mutex<LatencyMeter>>,
    /// The host's stamp of the message the picture shows.
    pub stamp: Option<FrameStamp>,
    /// The message was complete in the inbox.
    pub received: Instant,
    pub decoded: Instant,
}

impl FrameLatency {
    /// Records the frame as presented at `at`: rendered by the UI toolkit, or handed to it where the renderer
    /// cannot report rendering (see `ui::presenter`).
    pub fn shown(self, at: Instant) {
        self.meter.lock().record(&FrameTimes {
            stamp: self.stamp,
            received_us: mono_us(self.received),
            decoded_us: mono_us(self.decoded),
            shown_us: mono_us(at),
        });
    }
}

const NONE: &str = "–";

/// `avg / max ms`, or the placeholder when nothing was measured.
fn stage(s: &Option<StageSummary>) -> String {
    s.as_ref().map_or_else(|| NONE.to_string(), |s| ms_pair(s.avg_ms as f32, s.max_ms as f32))
}

fn ms_pair(avg: f32, max: f32) -> String {
    format!("{avg:.1} / {max:.1} ms")
}

/// An optional `host-stats` pair (absent from older hosts and when unmeasured).
fn opt_pair(avg: Option<f32>, max: Option<f32>) -> String {
    match (avg, max) {
        (Some(avg), Some(max)) => ms_pair(avg, max),
        _ => NONE.to_string(),
    }
}

/// `0.4% · 3 frags · 1 frames`, or the placeholder off the UDP path.
fn loss(l: &Option<LossLap>) -> String {
    l.as_ref().map_or_else(
        || NONE.to_string(),
        |l| format!("{:.1}% · {} frags · {} frames", l.percent(), l.lost, l.lost_frames),
    )
}

/// `3% · 148 MB`, or the placeholder.
fn usage(cpu: Option<f32>, ram_mb: Option<f32>) -> String {
    match (cpu, ram_mb) {
        (Some(cpu), Some(ram)) => format!("{cpu:.0}% · {ram:.0} MB"),
        _ => NONE.to_string(),
    }
}

/// The session path as shown to the user: the direct UDP path, the same path through the relay's UDP forwarder,
/// or the relay's WebSocket.
pub fn path_label(p2p: bool, relayed: bool) -> &'static str {
    match (p2p, relayed) {
        (true, false) => "P2P",
        (true, true) => "Relay (UDP)",
        (false, _) => "Relay",
    }
}

impl SessionStats {
    /// Overlay rows: (label, value). `e2e`: the session is end-to-end encrypted (both sides list `e2e`).
    /// Host, Network, Decode, Display and Total are the stages of one frame from capture to screen (avg / max);
    /// Host capture is the capture work only, without waiting for the screen to change; Host convert and Host
    /// encode split the encoder's time, Host send is the sender's. Ack RTT, window and Loss exist on the UDP
    /// path only.
    pub fn rows(&self, path: &str, e2e: bool) -> Vec<(&'static str, String)> {
        let v = &self.viewer;
        let l = &self.latency;
        let h = self.host.as_ref();
        let host = |f: &dyn Fn(&HostStats) -> String| h.map_or_else(|| NONE.to_string(), f);
        vec![
            ("RTT", self.rtt_ms.map_or_else(|| NONE.to_string(), |ms| format!("{ms} ms"))),
            ("Path", path.to_string()),
            ("Encryption", if e2e { "End-to-end" } else { "Not end-to-end" }.to_string()),
            ("Received", format!("{} kbps", v.kbps)),
            (
                "FPS",
                match h {
                    Some(h) => format!("{:.0} / {}", v.fps, h.target_fps),
                    None => format!("{:.0}", v.fps),
                },
            ),
            ("Encoder", host(&|h| encoder_label(&h.encoder))),
            ("Host", stage(&l.host)),
            ("Network", stage(&l.network)),
            ("Decode", stage(&l.decode)),
            ("Display", stage(&l.display)),
            ("Total", stage(&l.total)),
            ("Host capture", host(&|h| ms_pair(h.grab_ms, h.grab_max_ms))),
            ("Host convert", host(&|h| opt_pair(h.convert_ms, h.convert_max_ms))),
            // Hosts up to 1.3.1 send the whole encode only.
            (
                "Host encode",
                host(&|h| match h.codec_ms {
                    Some(_) => opt_pair(h.codec_ms, h.codec_max_ms),
                    None => ms_pair(h.encode_ms, h.encode_max_ms),
                }),
            ),
            ("Host send", host(&|h| opt_pair(h.send_ms, h.send_max_ms))),
            (
                "Ack RTT",
                host(&|h| match (opt_pair(h.ack_rtt_ms, h.ack_rtt_max_ms), h.window) {
                    (rtt, Some(w)) => format!("{rtt} · window {w}"),
                    (rtt, None) => rtt,
                }),
            ),
            ("Loss", loss(&self.loss)),
            ("Key frames", format!("{} requested / min", self.key_requests)),
            ("Frames", host(&|h| format!("{} idle · {} dropped", h.idle, h.gate_skips))),
            ("Congestion", host(&|h| format!("{:.0}%", h.congestion.clamp(0.0, 1.0) * 100.0))),
            ("CPU / RAM", usage(v.usage.map(|u| u.cpu_pct), v.usage.map(|u| u.ram_mb))),
            ("Host CPU / RAM", h.map_or_else(|| NONE.to_string(), |h| usage(h.cpu, h.ram_mb))),
        ]
    }

    /// The rows as two newline-joined columns (labels, values) for the overlay's two text blocks.
    pub fn columns(&self, path: &str, e2e: bool) -> (String, String) {
        let rows = self.rows(path, e2e);
        let labels = rows.iter().map(|r| r.0).collect::<Vec<_>>().join("\n");
        let values = rows.into_iter().map(|r| r.1).collect::<Vec<_>>().join("\n");
        (labels, values)
    }
}

/// "HW · Intel QSV", "SW · Microsoft H.264", or "JPEG" when the host has no H.264 encoder running. Encoder names
/// end in a parenthesis naming the kind ("(hardware MFT)", "(software)", "(software, <dll>)").
fn encoder_label(name: &str) -> String {
    if name.is_empty() {
        return "JPEG".into();
    }
    let kind = if name.contains("software") { "SW" } else { "HW" };
    let short = name.split(" (").next().unwrap_or(name);
    format!("{kind} · {short}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn overlay_without_host_stats_shows_the_viewer_side() {
        let s = SessionStats {
            viewer: ViewerStats { fps: 29.6, kbps: 812, usage: None },
            latency: LatencyStats { decode: summary(2.44, 9.0), display: summary(1.0, 1.5), ..Default::default() },
            ..Default::default()
        };
        let (labels, values) = s.columns(path_label(false, false), false);
        assert_eq!(
            labels,
            "RTT\nPath\nEncryption\nReceived\nFPS\nEncoder\nHost\nNetwork\nDecode\nDisplay\nTotal\nHost capture\nHost convert\nHost encode\nHost send\nAck RTT\nLoss\nKey frames\nFrames\nCongestion\nCPU / RAM\nHost CPU / RAM"
        );
        // A host up to 1.2.0 (no stamps, no host stats): only the viewer's own stages.
        assert_eq!(
            values,
            "–\nRelay\nNot end-to-end\n812 kbps\n30\n–\n–\n–\n2.4 / 9.0 ms\n1.0 / 1.5 ms\n–\n–\n–\n–\n–\n–\n–\n0 requested / min\n–\n–\n–\n–"
        );
    }

    #[test]
    fn overlay_with_host_stats() {
        let s = SessionStats {
            viewer: ViewerStats { fps: 58.2, kbps: 2450, usage: Some(ProcessUsage { cpu_pct: 4.4, ram_mb: 180.2 }) },
            host: Some(HostStats {
                target_fps: 60,
                grab_ms: 1.2,
                grab_max_ms: 4.0,
                encode_ms: 3.1,
                encode_max_ms: 7.5,
                idle: 12,
                gate_skips: 1,
                congestion: 0.254,
                encoder: "Intel QSV (hardware MFT)".into(),
                cpu: Some(12.0),
                ram_mb: Some(95.6),
                convert_ms: Some(0.4),
                convert_max_ms: Some(1.0),
                codec_ms: Some(2.7),
                codec_max_ms: Some(6.5),
                send_ms: Some(0.8),
                send_max_ms: Some(2.0),
                ack_rtt_ms: Some(12.0),
                ack_rtt_max_ms: Some(18.0),
                window: Some(3),
                ..Default::default()
            }),
            rtt_ms: Some(23),
            loss: Some(LossLap { received: 995, lost: 5, lost_frames: 1 }),
            key_requests: 2,
            latency: LatencyStats {
                host: summary(5.0, 9.0),
                network: summary(11.5, 20.0),
                decode: summary(3.1, 6.25),
                display: summary(2.0, 4.0),
                total: summary(21.6, 30.0),
            },
        };
        let (_, values) = s.columns(path_label(true, false), true);
        assert_eq!(
            values,
            "23 ms\nP2P\nEnd-to-end\n2450 kbps\n58 / 60\nHW · Intel QSV\n5.0 / 9.0 ms\n11.5 / 20.0 ms\n3.1 / 6.2 ms\n2.0 / 4.0 ms\n21.6 / 30.0 ms\n1.2 / 4.0 ms\n0.4 / 1.0 ms\n2.7 / 6.5 ms\n0.8 / 2.0 ms\n12.0 / 18.0 ms · window 3\n0.5% · 5 frags · 1 frames\n2 requested / min\n12 idle · 1 dropped\n25%\n4% · 180 MB\n12% · 96 MB"
        );
    }

    #[test]
    fn hosts_up_to_1_3_1_show_their_whole_encode_time() {
        let s = SessionStats {
            host: Some(HostStats { encode_ms: 3.1, encode_max_ms: 7.5, ..Default::default() }),
            ..Default::default()
        };
        let rows = s.rows("Relay", false);
        let row = |label: &str| rows.iter().find(|r| r.0 == label).map(|r| r.1.clone()).unwrap();
        assert_eq!(row("Host convert"), "–");
        assert_eq!(row("Host encode"), "3.1 / 7.5 ms");
        assert_eq!((row("Host send"), row("Ack RTT")), ("–".to_string(), "–".to_string()));
    }

    fn summary(avg_ms: f64, max_ms: f64) -> Option<StageSummary> {
        Some(StageSummary { count: 1, avg_ms, max_ms })
    }

    #[test]
    fn shown_frames_feed_the_session_meter() {
        let meter = Arc::new(Mutex::new(LatencyMeter::default()));
        let t0 = Instant::now();
        let f = FrameLatency {
            meter: Arc::clone(&meter),
            stamp: None,
            received: t0,
            decoded: t0 + Duration::from_millis(3),
        };
        f.shown(t0 + Duration::from_millis(5));
        let lap = meter.lock().take_lap();
        assert_eq!((lap.decode.map(|s| s.avg_ms), lap.display.map(|s| s.avg_ms)), (Some(3.0), Some(2.0)));
        assert!(lap.total.is_none(), "unstamped");
    }

    #[test]
    fn encoder_labels() {
        assert_eq!(encoder_label(""), "JPEG");
        assert_eq!(encoder_label("Microsoft H.264 (software)"), "SW · Microsoft H.264");
        assert_eq!(encoder_label("Cisco OpenH264 2.6.0 (software, openh264.dll)"), "SW · Cisco OpenH264 2.6.0");
        assert_eq!(encoder_label("NVIDIA H.264 Encoder MFT (hardware MFT)"), "HW · NVIDIA H.264 Encoder MFT");
    }
}
