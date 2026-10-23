package model

import "time"

// Host is a device that has registered with the relay at least once.
type Host struct {
	CleanID    string
	DeskID     string
	DeviceName string
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
