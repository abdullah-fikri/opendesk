//! Capabilities exchanged at registration (`register.caps`, `registered-ack.caps`, `session-connected.peerCaps`).

/// Receiver answers `end` with an ACK/NACK.
pub const FILE_ACK: &str = "file-ack";
/// Copied files are offered and pulled on paste.
pub const FILE_OFFER: &str = "file-offer";
/// Folder transfer (relative paths, `group`/`rel`).
pub const FILE_FOLDER: &str = "file-folder";
/// The viewer draws its own cursor in the host's shape.
pub const CURSOR_SHAPE: &str = "cursor-shape";
/// Framed key/delta video.
pub const VIDEO_DELTA: &str = "video-delta";
/// H.264 motion frames inside the framed format.
pub const VIDEO_H264: &str = "video-h264";
/// Opus voice packets (`[0x02][0x4F][seq][opus]`); without it the partner gets PCM.
pub const AUDIO_OPUS: &str = "audio-opus";
/// Direct encrypted UDP path.
pub const P2P: &str = "p2p.v1";
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
    pub cursor_shape: bool,
    pub video_delta: bool,
    pub video_h264: bool,
    /// Voice to this partner may be Opus (we always decode it, so the partner's word is enough).
    pub audio_opus: bool,
    pub p2p: bool,
    /// The partner can show a privacy screen while hosting.
    pub privacy_screen: bool,
    /// The partner can lock its local input while hosting.
    pub block_input: bool,
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
            cursor_shape: has(CURSOR_SHAPE),
            video_delta: has(VIDEO_DELTA),
            video_h264: has(VIDEO_H264),
            audio_opus: has(AUDIO_OPUS),
            p2p: has(P2P),
            privacy_screen: legacy_host || has(PRIVACY_SCREEN),
            block_input: legacy_host || has(BLOCK_INPUT),
        }
    }
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
    fn host_features_default_to_legacy_windows_hosts() {
        let legacy = PeerCaps::from_list(&[VIDEO_DELTA.to_string()], true);
        assert!(legacy.privacy_screen && legacy.block_input);
        let linux = PeerCaps::from_list(&[HOST_FEATURES.to_string()], true);
        assert!(!linux.privacy_screen && !linux.block_input);
        let partial = PeerCaps::from_list(&[HOST_FEATURES.to_string(), BLOCK_INPUT.to_string()], true);
        assert!(!partial.privacy_screen && partial.block_input);
    }
}
