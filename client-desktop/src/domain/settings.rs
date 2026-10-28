//! Persistent client settings and gateway URL rules.

use serde::{Deserialize, Serialize};

use super::desk_id;

/// Relay URL used until the user configures one.
pub const DEFAULT_GATEWAY_URL: &str = "ws://127.0.0.1:8088";

/// Settings stored in `config.json` of the app directory (`%APPDATA%\OpenDesk`, `~/.config/opendesk`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settings {
    pub gateway_url: String,
    pub desk_id: String,
    /// Relay credential for this desk ID (see `server-relay/docs/provisioning.md`); `OPENDESK_DEVICE_TOKEN`
    /// overrides it.
    #[serde(default)]
    pub device_token: String,
    /// Relay that issued `device_token` (the token goes to that relay only). Empty for a token set by hand,
    /// which is sent to every relay as before.
    #[serde(default)]
    pub device_token_relay: String,
    /// Monitor wall (admin edition). Stored by every edition so switching editions never loses it.
    #[serde(default)]
    pub monitor: MonitorSettings,
    /// Let the service install signed updates (Windows, see `docs/auto-update.md`). `OPENDESK_AUTO_UPDATE=0`
    /// also turns it off.
    #[serde(default = "yes")]
    pub auto_update: bool,
    /// Manifest URL instead of the GitHub release (`OPENDESK_UPDATE_URL` overrides it). Empty: default.
    #[serde(default)]
    pub update_url: String,
}

fn yes() -> bool {
    true
}

/// Monitor-wall preferences (see `docs/features/monitor-wall.md` at the repository root).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct MonitorSettings {
    /// Tiles per row and column: 2, 3 or 4.
    pub grid: u8,
    /// Switch to the next page every `rotate_secs` seconds.
    pub rotate: bool,
    pub rotate_secs: u32,
    /// Show every online device; otherwise only `selected`.
    pub all: bool,
    /// Clean desk IDs picked by hand.
    pub selected: Vec<String>,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self { grid: 3, rotate: false, rotate_secs: 10, all: true, selected: Vec::new() }
    }
}

impl Settings {
    /// Fresh settings for a device whose ID is `desk_id`.
    pub fn new(desk_id: String) -> Self {
        Self {
            gateway_url: DEFAULT_GATEWAY_URL.to_string(),
            desk_id,
            device_token: String::new(),
            device_token_relay: String::new(),
            monitor: MonitorSettings::default(),
            auto_update: true,
            update_url: String::new(),
        }
    }

    /// Repairs stored values: an invalid desk ID is replaced by `fresh_id()`, the gateway URL is normalized.
    /// Returns true when something changed and the settings should be saved again.
    pub fn repair(&mut self, fresh_id: impl FnOnce() -> String) -> bool {
        let mut changed = false;
        if !desk_id::is_valid(&desk_id::clean(&self.desk_id)) {
            self.desk_id = fresh_id();
            changed = true;
        }
        let gateway = normalize_gateway_url(&self.gateway_url);
        if gateway != self.gateway_url {
            self.gateway_url = gateway;
            changed = true;
        }
        changed
    }
}

/// The token to present to `gateway_url`: one the relay issued is only sent back to that relay.
pub fn token_for(settings: &Settings, gateway_url: &str) -> String {
    let relay = &settings.device_token_relay;
    if relay.is_empty() || normalize_gateway_url(relay) == normalize_gateway_url(gateway_url) {
        settings.device_token.clone()
    } else {
        String::new()
    }
}

/// Turns user input into a WebSocket URL: http(s) becomes ws(s), private addresses default to ws://,
/// everything else to wss://.
pub fn normalize_gateway_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return DEFAULT_GATEWAY_URL.to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("https://") {
        return format!("wss://{rest}");
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        return format!("ws://{rest}");
    }
    if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        return trimmed.to_string();
    }
    let private = ["127.", "localhost", "192.168.", "10."].iter().any(|p| trimmed.starts_with(p));
    if private { format!("ws://{trimmed}") } else { format!("wss://{trimmed}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_urls_are_normalized() {
        assert_eq!(normalize_gateway_url(" https://relay.example.com/ "), "wss://relay.example.com");
        assert_eq!(normalize_gateway_url("192.168.1.5:8088"), "ws://192.168.1.5:8088");
        assert_eq!(normalize_gateway_url("relay.example.com"), "wss://relay.example.com");
        assert_eq!(normalize_gateway_url(""), DEFAULT_GATEWAY_URL);
    }

    #[test]
    fn repair_replaces_invalid_ids_only() {
        let mut s = Settings { gateway_url: "relay.example.com".into(), ..Settings::new("42".into()) };
        assert!(s.repair(|| "123456789".into()));
        assert_eq!((s.desk_id.as_str(), s.gateway_url.as_str()), ("123456789", "wss://relay.example.com"));
        assert!(!s.repair(|| unreachable!()));
    }

    #[test]
    fn issued_tokens_go_back_to_their_relay_only() {
        let mut s = Settings::new("123456789".into());
        s.device_token = "t".into();
        assert_eq!(token_for(&s, "wss://a.example"), "t", "a hand-set token goes everywhere");
        s.device_token_relay = "wss://a.example".into();
        assert_eq!(token_for(&s, "a.example"), "t");
        assert_eq!(token_for(&s, "wss://b.example"), "");
    }

    #[test]
    fn settings_without_monitor_section_still_load() {
        let s: Settings = serde_json::from_str(r#"{"gateway_url":"ws://x","desk_id":"123456789"}"#).unwrap();
        assert_eq!(s.monitor, MonitorSettings::default());
        let partial: MonitorSettings = serde_json::from_str(r#"{"grid":4}"#).unwrap();
        assert_eq!((partial.grid, partial.all, partial.rotate_secs), (4, true, 10));
        assert!(s.auto_update);
    }

    /// Rollback rule: an older build must read the config a newer one wrote, so fields are only ever added
    /// (with a default), never removed or renamed, and unknown fields are ignored.
    #[test]
    fn settings_with_fields_from_a_newer_version_still_load() {
        let s: Settings = serde_json::from_str(
            r#"{"gateway_url":"ws://x","desk_id":"123456789","from_a_later_release":{"a":1},"monitor":{"grid":2,"later":true}}"#,
        )
        .unwrap();
        assert_eq!((s.desk_id.as_str(), s.monitor.grid), ("123456789", 2));
    }
}
