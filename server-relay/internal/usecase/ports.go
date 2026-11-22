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
// Only the peer's writer goroutine calls WriteText/WriteBinary; Ping and Close may be called concurrently with them.
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
	// SetAlias stores the administrator-given name of a device; an empty alias clears it.
	SetAlias(ctx context.Context, cleanID, alias string) error
	// DeleteOffline removes the rows of those cleanIDs whose device is offline (with their group memberships)
	// and returns how many it removed. A device that registers again simply gets a new row.
	DeleteOffline(ctx context.Context, cleanIDs []string) (int, error)
}

// CommunityRepository persists communities, their groups and group memberships, shared by every administrator.
type CommunityRepository interface {
	// ListCommunities returns every community with its groups and members, in creation order.
	ListCommunities(ctx context.Context) ([]model.Community, error)
	CreateCommunity(ctx context.Context, c model.Community) error
	UpdateCommunity(ctx context.Context, id, name, icon, color string) error
	// DeleteCommunity also removes its groups and their memberships.
	DeleteCommunity(ctx context.Context, id string) error
	CreateGroup(ctx context.Context, g model.Group) error
	RenameGroup(ctx context.Context, id, name string) error
	// DeleteGroup removes the group and its memberships; the devices stay registered.
	DeleteGroup(ctx context.Context, id string) error
	// AddMembers adds the registered devices among cleanIDs; unknown IDs and existing members are skipped.
	AddMembers(ctx context.Context, groupID string, cleanIDs []string) error
	RemoveMembers(ctx context.Context, groupID string, cleanIDs []string) error
}

// SecurityRepository persists the device allowlist and whether it is enforced.
type SecurityRepository interface {
	LoadSecurity(ctx context.Context) (model.SecurityState, error)
	SetEnforced(ctx context.Context, enforced bool) error
	// PutAllowedDevice inserts or replaces one allowlist entry.
	PutAllowedDevice(ctx context.Context, device model.AllowedDevice) error
	DeleteAllowedDevice(ctx context.Context, cleanID string) error
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
	DroppedAudio()
	SlowReceiver()
	Rejected()
	ObservePeers(n int)
	ObserveHostList(d time.Duration)
}
