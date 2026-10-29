//! Capabilities exchanged at registration (`register.caps`, `registered-ack.caps`, `session-connected.peerCaps`).

use super::frame_rate::DEFAULT_FPS;
use super::performance::Performance;

/// Receiver answers `end` with an ACK/NACK.
pub const FILE_ACK: &str = "file-ack";
/// Copied files are offered and pulled on paste.
pub const FILE_OFFER: &str = "file-offer";
/// Folder transfer (relative paths, `group`/`rel`).
pub const FILE_FOLDER: &str = "file-folder";
/// File chunks as binary messages instead of base64 JSON. From a client: it accepts them; from the relay: it
/// forwards them. Used only when both the partner and the relay list it.
pub const FILE_BINARY: &str = "file-binary";
/// The viewer draws its own cursor in the host's shape.
pub const CURSOR_SHAPE: &str = "cursor-shape";
/// Framed key/delta video.
pub const VIDEO_DELTA: &str = "video-delta";
/// H.264 motion frames inside the framed format.
pub const VIDEO_H264: &str = "video-h264";
/// Framed video may end in a capture stamp (`core_engine::codec::frame::FrameStamp`) for the latency rows of
/// the stats overlay. From a viewer: it parses the trailer; a host stamps only toward viewers that list it.
pub const FRAME_STAMP: &str = "frame-stamp";
/// Opus voice packets (`[0x02][0x4F][seq][opus]`); without it the partner gets PCM.
pub const AUDIO_OPUS: &str = "audio-opus";
/// Prefix of the viewer's frame-rate cap (`fps-144`): the most frames per second it wants. A dash, not `=`:
/// the relay forwards only names of `[a-z0-9.-]`. Without it the partner gets 60 (see `domain::frame_rate`).
pub const MAX_FPS_PREFIX: &str = "fps-";
// The viewer's performance preset travels as `perf-low` / `perf-medium` / `perf-high` (`Performance::cap`): how
// the host spends bandwidth. Without one the partner gets Medium. The frame rate still travels as `fps-N`.
/// Direct encrypted UDP path.
pub const P2P: &str = "p2p.v1";
/// Can use the relay server's UDP forwarder for the direct path (`udp-relay-offer`); the relay lists it too.
pub const UDP_RELAY: &str = "udp-relay";
/// End-to-end encryption with pinned device keys (`Noise_XX`, see `docs/features/e2e.md`).
pub const E2E: &str = "e2e";
/// Up to four viewer sessions per connection.
pub const MULTI_SESSION: &str = "multi-session";
/// The relay forwards P2P signaling.
pub const RELAY_P2P_SIGNAL: &str = "p2p-signal";
/// This machine streams monitor-wall thumbnails when the relay asks (every edition).
pub const MONITOR_HOST: &str = "monitor-host";
/// This client shows the monitor wall (admin edition only).
#[cfg(feature = "admin")]
pub const MONITOR_VIEW: &str = "monitor-view";
/// The relay runs the monitor wall.
pub const RELAY_MONITOR: &str = "monitor";
/// The relay keeps an administrator-managed device allowlist with automatic enrollment tokens.
pub const RELAY_DEVICE_SECURITY: &str = "device-security";
/// Administrators can name devices in the relay's host list.
pub const RELAY_HOST_ALIAS: &str = "host-alias";
/// This device only views; it can never be controlled (Android, Linux in a Wayland session). The relay refuses
/// connect requests to it.
pub const VIEWER_ONLY: &str = "viewer-only";
/// The host lists its optional host features (`privacy-screen`, `block-input`). Without this marker a host is a
/// legacy Windows host that has all of them, so older clients keep their controls.
pub const HOST_FEATURES: &str = "host-features";
/// With `host-features`: the host can show a privacy screen (`set-privacy`).
pub const PRIVACY_SCREEN: &str = "privacy-screen";
/// With `host-features`: the host can lock its physical keyboard and mouse (`set-block-input`).
pub const BLOCK_INPUT: &str = "block-input";

/// Features a session partner advertised in `peerCaps`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerCaps {
    pub file_ack: bool,
    /// Offers need relay support too, so this is the partner's capability AND the relay's.
    pub file_offer: bool,
    pub file_folder: bool,
    /// The partner accepts binary file chunks (the relay must list `file-binary` too, see [`RelayCaps`]).
    pub file_binary: bool,
    pub cursor_shape: bool,
    pub video_delta: bool,
    pub video_h264: bool,
    /// Framed video to this viewer may carry capture stamps.
    pub frame_stamp: bool,
    /// Voice to this partner may be Opus (we always decode it, so the partner's word is enough).
    pub audio_opus: bool,
    pub p2p: bool,
    pub e2e: bool,
    /// The partner can show a privacy screen while hosting.
    pub privacy_screen: bool,
    /// The partner can lock its local input while hosting.
    pub block_input: bool,
    /// Frame rate the viewer wants at most (`fps-N`); 60 when it lists none.
    pub max_fps: u32,
    /// The viewer's performance preset (`perf-*`); Medium when it lists none (older builds, Android).
    pub performance: Performance,
}

