package postgres

import (
	"context"
	"database/sql"
	"fmt"

	"opendesk/server-relay/internal/model"
)

// SessionRepository stores session history in remote_sessions.
type SessionRepository struct {
	db *sql.DB
}

// NewSessionRepository returns a repository backed by db.
func NewSessionRepository(db *sql.DB) *SessionRepository {
	return &SessionRepository{db: db}
}

// Start closes stale open rows and inserts a new session row.
func (r *SessionRepository) Start(ctx context.Context, s model.SessionRecord, keepViewerSessions bool) error {
	// A multi-session viewer keeps its other open sessions; only rows involving the host are stale.
	// Otherwise every open row of either peer is stale.
	staleQuery := `
	UPDATE remote_sessions SET status = 'closed', ended_at = NOW()
	WHERE status IN ('active', 'pending') AND (viewer_clean_id IN ($1, $2) OR host_clean_id IN ($1, $2))`
	if keepViewerSessions {
		staleQuery = `
		UPDATE remote_sessions SET status = 'closed', ended_at = NOW()
		WHERE status IN ('active', 'pending') AND (viewer_clean_id = $2 OR host_clean_id = $2 OR (viewer_clean_id = $1 AND host_clean_id = $2))`
	}
	if _, err := r.db.ExecContext(ctx, staleQuery, s.ViewerCleanID, s.HostCleanID); err != nil {
		return fmt.Errorf("close stale sessions: %w", err)
	}

	const insert = `
	INSERT INTO remote_sessions (viewer_clean_id, viewer_desk_id, host_clean_id, host_desk_id, status, started_at)
	VALUES ($1, $2, $3, $4, $5, NOW())`
	if _, err := r.db.ExecContext(ctx, insert, s.ViewerCleanID, s.ViewerDeskID, s.HostCleanID, s.HostDeskID, string(s.Status)); err != nil {
		return fmt.Errorf("insert session %s -> %s: %w", s.ViewerCleanID, s.HostCleanID, err)
	}
	return nil
}

// UpdateStatus moves the open session between viewer and host to status. Terminal states also stamp ended_at;
// activation only applies to a pending row.
func (r *SessionRepository) UpdateStatus(ctx context.Context, viewerCleanID, hostCleanID string, status model.SessionStatus) error {
	query := `UPDATE remote_sessions SET status = $3 WHERE viewer_clean_id = $1 AND host_clean_id = $2 AND status = 'pending'`
	if status == model.SessionClosed || status == model.SessionRejected {
		query = `
		UPDATE remote_sessions SET status = $3, ended_at = NOW()
		WHERE viewer_clean_id = $1 AND host_clean_id = $2 AND status IN ('active', 'pending')`
	}
	if _, err := r.db.ExecContext(ctx, query, viewerCleanID, hostCleanID, string(status)); err != nil {
		return fmt.Errorf("update session %s -> %s: %w", viewerCleanID, hostCleanID, err)
	}
	return nil
}

// CloseForPeer closes every open session the peer takes part in.
func (r *SessionRepository) CloseForPeer(ctx context.Context, cleanID string) error {
	const query = `
	UPDATE remote_sessions SET status = 'closed', ended_at = NOW()
	WHERE (viewer_clean_id = $1 OR host_clean_id = $1) AND status IN ('active', 'pending')`
	if _, err := r.db.ExecContext(ctx, query, cleanID); err != nil {
		return fmt.Errorf("close sessions of %s: %w", cleanID, err)
	}
	return nil
}

// CloseAllOpen closes every open session; used at startup to clear state from a previous run.
func (r *SessionRepository) CloseAllOpen(ctx context.Context) error {
	const query = `UPDATE remote_sessions SET status = 'closed', ended_at = NOW() WHERE status IN ('active', 'pending')`
	if _, err := r.db.ExecContext(ctx, query); err != nil {
		return fmt.Errorf("close open sessions: %w", err)
	}
	return nil
}
