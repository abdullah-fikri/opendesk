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
    pub p2p: bool,
}

impl PeerCaps {
    /// Reads `peer_caps`; `relay_offer` is whether the relay listed `file-offer`.
    pub fn from_list(peer_caps: &[String], relay_offer: bool) -> Self {
        let has = |cap: &str| peer_caps.iter().any(|c| c == cap);
        Self {
            file_ack: has(FILE_ACK),
            file_offer: has(FILE_OFFER) && relay_offer,
            file_folder: has(FILE_FOLDER),
            cursor_shape: has(CURSOR_SHAPE),
            video_delta: has(VIDEO_DELTA),
            video_h264: has(VIDEO_H264),
            p2p: has(P2P),
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
}
