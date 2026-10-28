//! The session stats overlay: the viewer's own numbers (decoder thread, once a second), the host's
//! `host-stats` summary and the `stats-ping` round trip, formatted as label and value columns.

use crate::protocol::HostStats;

/// The decoder thread's numbers over the last second.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewerStats {
    /// Frames handed to the UI per second.
    pub fps: f32,
    /// Video bytes received, in kilobits per second.
    pub kbps: u64,
    /// Decode and compose time per batch, average and maximum ms.
    pub decode_ms: f32,
    pub decode_max_ms: f32,
}

/// What the overlay of one session shows. Hosts that predate `host-stats` leave `host` and `rtt_ms` empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionStats {
    pub viewer: ViewerStats,
    pub host: Option<HostStats>,
    pub rtt_ms: Option<u64>,
}

const NONE: &str = "–";

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
    pub fn rows(&self, path: &str, e2e: bool) -> Vec<(&'static str, String)> {
        let v = &self.viewer;
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
            ("Decode", format!("{:.1} / {:.1} ms", v.decode_ms, v.decode_max_ms)),
            ("Encoder", host(&|h| encoder_label(&h.encoder))),
            ("Host grab", host(&|h| format!("{:.1} / {:.1} ms", h.grab_ms, h.grab_max_ms))),
            ("Host encode", host(&|h| format!("{:.1} / {:.1} ms", h.encode_ms, h.encode_max_ms))),
            ("Frames", host(&|h| format!("{} idle · {} dropped", h.idle, h.gate_skips))),
            ("Congestion", host(&|h| format!("{:.0}%", h.congestion.clamp(0.0, 1.0) * 100.0))),
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

    #[test]
    fn overlay_without_host_stats_shows_the_viewer_side() {
        let s = SessionStats {
            viewer: ViewerStats { fps: 29.6, kbps: 812, decode_ms: 2.44, decode_max_ms: 9.0 },
            ..Default::default()
        };
        let (labels, values) = s.columns(path_label(false, false), false);
        assert_eq!(
            labels,
            "RTT\nPath\nEncryption\nReceived\nFPS\nDecode\nEncoder\nHost grab\nHost encode\nFrames\nCongestion"
        );
        assert_eq!(values, "–\nRelay\nNot end-to-end\n812 kbps\n30\n2.4 / 9.0 ms\n–\n–\n–\n–\n–");
    }

    #[test]
    fn overlay_with_host_stats() {
        let s = SessionStats {
            viewer: ViewerStats { fps: 58.2, kbps: 2450, decode_ms: 3.1, decode_max_ms: 6.25 },
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
                ..Default::default()
            }),
            rtt_ms: Some(23),
        };
        let (_, values) = s.columns(path_label(true, false), true);
        assert_eq!(
            values,
            "23 ms\nP2P\nEnd-to-end\n2450 kbps\n58 / 60\n3.1 / 6.2 ms\nHW · Intel QSV\n1.2 / 4.0 ms\n3.1 / 7.5 ms\n12 idle · 1 dropped\n25%"
        );
    }

    #[test]
    fn encoder_labels() {
        assert_eq!(encoder_label(""), "JPEG");
        assert_eq!(encoder_label("Microsoft H.264 (software)"), "SW · Microsoft H.264");
        assert_eq!(encoder_label("Cisco OpenH264 2.6.0 (software, openh264.dll)"), "SW · Cisco OpenH264 2.6.0");
        assert_eq!(encoder_label("NVIDIA H.264 Encoder MFT (hardware MFT)"), "HW · NVIDIA H.264 Encoder MFT");
    }
}
