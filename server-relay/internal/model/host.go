package model

import "time"

// Host is a device that has registered with the relay at least once.
type Host struct {
	CleanID    string
	DeskID     string
	DeviceName string
	// Alias is the name an administrator gave the device; empty shows DeviceName.
	Alias      string
	Platform   string
	IsAdmin    bool
	Online     bool
	RemoteAddr string
	LastSeen   time.Time
}

// DeviceCredential is a provisioned per-device secret. Admin grants access to the host list.
type DeviceCredential struct {
	Token string `json:"token"`
	Admin bool   `json:"admin"`
}

// MinDeviceTokenLength is the shortest token the relay accepts as a provisioned credential.
const MinDeviceTokenLength = 32

// AllowedDevice is one entry of the administrator-managed allowlist (see docs/features/device-security.md at
// the repository root). TokenHash is the hex SHA-256 of the token issued when the device first connected; empty
// means the device has not enrolled yet and its next registration receives a fresh token.
type AllowedDevice struct {
	CleanID    string
	Admin      bool
	TokenHash  string
	AddedBy    string
	CreatedAt  time.Time
	EnrolledAt time.Time
}

// Enrolled reports whether the device already holds its token.
func (d AllowedDevice) Enrolled() bool { return d.TokenHash != "" }

// SecurityState is the persisted allowlist configuration.
type SecurityState struct {
	// Enforced admits only allowlisted devices. Off (the default) keeps the relay open.
	Enforced bool
	Devices  []AllowedDevice
}

// MaxAliasLength bounds administrator-given device names.
const MaxAliasLength = 64
