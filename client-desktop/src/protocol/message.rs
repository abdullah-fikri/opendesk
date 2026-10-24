//! JSON text messages exchanged with the relay.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{FilePayload, InputEvent};

/// Messages this client sends to the relay.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    /// First message on every connection.
    Register {
        desk_id: String,
        platform: String,
        device_name: String,
        device_token: String,
        is_admin: bool,
        caps: Vec<String>,
    },
    /// Administrators only: asks for the device registry.
    GetHosts,
    /// Opens a viewer session to `target_id`.
    ConnectRequest { target_id: String },
    /// Ends this client's session with `target_clean`.
    SessionDisconnect { target_clean: String },
    /// Input or control event for the session partner.
    RelayInput { target_clean: String, event: InputEvent },
    /// Clipboard text for the session partner.
    RelayClipboard { target_clean: String, text: String },
    /// One file-transfer message for the session partner.
    RelayFile { target_clean: String, payload: FilePayload },
    /// P2P handshake or address candidate for the session partner.
    P2pSignal { target_clean: String, candidate: String },
    /// Administrators only: replaces the hosts shown on the monitor wall (at most 16; empty stops watching).
    #[cfg(feature = "admin")]
    MonitorSet { targets: Vec<String> },
}

impl ClientMessage {
    /// Wraps an input event for `target`.
    pub fn input(target: &str, event: InputEvent) -> Self {
        Self::RelayInput { target_clean: target.to_string(), event }
    }

    /// Wraps a file payload for `target`.
    pub fn file(target: &str, payload: FilePayload) -> Self {
        Self::RelayFile { target_clean: target.to_string(), payload }
    }

    /// Ends the session with `target`.
    pub fn disconnect(target: &str) -> Self {
        Self::SessionDisconnect { target_clean: target.to_string() }
    }

    /// Serializes to the JSON text sent on the WebSocket.
    pub fn to_json(&self) -> String {
        // Every variant is plain data; serialization cannot fail.
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Messages the relay sends to this client. Unknown types decode as `Unknown` and are ignored.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    RegisteredAck {
        #[serde(default)]
        caps: Vec<String>,
    },
    HostList {
        #[serde(default)]
        hosts: Vec<HostEntry>,
    },
    SessionConnected {
        target_clean: String,
        #[serde(default)]
        role: String,
        #[serde(default)]
        peer_caps: Vec<String>,
    },
    /// `event` is decoded separately so one unknown action does not hide the sender.
    InputCommand {
        event: Value,
        from_clean: Option<String>,
    },
    ClipboardSync {
        text: Option<String>,
        from_clean: Option<String>,
    },
    FileTransfer {
        from_clean: Option<String>,
        payload: FilePayload,
    },
    P2pSignal {
        from_clean: String,
        candidate: String,
    },
    SessionClosed {
        message: Option<String>,
        target_clean: Option<String>,
    },
    Error {
        message: Option<String>,
        target_clean: Option<String>,
    },
    /// An administrator watches this machine: stream thumbnails (see `service::host::monitor_stream`).
    MonitorStart {
        #[serde(default)]
        max_width: i64,
        #[serde(default)]
        fps: i64,
    },
    /// Nobody watches this machine any more.
    MonitorStop,
    /// What one monitor-wall tile shows: `live`, `offline`, `unsupported` or `denied`.
    #[cfg(feature = "admin")]
    MonitorState {
        target_clean: String,
        state: String,
    },
    #[serde(other)]
    Unknown,
}

impl ServerMessage {
    /// Decodes one text message; malformed JSON yields `None`.
    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

/// One device in a `host-list`. `last_seen` is RFC 3339.
#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct HostEntry {
    pub desk_id: String,
    pub device_name: String,
    pub platform: String,
    pub online: bool,
    pub last_seen: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_use_the_relay_field_names() {
        let reg = ClientMessage::Register {
            desk_id: "123456789".into(),
            platform: "windows-pc".into(),
            device_name: "PC".into(),
            device_token: String::new(),
            is_admin: false,
            caps: vec!["file-ack".into()],
        };
        let v: Value = serde_json::from_str(&reg.to_json()).unwrap();
        assert_eq!(v["type"], "register");
        assert_eq!(v["deskId"], "123456789");
        assert_eq!(v["isAdmin"], false);
        let sig = ClientMessage::P2pSignal { target_clean: "1".into(), candidate: "c".into() };
        assert_eq!(serde_json::to_value(&sig).unwrap()["type"], "p2p-signal");
        assert_eq!(serde_json::to_value(ClientMessage::GetHosts).unwrap()["type"], "get-hosts");
    }

    #[test]
    fn server_messages_decode_and_unknown_types_are_ignored() {
        let m = ServerMessage::parse(
            r#"{"type":"session-connected","targetId":"1 2","targetClean":"12","role":"viewer","peerCaps":["a"]}"#,
        );
        assert_eq!(
            m,
            Some(ServerMessage::SessionConnected {
                target_clean: "12".into(),
                role: "viewer".into(),
                peer_caps: vec!["a".into()]
            })
        );
        assert_eq!(ServerMessage::parse(r#"{"type":"something-new","x":1}"#), Some(ServerMessage::Unknown));
        assert_eq!(ServerMessage::parse("not json"), None);
        let closed = ServerMessage::parse(r#"{"type":"session-closed","message":"bye"}"#);
        assert_eq!(closed, Some(ServerMessage::SessionClosed { message: Some("bye".into()), target_clean: None }));
    }

    #[test]
    fn monitor_messages_decode() {
        let start = ServerMessage::parse(r#"{"type":"monitor-start","maxWidth":640,"fps":2}"#);
        assert_eq!(start, Some(ServerMessage::MonitorStart { max_width: 640, fps: 2 }));
        assert_eq!(ServerMessage::parse(r#"{"type":"monitor-stop"}"#), Some(ServerMessage::MonitorStop));
        let state = ServerMessage::parse(r#"{"type":"monitor-state","targetClean":"222222222","state":"live"}"#);
        #[cfg(feature = "admin")]
        assert_eq!(state, Some(ServerMessage::MonitorState { target_clean: "222222222".into(), state: "live".into() }));
        // Other editions never ask for the wall, and ignore its messages.
        #[cfg(not(feature = "admin"))]
        assert_eq!(state, Some(ServerMessage::Unknown));
        #[cfg(feature = "admin")]
        {
            let set = ClientMessage::MonitorSet { targets: vec!["222222222".into()] };
            assert_eq!(set.to_json(), r#"{"type":"monitor-set","targets":["222222222"]}"#);
        }
    }
}