impl PeerCaps {
    /// Reads `peer_caps`; `relay_offer` is whether the relay listed `file-offer`.
    pub fn from_list(peer_caps: &[String], relay_offer: bool) -> Self {
        let has = |cap: &str| peer_caps.iter().any(|c| c == cap);
        let legacy_host = !has(HOST_FEATURES);
        Self {
            file_ack: has(FILE_ACK),
            file_offer: has(FILE_OFFER) && relay_offer,
            file_folder: has(FILE_FOLDER),
            file_binary: has(FILE_BINARY),
            cursor_shape: has(CURSOR_SHAPE),
            video_delta: has(VIDEO_DELTA),
            video_h264: has(VIDEO_H264),
            frame_stamp: has(FRAME_STAMP),
            audio_opus: has(AUDIO_OPUS),
            p2p: has(P2P),
            e2e: has(E2E),
            privacy_screen: legacy_host || has(PRIVACY_SCREEN),
            block_input: legacy_host || has(BLOCK_INPUT),
            max_fps: peer_caps
                .iter()
                .find_map(|c| c.strip_prefix(MAX_FPS_PREFIX)?.parse().ok())
                .filter(|&n| n > 0)
                .unwrap_or(DEFAULT_FPS),
            performance: peer_caps.iter().find_map(|c| Performance::from_cap(c)).unwrap_or_default(),
        }
    }
}

/// The `fps-N` capability for a viewer that wants up to `fps` frames per second.
pub fn max_fps_cap(fps: u32) -> String {
    format!("{MAX_FPS_PREFIX}{fps}")
}

/// Features the relay advertised in `registered-ack`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelayCaps {
    pub p2p_signal: bool,
    pub file_offer: bool,
    pub multi_session: bool,
    pub monitor: bool,
    pub device_security: bool,
    pub host_alias: bool,
    /// Binary file chunks are forwarded.
    pub file_binary: bool,
}

impl RelayCaps {
    /// Reads the relay's capability list.
    pub fn from_list(caps: &[String]) -> Self {
        let has = |cap: &str| caps.iter().any(|c| c == cap);
        Self {
            p2p_signal: has(RELAY_P2P_SIGNAL),
            file_offer: has(FILE_OFFER),
            multi_session: has(MULTI_SESSION),
            monitor: has(RELAY_MONITOR),
            device_security: has(RELAY_DEVICE_SECURITY),
            host_alias: has(RELAY_HOST_ALIAS),
            file_binary: has(FILE_BINARY),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_need_both_partner_and_relay() {
        let caps = vec![FILE_OFFER.to_string(), FILE_ACK.to_string()];
        assert!(PeerCaps::from_list(&caps, true).file_offer);
        assert!(!PeerCaps::from_list(&caps, false).file_offer);
        assert!(PeerCaps::from_list(&caps, false).file_ack);
        assert!(RelayCaps::from_list(&[MULTI_SESSION.to_string()]).multi_session);
        assert!(PeerCaps::from_list(&[FILE_BINARY.to_string()], false).file_binary);
        assert!(RelayCaps::from_list(&[FILE_BINARY.to_string()]).file_binary && !RelayCaps::from_list(&[]).file_binary);
    }

    #[test]
    fn opus_only_toward_partners_that_list_it() {
        // Android and desktop builds before Opus never list it, so they keep receiving PCM.
        let android = PeerCaps::from_list(&[VIEWER_ONLY.to_string()], true);
        let old_desktop = PeerCaps::from_list(&[VIDEO_DELTA.to_string(), P2P.to_string()], true);
        assert!(!android.audio_opus && !old_desktop.audio_opus);
        // No relay support is needed: Opus keeps the 0x02 audio marker every relay forwards.
        assert!(PeerCaps::from_list(&[AUDIO_OPUS.to_string()], false).audio_opus);
    }

    #[test]
    fn frame_rate_cap_defaults_to_60() {
        assert_eq!(PeerCaps::from_list(&[VIDEO_DELTA.to_string()], true).max_fps, 60);
        assert_eq!(PeerCaps::from_list(&[max_fps_cap(144)], true).max_fps, 144);
        for junk in ["fps-0", "fps-", "fps-fast", "fps=144"] {
            assert_eq!(PeerCaps::from_list(&[junk.to_string()], true).max_fps, 60, "{junk}");
        }
    }

    #[test]
    fn frame_stamps_only_toward_viewers_that_list_them() {
        // Android and desktops up to 1.2.0 parse framed video without the trailer.
        let android = PeerCaps::from_list(&[VIEWER_ONLY.to_string(), VIDEO_DELTA.to_string(), E2E.to_string()], true);
        let old_desktop = PeerCaps::from_list(&[VIDEO_DELTA.to_string(), VIDEO_H264.to_string()], true);
        assert!(!android.frame_stamp && !old_desktop.frame_stamp);
        assert!(PeerCaps::from_list(&[VIDEO_DELTA.to_string(), FRAME_STAMP.to_string()], false).frame_stamp);
    }

    #[test]
    fn performance_preset_defaults_to_medium() {
        // Older desktops and Android list no preset.
        assert_eq!(PeerCaps::from_list(&[VIDEO_H264.to_string()], true).performance, Performance::Medium);
        for p in Performance::ALL {
            assert_eq!(PeerCaps::from_list(&[max_fps_cap(30), p.cap().to_string()], true).performance, p);
        }
        assert_eq!(PeerCaps::from_list(&["perf-ultra".to_string()], true).performance, Performance::Medium);
    }

    #[test]
    fn e2e_only_with_partners_that_list_it() {
        // Older desktops and Android keep Noise_NN on P2P and plain relay traffic.
        assert!(!PeerCaps::from_list(&[P2P.to_string()], true).e2e);
        assert!(PeerCaps::from_list(&[E2E.to_string()], false).e2e);
    }

    #[test]
    fn host_features_default_to_legacy_windows_hosts() {
        let legacy = PeerCaps::from_list(&[VIDEO_DELTA.to_string()], true);
        assert!(legacy.privacy_screen && legacy.block_input);
        let linux = PeerCaps::from_list(&[HOST_FEATURES.to_string()], true);
        assert!(!linux.privacy_screen && !linux.block_input);
        let partial = PeerCaps::from_list(&[HOST_FEATURES.to_string(), BLOCK_INPUT.to_string()], true);
        assert!(!partial.privacy_screen && partial.block_input);
    }
}
