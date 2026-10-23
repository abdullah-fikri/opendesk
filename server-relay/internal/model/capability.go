package model

// Capabilities the relay advertises in registered-ack so clients can detect relay features.
const (
	CapFileGrant    = "file-grant"
	CapFileOffer    = "file-offer"
	CapP2PSignal    = "p2p-signal"
	CapCaps         = "caps"
	CapMultiSession = "multi-session"
)

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
