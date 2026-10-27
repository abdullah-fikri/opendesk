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
	// ErrPeerClosed means a message was addressed to a connection whose writer has stopped.
	ErrPeerClosed = errors.New("peer connection closed")
	// ErrSlowReceiver means the receiver fell too far behind and was closed.
	ErrSlowReceiver = errors.New("receiver too slow; connection closed")
)
