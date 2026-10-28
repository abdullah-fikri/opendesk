package model

// Capabilities the relay advertises in registered-ack so clients can detect relay features.
const (
	CapFileGrant    = "file-grant"
	CapFileOffer    = "file-offer"
	CapP2PSignal    = "p2p-signal"
	CapCaps         = "caps"
	CapMultiSession = "multi-session"
	// CapMonitor: the relay runs the read-only monitor wall (see docs/features/monitor-wall.md at the repository root).
	CapMonitor = "monitor"
	// CapDeviceSecurity: administrators manage the device allowlist and enrollment tokens from their client.
	CapDeviceSecurity = "device-security"
	// CapHostAlias: administrators can name devices in the shared host list.
	CapHostAlias = "host-alias"
	// CapFileBinary: the relay forwards binary file chunks. As a client capability it means the client accepts
	// them; the relay sends one only to such a receiver.
	CapFileBinary = "file-binary"
	// CapUDPRelay: the relay forwards direct-path UDP datagrams (udp-relay-offer). As a client capability it
	// means the client can use one; offers go only to sessions where both clients list it.
	CapUDPRelay = "udp-relay"
)

// CapP2P is the client capability of the direct UDP path, which the UDP relay carries.
const CapP2P = "p2p.v1"

// Client capabilities the relay acts on for the monitor wall.
const (
	// CapMonitorHost: the client streams a thumbnail profile on monitor-start.
	CapMonitorHost = "monitor-host"
	// CapMonitorView: the client understands monitor-state and the monitor binary tag (admin edition).
	CapMonitorView = "monitor-view"
)

// CapViewerOnly is a client capability: the device only views others and can never be taken as a host
// (Android, Linux desktops in a Wayland session). Connect requests to it are refused.
const CapViewerOnly = "viewer-only"

// PlatformAndroid identifies Android peers. Android hosts must approve each session before it becomes active.
const PlatformAndroid = "android"

const (
	maxCapabilities  = 16
	maxCapabilityLen = 32
)

// SanitizeCapabilities keeps at most 16 well-formed capability names (lowercase letters, digits, '-' and '.',
// 1..32 characters) and drops everything else. The result is never nil.
func SanitizeCapabilities(raw []string) []string {
	caps := make([]string, 0, min(len(raw), maxCapabilities))
	for _, c := range raw {
		if len(caps) >= maxCapabilities {
			break
		}
		if isCapabilityName(c) {
			caps = append(caps, c)
		}
	}
	return caps
}

func isCapabilityName(s string) bool {
	if len(s) == 0 || len(s) > maxCapabilityLen {
		return false
	}
	for _, c := range s {
		if !(c >= 'a' && c <= 'z' || c >= '0' && c <= '9' || c == '-' || c == '.') {
			return false
		}
	}
	return true
}
