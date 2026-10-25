package postgres

import (
	"context"
	"database/sql"
	"fmt"
	"strconv"
	"time"

	"opendesk/server-relay/internal/model"
)

// settingEnforced is the relay_settings key of the allowlist switch.
const settingEnforced = "device_allowlist_enforced"

// SecurityRepository stores the device allowlist in allowed_devices and its switch in relay_settings.
type SecurityRepository struct {
	db *sql.DB
}

// NewSecurityRepository returns a repository backed by db.
func NewSecurityRepository(db *sql.DB) *SecurityRepository {
	return &SecurityRepository{db: db}
}

// LoadSecurity reads the switch (off when never set) and every allowlist entry.
func (r *SecurityRepository) LoadSecurity(ctx context.Context) (model.SecurityState, error) {
	var state model.SecurityState
	var raw string
	err := r.db.QueryRowContext(ctx, `SELECT value FROM relay_settings WHERE key = $1`, settingEnforced).Scan(&raw)
	switch {
	case err == nil:
		state.Enforced, _ = strconv.ParseBool(raw)
	case err != sql.ErrNoRows:
		return state, fmt.Errorf("load allowlist switch: %w", err)
	}

	rows, err := r.db.QueryContext(ctx, `
	SELECT clean_id, is_admin, token_hash, added_by, created_at, enrolled_at
	FROM allowed_devices ORDER BY created_at ASC`)
	if err != nil {
		return state, fmt.Errorf("load allowlist: %w", err)
	}
	defer rows.Close()
	for rows.Next() {
		var d model.AllowedDevice
		var enrolled sql.NullTime
		if err := rows.Scan(&d.CleanID, &d.Admin, &d.TokenHash, &d.AddedBy, &d.CreatedAt, &enrolled); err != nil {
			return state, fmt.Errorf("scan allowed device: %w", err)
		}
		d.EnrolledAt = enrolled.Time
		state.Devices = append(state.Devices, d)
	}
	return state, rows.Err()
}

// SetEnforced turns the allowlist on or off.
func (r *SecurityRepository) SetEnforced(ctx context.Context, enforced bool) error {
	const query = `
	INSERT INTO relay_settings (key, value, updated_at) VALUES ($1, $2, NOW())
	ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()`
	if _, err := r.db.ExecContext(ctx, query, settingEnforced, strconv.FormatBool(enforced)); err != nil {
		return fmt.Errorf("set allowlist switch: %w", err)
	}
	return nil
}

// PutAllowedDevice inserts or replaces one allowlist entry.
func (r *SecurityRepository) PutAllowedDevice(ctx context.Context, d model.AllowedDevice) error {
	const query = `
	INSERT INTO allowed_devices (clean_id, is_admin, token_hash, added_by, created_at, enrolled_at)
	VALUES ($1, $2, $3, $4, $5, $6)
	ON CONFLICT (clean_id) DO UPDATE SET
		is_admin    = EXCLUDED.is_admin,
		token_hash  = EXCLUDED.token_hash,
		enrolled_at = EXCLUDED.enrolled_at`
	var enrolled *time.Time
	if !d.EnrolledAt.IsZero() {
		enrolled = &d.EnrolledAt
	}
	if _, err := r.db.ExecContext(ctx, query, d.CleanID, d.Admin, d.TokenHash, d.AddedBy, d.CreatedAt, enrolled); err != nil {
		return fmt.Errorf("store allowed device %s: %w", d.CleanID, err)
	}
	return nil
}

// DeleteAllowedDevice removes one allowlist entry.
func (r *SecurityRepository) DeleteAllowedDevice(ctx context.Context, cleanID string) error {
	if _, err := r.db.ExecContext(ctx, `DELETE FROM allowed_devices WHERE clean_id = $1`, cleanID); err != nil {
		return fmt.Errorf("delete allowed device %s: %w", cleanID, err)
	}
	return nil
}
