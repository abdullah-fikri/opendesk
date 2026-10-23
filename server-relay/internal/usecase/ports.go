// Package usecase implements the relay's business rules: device registration, the session state machine
// (single and multi-session), file-transfer grants and authorized message routing. It depends only on the
// interfaces declared in this file; transport and storage are injected by the composition root.
package usecase

import (
	"context"
	"time"

	"opendesk/server-relay/internal/model"
)

// Conn is the outbound side of one client connection.
// Peer serializes WriteText/WriteBinary; Ping and Close may be called concurrently with them.
type Conn interface {
	// WriteText sends one text message.
	WriteText(data []byte) error
	// WriteBinary sends parts as one binary message without first joining them.
	WriteBinary(parts ...[]byte) error
	// Ping sends a keep-alive control frame.
	Ping() error
	// Close tears the connection down, which ends its read loop.
	Close() error
}

// HostRepository persists the device registry shown to administrators.
type HostRepository interface {
	Upsert(ctx context.Context, host model.Host) error
	MarkOffline(ctx context.Context, cleanID string, at time.Time) error
	MarkAllOffline(ctx context.Context, at time.Time) error
	List(ctx context.Context) ([]model.Host, error)
}

// SessionRepository records session history.
type SessionRepository interface {
	// Start closes stale open rows and inserts a new one. keepViewerSessions limits the cleanup to rows
	// involving the host, so a multi-session viewer's other sessions stay open.
	Start(ctx context.Context, record model.SessionRecord, keepViewerSessions bool) error
	UpdateStatus(ctx context.Context, viewerCleanID, hostCleanID string, status model.SessionStatus) error
	CloseForPeer(ctx context.Context, cleanID string) error
	CloseAllOpen(ctx context.Context) error
}

// FrameMetrics receives the counters the usecase layer produces.
type FrameMetrics interface {
	Egress(size int)
	DroppedFrame()
	Rejected()
	ObservePeers(n int)
	ObserveHostList(d time.Duration)
}
