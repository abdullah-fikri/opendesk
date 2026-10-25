package model

// SessionStatus is the lifecycle state of a remote session as recorded in storage.
type SessionStatus string

const (
	SessionPending  SessionStatus = "pending"  // waiting for an Android host to approve
	SessionActive   SessionStatus = "active"   // traffic may flow between the pair
	SessionClosed   SessionStatus = "closed"   // ended by either side or by a disconnect
	SessionRejected SessionStatus = "rejected" // the host declined the request
)

// Role is the side a peer plays in a session.
type Role string

const (
	RoleViewer Role = "viewer"
	RoleHost   Role = "host"
)

// SessionRecord is one viewer-to-host session as persisted for auditing.
type SessionRecord struct {
	ViewerCleanID string
	ViewerDeskID  string
	HostCleanID   string
	HostDeskID    string
	Status        SessionStatus
}

// MaxViewSessions is how many hosts a multi-session viewer may hold at once (see docs/features/multi-session.md at the repository root).
const MaxViewSessions = 4
