package dto

import "encoding/json"

// Message types sent by the relay.
const (
	TypeRegisteredAck    = "registered-ack"
	TypeHostList         = "host-list"
	TypeIncomingRequest  = "incoming-request"
	TypeSessionConnected = "session-connected"
	TypeInputCommand     = "input-command"
	TypeClipboardSync    = "clipboard-sync"
	TypeFileTransfer     = "file-transfer"
	TypeSessionClosed    = "session-closed"
	TypeError            = "error"
	TypeMonitorState     = "monitor-state"
	TypeMonitorStart     = "monitor-start"
	TypeMonitorStop      = "monitor-stop"
	TypeDeviceCredential = "device-credential"
	TypeSecurityState    = "security-state"
)

// RegisteredAck confirms registration and lists the relay's capabilities.
type RegisteredAck struct {
	Type   string   `json:"type"`
	DeskID string   `json:"deskId"`
	Caps   []string `json:"caps"`
}

// HostList is sent to administrators on request and whenever a device comes or goes.
type HostList struct {
	Type  string `json:"type"`
	Hosts []Host `json:"hosts"`
}

// Host is one entry of a HostList. LastSeen is RFC 3339; clients format it for display.
type Host struct {
	CleanID    string `json:"clean_id"`
	DeskID     string `json:"desk_id"`
	DeviceName string `json:"device_name"`
	Alias      string `json:"alias"`
	Platform   string `json:"platform"`
	Online     bool   `json:"online"`
	LastSeen   string `json:"last_seen"`
}

// IncomingRequest asks an Android host to approve a viewer.
type IncomingRequest struct {
	Type       string `json:"type"`
	FromID     string `json:"fromId"`
	FromClean  string `json:"fromClean"`
	DeviceName string `json:"deviceName"`
}

// SessionConnected tells each side of a pair that traffic may flow.
type SessionConnected struct {
	Type        string   `json:"type"`
	TargetID    string   `json:"targetId"`
	TargetClean string   `json:"targetClean"`
	Role        string   `json:"role"`
	PeerCaps    []string `json:"peerCaps"`
}

// InputCommand delivers a relayed input event. FromClean is set only for multi-session viewers.
type InputCommand struct {
	Type      string          `json:"type"`
	Event     json.RawMessage `json:"event"`
	FromClean string          `json:"fromClean,omitempty"`
}

// ClipboardSync delivers relayed clipboard text. FromClean is set only for multi-session viewers.
type ClipboardSync struct {
	Type      string          `json:"type"`
	Text      json.RawMessage `json:"text"`
	FromClean string          `json:"fromClean,omitempty"`
}

// FileTransfer delivers a relayed file message. FromClean is stamped by the relay, never trusted from the payload.
type FileTransfer struct {
	Type      string          `json:"type"`
	FromClean string          `json:"fromClean"`
	Payload   json.RawMessage `json:"payload"`
}

// P2PSignalForward delivers a P2P candidate from FromClean.
type P2PSignalForward struct {
	Type      string `json:"type"`
	FromClean string `json:"fromClean"`
	Candidate string `json:"candidate"`
}

// SessionClosed ends a session. TargetClean names it for multi-session viewers only.
type SessionClosed struct {
	Type        string `json:"type"`
	Message     string `json:"message"`
	TargetClean string `json:"targetClean,omitempty"`
}

// Error reports a failed request. TargetClean names the failed session for multi-session viewers only.
type Error struct {
	Type        string `json:"type"`
	Message     string `json:"message"`
	TargetClean string `json:"targetClean,omitempty"`
}

// MonitorState tells an administrator what one monitor-wall tile shows: live, offline, unsupported or denied.
type MonitorState struct {
	Type        string `json:"type"`
	TargetClean string `json:"targetClean"`
	State       string `json:"state"`
}

// MonitorStart asks a host to stream monitor-wall thumbnails: plain JPEGs at most MaxWidth pixels wide, at most
// FPS per second, sent only when the screen changed.
type MonitorStart struct {
	Type     string `json:"type"`
	MaxWidth int    `json:"maxWidth"`
	FPS      int    `json:"fps"`
}

// MonitorStop tells a host that nobody watches it any more.
type MonitorStop struct {
	Type string `json:"type"`
}

// DeviceCredential hands a device the token it must present from now on. Clients store it for this relay.
type DeviceCredential struct {
	Type  string `json:"type"`
	Token string `json:"token"`
}

// SecurityState is the device allowlist shown to administrators.
type SecurityState struct {
	Type         string           `json:"type"`
	Enabled      bool             `json:"enabled"`
	ManagedByEnv bool             `json:"managedByEnv"`
	Devices      []SecurityDevice `json:"devices"`
}

// SecurityDevice is one allowlist entry. EnrolledAt is RFC 3339, empty until the device first connects.
type SecurityDevice struct {
	CleanID    string `json:"cleanId"`
	DeskID     string `json:"deskId"`
	Name       string `json:"name"`
	Admin      bool   `json:"admin"`
	Enrolled   bool   `json:"enrolled"`
	EnrolledAt string `json:"enrolledAt"`
	Online     bool   `json:"online"`
}
