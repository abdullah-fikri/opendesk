// Package dto defines the JSON wire format between clients and the relay. Types here carry data only;
// validation and decisions belong to the usecase layer. See docs/protocol.md at the repository root for the full protocol.
package dto

import "encoding/json"

// Message types sent by clients.
const (
	TypeRegister          = "register"
	TypeGetHosts          = "get-hosts"
	TypeConnectRequest    = "connect-request"
	TypeApprovalResponse  = "approval-response"
	TypeRelayInput        = "relay-input"
	TypeRelayClipboard    = "relay-clipboard"
	TypeRelayFile         = "relay-file"
	TypeSessionDisconnect = "session-disconnect"
	TypeP2PSignal         = "p2p-signal"
	TypeMonitorSet        = "monitor-set"
	// Administrator-only device security and naming (see docs/features/device-security.md).
	TypeSecurityGet         = "security-get"
	TypeSecuritySet         = "security-set"
	TypeSecurityAllow       = "security-allow"
	TypeSecurityAllowOnline = "security-allow-online"
	TypeSecurityRemove      = "security-remove"
	TypeSecurityReset       = "security-reset"
	TypeHostRename          = "host-rename"
)

// Envelope is decoded first from every text message; Type selects the concrete request type.
type Envelope struct {
	Type string `json:"type"`
}

// RegisterRequest announces a device. It must be the first message on a connection.
type RegisterRequest struct {
	DeskID      string       `json:"deskId"`
	DeviceToken string       `json:"deviceToken"`
	IsAdmin     bool         `json:"isAdmin"`
	Platform    string       `json:"platform"`
	DeviceName  string       `json:"deviceName"`
	Caps        Capabilities `json:"caps"`
}

// Capabilities decodes a JSON array and keeps only its string entries, so one malformed entry
// does not reject the whole registration.
type Capabilities []string

// UnmarshalJSON implements json.Unmarshaler.
func (c *Capabilities) UnmarshalJSON(data []byte) error {
	var raw []any
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	out := make([]string, 0, len(raw))
	for _, item := range raw {
		if s, ok := item.(string); ok {
			out = append(out, s)
		}
	}
	*c = out
	return nil
}

// ConnectRequest asks to view the device TargetID (any formatting; non-digits are ignored).
type ConnectRequest struct {
	TargetID string `json:"targetId"`
}

// ApprovalResponse is an Android host's answer to an incoming-request.
type ApprovalResponse struct {
	FromClean string `json:"fromClean"`
	Accept    bool   `json:"accept"`
}

// RelayInput forwards an input event to the session partner. Event is passed through untouched.
type RelayInput struct {
	TargetClean string          `json:"targetClean"`
	Event       json.RawMessage `json:"event"`
}

// RelayClipboard forwards clipboard text to the session partner.
type RelayClipboard struct {
	TargetClean string          `json:"targetClean"`
	Text        json.RawMessage `json:"text"`
}

// RelayFile forwards one file-transfer message. Payload is passed through; only its header is inspected.
type RelayFile struct {
	TargetClean string          `json:"targetClean"`
	Payload     json.RawMessage `json:"payload"`
}

// FilePayloadHeader is the part of a file-transfer payload the relay needs for authorization.
type FilePayloadHeader struct {
	Action string `json:"action"`
	ID     string `json:"id"`
}

// SessionDisconnect ends the sender's session with TargetClean.
type SessionDisconnect struct {
	TargetClean string `json:"targetClean"`
}

// P2PSignal carries one handshake or address candidate for a direct UDP path.
type P2PSignal struct {
	TargetClean string `json:"targetClean"`
	Candidate   string `json:"candidate"`
}

// MonitorSet replaces the hosts an administrator watches on the monitor wall (clean IDs, at most 16 used).
// An empty list stops watching.
type MonitorSet struct {
	Targets []string `json:"targets"`
}

// SecuritySet turns the device allowlist on or off.
type SecuritySet struct {
	Enabled bool `json:"enabled"`
}

// SecurityAllow adds TargetID (any formatting) to the allowlist, as an administrator when Admin is set.
type SecurityAllow struct {
	TargetID string `json:"targetId"`
	Admin    bool   `json:"admin"`
}

// SecurityTarget names one allowlisted device for security-remove and security-reset.
type SecurityTarget struct {
	TargetClean string `json:"targetClean"`
}

// HostRename names a device in the shared host list; an empty Alias clears the name.
type HostRename struct {
	TargetClean string `json:"targetClean"`
	Alias       string `json:"alias"`
}
