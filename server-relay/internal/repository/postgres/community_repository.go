package postgres

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/lib/pq"

	"opendesk/server-relay/internal/model"
)

// CommunityRepository stores communities, their groups and group memberships (migration 0004).
type CommunityRepository struct {
	db *sql.DB
}

// NewCommunityRepository returns a repository backed by db.
func NewCommunityRepository(db *sql.DB) *CommunityRepository {
	return &CommunityRepository{db: db}
}

// ListCommunities returns every community with its groups and their members, each in creation order. Three
// small queries instead of one join keep empty communities and groups without NULL handling.
func (r *CommunityRepository) ListCommunities(ctx context.Context) ([]model.Community, error) {
	rows, err := r.db.QueryContext(ctx, `SELECT id, name, icon, color, created_by, created_at FROM communities ORDER BY created_at, id`)
	if err != nil {
		return nil, fmt.Errorf("list communities: %w", err)
	}
	out := make([]model.Community, 0)
	index := map[string]int{}
	for rows.Next() {
		var c model.Community
		if err := rows.Scan(&c.ID, &c.Name, &c.Icon, &c.Color, &c.CreatedBy, &c.CreatedAt); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan community: %w", err)
		}
		index[c.ID] = len(out)
		out = append(out, c)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return nil, err
	}

	rows, err = r.db.QueryContext(ctx, `SELECT id, community_id, name, created_at FROM community_groups ORDER BY created_at, id`)
	if err != nil {
		return nil, fmt.Errorf("list groups: %w", err)
	}
	type at struct{ c, g int }
	groups := map[string]at{}
	for rows.Next() {
		var g model.Group
		if err := rows.Scan(&g.ID, &g.CommunityID, &g.Name, &g.CreatedAt); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan group: %w", err)
		}
		ci, ok := index[g.CommunityID]
		if !ok {
			continue
		}
		g.Members = []string{}
		groups[g.ID] = at{ci, len(out[ci].Groups)}
		out[ci].Groups = append(out[ci].Groups, g)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return nil, err
	}

	rows, err = r.db.QueryContext(ctx, `SELECT group_id, host_clean_id FROM group_members ORDER BY host_clean_id`)
	if err != nil {
		return nil, fmt.Errorf("list group members: %w", err)
	}
	defer rows.Close()
	for rows.Next() {
		var gid, clean string
		if err := rows.Scan(&gid, &clean); err != nil {
			return nil, fmt.Errorf("scan group member: %w", err)
		}
		if p, ok := groups[gid]; ok {
			g := &out[p.c].Groups[p.g]
			g.Members = append(g.Members, clean)
		}
	}
	return out, rows.Err()
}

// CreateCommunity inserts c (without groups).
func (r *CommunityRepository) CreateCommunity(ctx context.Context, c model.Community) error {
	const query = `INSERT INTO communities (id, name, icon, color, created_by, created_at) VALUES ($1, $2, $3, $4, $5, $6)`
	if _, err := r.db.ExecContext(ctx, query, c.ID, c.Name, c.Icon, c.Color, c.CreatedBy, c.CreatedAt); err != nil {
		return fmt.Errorf("create community: %w", err)
	}
	return nil
}

// UpdateCommunity replaces the name, icon and color of community id.
func (r *CommunityRepository) UpdateCommunity(ctx context.Context, id, name, icon, color string) error {
	const query = `UPDATE communities SET name = $2, icon = $3, color = $4, updated_at = NOW() WHERE id = $1`
	if _, err := r.db.ExecContext(ctx, query, id, name, icon, color); err != nil {
		return fmt.Errorf("update community %s: %w", id, err)
	}
	return nil
}

// DeleteCommunity removes community id; its groups and their memberships cascade.
func (r *CommunityRepository) DeleteCommunity(ctx context.Context, id string) error {
	if _, err := r.db.ExecContext(ctx, `DELETE FROM communities WHERE id = $1`, id); err != nil {
		return fmt.Errorf("delete community %s: %w", id, err)
	}
	return nil
}

// CreateGroup inserts g (without members).
func (r *CommunityRepository) CreateGroup(ctx context.Context, g model.Group) error {
	const query = `INSERT INTO community_groups (id, community_id, name, created_at) VALUES ($1, $2, $3, $4)`
	if _, err := r.db.ExecContext(ctx, query, g.ID, g.CommunityID, g.Name, g.CreatedAt); err != nil {
		return fmt.Errorf("create group: %w", err)
	}
	return nil
}

// RenameGroup names group id.
func (r *CommunityRepository) RenameGroup(ctx context.Context, id, name string) error {
	if _, err := r.db.ExecContext(ctx, `UPDATE community_groups SET name = $2 WHERE id = $1`, id, name); err != nil {
		return fmt.Errorf("rename group %s: %w", id, err)
	}
	return nil
}

// DeleteGroup removes group id and its memberships; the devices stay registered.
func (r *CommunityRepository) DeleteGroup(ctx context.Context, id string) error {
	if _, err := r.db.ExecContext(ctx, `DELETE FROM community_groups WHERE id = $1`, id); err != nil {
		return fmt.Errorf("delete group %s: %w", id, err)
	}
	return nil
}

// AddMembers puts the registered devices among cleanIDs into group id. IDs the registry does not know are
// skipped (the foreign key would refuse them), as are existing memberships.
func (r *CommunityRepository) AddMembers(ctx context.Context, id string, cleanIDs []string) error {
	const query = `
	INSERT INTO group_members (group_id, host_clean_id)
	SELECT $1, clean_id FROM hosts_registry WHERE clean_id = ANY($2)
	ON CONFLICT DO NOTHING`
	if _, err := r.db.ExecContext(ctx, query, id, pq.Array(cleanIDs)); err != nil {
		return fmt.Errorf("add members to group %s: %w", id, err)
	}
	return nil
}

// RemoveMembers takes cleanIDs out of group id.
func (r *CommunityRepository) RemoveMembers(ctx context.Context, id string, cleanIDs []string) error {
	const query = `DELETE FROM group_members WHERE group_id = $1 AND host_clean_id = ANY($2)`
	if _, err := r.db.ExecContext(ctx, query, id, pq.Array(cleanIDs)); err != nil {
		return fmt.Errorf("remove members from group %s: %w", id, err)
	}
	return nil
}
