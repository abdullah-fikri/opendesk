package usecase

import "errors"

var (
	// ErrInvalidDeskID means the desk ID is not nine digits without a leading zero.
	ErrInvalidDeskID = errors.New("invalid desk ID")
	// ErrUnauthorized means the device is not provisioned or presented the wrong token.
	ErrUnauthorized = errors.New("invalid device credentials")
	// ErrTargetUnavailable means the target is offline, busy, or the caller cannot open another session.
	ErrTargetUnavailable = errors.New("target device unavailable")
	// ErrTargetViewerOnly means the target advertised model.CapViewerOnly and cannot be controlled.
	ErrTargetViewerOnly = errors.New("target device is viewer-only")
	// ErrSelfConnect means a device tried to view itself.
	ErrSelfConnect = errors.New("cannot connect to own device")
	// ErrNoPendingSession means an approval did not match a pending session.
	ErrNoPendingSession = errors.New("no matching pending session")
	// ErrNotAdmin means a request reserved for administrators came from another device.
	ErrNotAdmin = errors.New("administrator rights required")
	// ErrDeviceOnline means an administrator tried to delete a device that is connected (or reconnecting).
	ErrDeviceOnline = errors.New("device is online")
	// ErrInvalidName means a community or group name is empty or too long after trimming, or an icon or color
	// is not accepted.
	ErrInvalidName = errors.New("invalid name")
	// ErrNameTaken means another community (or group of the same community) already has this name.
	ErrNameTaken = errors.New("name already in use")
	// ErrNotFound means the community or group does not exist (any more).
	ErrNotFound = errors.New("not found")
	// ErrLimit means a community or group limit (model.MaxCommunities, ...) is reached.
	ErrLimit = errors.New("limit reached")
	// ErrUnsupported means the relay runs without community storage.
	ErrUnsupported = errors.New("not supported by this relay")
	// ErrPeerClosed means a message was addressed to a connection whose writer has stopped.
	ErrPeerClosed = errors.New("peer connection closed")
	// ErrSlowReceiver means the receiver fell too far behind and was closed.
	ErrSlowReceiver = errors.New("receiver too slow; connection closed")
)
