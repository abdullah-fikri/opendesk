-- Initial schema. Matches the tables created by pre-1.0 relays, so it is a no-op on existing databases.
CREATE TABLE IF NOT EXISTS hosts_registry (
    clean_id    VARCHAR(32) PRIMARY KEY,
    desk_id     VARCHAR(32) NOT NULL,
    device_name VARCHAR(255) NOT NULL,
    platform    VARCHAR(64) NOT NULL,
    is_admin    BOOLEAN NOT NULL DEFAULT FALSE,
    online      BOOLEAN NOT NULL DEFAULT FALSE,
    remote_addr VARCHAR(128) NOT NULL DEFAULT '',
    last_seen   VARCHAR(64) NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_hosts_online ON hosts_registry(online DESC, device_name ASC);

CREATE TABLE IF NOT EXISTS remote_sessions (
    id              BIGSERIAL PRIMARY KEY,
    viewer_clean_id VARCHAR(32) NOT NULL,
    viewer_desk_id  VARCHAR(32) NOT NULL,
    host_clean_id   VARCHAR(32) NOT NULL,
    host_desk_id    VARCHAR(32) NOT NULL,
    status          VARCHAR(32) NOT NULL DEFAULT 'active',
    started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at        TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_sessions_active_viewer ON remote_sessions(viewer_clean_id, status);
CREATE INDEX IF NOT EXISTS idx_sessions_active_host ON remote_sessions(host_clean_id, status);
