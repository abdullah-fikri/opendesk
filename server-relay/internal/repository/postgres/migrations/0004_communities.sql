-- Communities (administrator-made collections of devices) and their groups, shared by every administrator
-- (see docs/features/communities.md at the repository root). IDs are random hex strings chosen by the relay.
-- A device may belong to several groups; memberships go away with their group, their community or the device's
-- registry row (an administrator deleting an offline device), so no dangling member is ever listed.
CREATE TABLE IF NOT EXISTS communities (
    id         VARCHAR(32) PRIMARY KEY,
    name       VARCHAR(64) NOT NULL,
    icon       VARCHAR(32) NOT NULL DEFAULT '',
    color      VARCHAR(16) NOT NULL DEFAULT '',
    created_by VARCHAR(32) NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS community_groups (
    id           VARCHAR(32) PRIMARY KEY,
    community_id VARCHAR(32) NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
    name         VARCHAR(64) NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_community_groups_community ON community_groups(community_id, created_at);

CREATE TABLE IF NOT EXISTS group_members (
    group_id      VARCHAR(32) NOT NULL REFERENCES community_groups(id) ON DELETE CASCADE,
    host_clean_id VARCHAR(32) NOT NULL REFERENCES hosts_registry(clean_id) ON DELETE CASCADE,
    added_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (group_id, host_clean_id)
);

CREATE INDEX IF NOT EXISTS idx_group_members_host ON group_members(host_clean_id);
