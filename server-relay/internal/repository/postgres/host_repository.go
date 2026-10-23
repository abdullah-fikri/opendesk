package postgres

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"opendesk/server-relay/internal/model"
)

// HostRepository stores the device registry in hosts_registry.
type HostRepository struct {
	db *sql.DB
}

// NewHostRepository returns a repository backed by db.
func NewHostRepository(db *sql.DB) *HostRepository {
	return &HostRepository{db: db}
}

// Upsert inserts or updates a host. Empty name, platform or address keep the stored value.
func (r *HostRepository) Upsert(ctx context.Context, h model.Host) error {
	const query = `
	INSERT INTO hosts_registry (clean_id, desk_id, device_name, platform, is_admin, online, remote_addr, last_seen, updated_at)
	VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
	ON CONFLICT (clean_id) DO UPDATE SET
		desk_id     = EXCLUDED.desk_id,
		device_name = CASE WHEN EXCLUDED.device_name <> '' THEN EXCLUDED.device_name ELSE hosts_registry.device_name END,
		platform    = CASE WHEN EXCLUDED.platform <> '' THEN EXCLUDED.platform ELSE hosts_registry.platform END,
		is_admin    = EXCLUDED.is_admin,
		online      = EXCLUDED.online,
		remote_addr = CASE WHEN EXCLUDED.remote_addr <> '' THEN EXCLUDED.remote_addr ELSE hosts_registry.remote_addr END,
		last_seen   = EXCLUDED.last_seen,
		updated_at  = NOW()`
	if _, err := r.db.ExecContext(ctx, query, h.CleanID, h.DeskID, h.DeviceName, h.Platform, h.IsAdmin, h.Online, h.RemoteAddr, h.LastSeen); err != nil {
		return fmt.Errorf("upsert host %s: %w", h.CleanID, err)
	}
	return nil
}

// MarkOffline flags one host offline as of at.
func (r *HostRepository) MarkOffline(ctx context.Context, cleanID string, at time.Time) error {
	const query = `UPDATE hosts_registry SET online = FALSE, last_seen = $2, updated_at = NOW() WHERE clean_id = $1`
	if _, err := r.db.ExecContext(ctx, query, cleanID, at); err != nil {
		return fmt.Errorf("mark host %s offline: %w", cleanID, err)
	}
	return nil
}

// MarkAllOffline flags every online host offline; used at startup to clear state from a previous run.
func (r *HostRepository) MarkAllOffline(ctx context.Context, at time.Time) error {
	const query = `UPDATE hosts_registry SET online = FALSE, last_seen = $1, updated_at = NOW() WHERE online = TRUE`
	if _, err := r.db.ExecContext(ctx, query, at); err != nil {
		return fmt.Errorf("mark all hosts offline: %w", err)
	}
	return nil
}

// List returns every host, online first, then by device name.
func (r *HostRepository) List(ctx context.Context) ([]model.Host, error) {
	const query = `
	SELECT clean_id, desk_id, device_name, platform, is_admin, online, remote_addr, last_seen
	FROM hosts_registry
	ORDER BY online DESC, device_name ASC`
	rows, err := r.db.QueryContext(ctx, query)
	if err != nil {
		return nil, fmt.Errorf("list hosts: %w", err)
	}
	defer rows.Close()

	hosts := make([]model.Host, 0)
	for rows.Next() {
		var h model.Host
		if err := rows.Scan(&h.CleanID, &h.DeskID, &h.DeviceName, &h.Platform, &h.IsAdmin, &h.Online, &h.RemoteAddr, &h.LastSeen); err != nil {
			return nil, fmt.Errorf("scan host: %w", err)
		}
		hosts = append(hosts, h)
	}
	return hosts, rows.Err()
}
